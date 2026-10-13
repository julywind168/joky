use super::*;
use crate::vm::VmMode;
const OLD: &str = r#"import joky/domain
struct Room { score:Int32 }
fn make(score:Int32)->Room { Room{score:score} }
fn read(room:&Room)->Int32 {room.score}
fn add(room:&mut Room,value:Int32)->Int32 {room.score=room.score+value room.score}
fn version()->Int32 {1}
fn wire() @suspends { let room=domain.create(make,10) domain.read_from(room.read_handle(),read); domain.write_to(room.handle(),add,1); }
"#;
const NEW: &str = r#"import joky/domain
struct Room { score:Int64, level:Int32 }
fn upgrade(old:&PreviousState)->Room {Room{score:int64(old.score),level:1}}
fn read(room:&Room)->Int64 {room.score}
fn level(room:&Room)->Int32 {room.level}
fn version()->Int32 {2}
fn wire(room:domain.ReadHandle<Room>) @suspends { domain.read_from(room,read); domain.read_from(room,level); }
"#;
fn setup(mode: VmMode) -> (VmEngine, DomainOwner, Vec<Rc<dyn Any>>) {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(OLD).unwrap();
    let domain = vm
        .create_domain("make", &[InstanceValue::Int32(42)])
        .unwrap();
    let owners = vec![vm.script_code_owner().unwrap()];
    (vm, domain, owners)
}
fn prepare_new(
    vm: &VmEngine,
    owners: &[Rc<dyn Any>],
    source: &str,
) -> Result<PreparedMigration, Error> {
    prepare(
        vm,
        &vm.domains,
        owners,
        schema(&vm.domains, owners)?,
        source,
        "upgrade",
    )
}
fn read(vm: &VmEngine, domain: &DomainOwner, name: &str) -> InstanceValue {
    let mut request = submit(
        &vm.domains,
        domain.read_handle(),
        name,
        &[],
        Default::default(),
    )
    .unwrap();
    vm.poll_tasks(10).unwrap();
    match request.take_result().unwrap() {
        DomainOutcome::Value(value) => value,
        other => panic!("{other:?}"),
    }
}
#[test]
fn migration_scalar_staging_preserves_identity_and_switches_code_with_state() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, domain, mut owners) = setup(mode);
        let registry = vm.domains.clone();
        let identity = domain.write_handle();
        let before = schema(&registry, &owners).unwrap();
        let mut queued = submit(
            &registry,
            domain.read_handle(),
            "read",
            &[],
            Default::default(),
        )
        .unwrap();
        let plan = prepare_new(&vm, &owners, NEW).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("version").unwrap(), 1);
        assert_eq!(schema(&registry, &owners).unwrap(), before);
        publish(&mut vm, &registry, &owners, plan).unwrap();
        owners.push(vm.script_code_owner().unwrap());
        assert_eq!(vm.call_int32_0("version").unwrap(), 2);
        assert_eq!(domain.write_handle(), identity);
        assert_ne!(schema(&registry, &owners).unwrap(), before);
        vm.poll_tasks(10).unwrap();
        assert!(matches!(
            queued.take_result(),
            Some(DomainOutcome::Failed(_))
        ));
        assert_eq!(read(&vm, &domain, "read"), InstanceValue::Int64(42));
        assert_eq!(read(&vm, &domain, "level"), InstanceValue::Int32(1));
        assert_eq!(vm.domain_revision(domain.read_handle()).unwrap(), 1);
        domain.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn migration_conflicts_failures_cancellation_and_schema_identity_preserve_old_root() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, domain, owners) = setup(mode);
        let registry = vm.domains.clone();
        let before = registry.borrow().usage();
        assert!(prepare(&vm, &registry, &owners, 0, NEW, "upgrade").is_err());
        for source in [
            NEW.replace("int64(old.score)","int64(old.missing)"),
            NEW.replace("fn upgrade(old:&PreviousState)->Room {", "fn upgrade(old:&PreviousState)->Room {let message=\"failed migration\" check.require(false,&message) ").replace("import joky/domain","import joky/domain import joky/check"),
            format!("{NEW}\nstruct PreviousState {{score:Int32}}"),
            NEW.replace("old:&PreviousState","old:&mut PreviousState"),
        ] {
            assert!(prepare_new(&vm,&owners,&source).is_err(),"{source}");
            assert_eq!(registry.borrow().usage(),before);
            assert_eq!(vm.call_int32_0("version").unwrap(),1);
            assert_eq!(read(&vm,&domain,"read"),InstanceValue::Int32(42));
        }
        drop(prepare_new(&vm, &owners, NEW).unwrap());
        assert_eq!(registry.borrow().usage(), before);
        let plan = prepare_new(&vm, &owners, NEW).unwrap();
        let mut write = submit(
            &registry,
            domain.write_handle(),
            "add",
            &[InstanceValue::Int32(3)],
            Default::default(),
        )
        .unwrap();
        vm.poll_tasks(10).unwrap();
        write.take_result().unwrap();
        assert!(publish(&mut vm, &registry, &owners, plan).is_err());
        assert_eq!(read(&vm, &domain, "read"), InstanceValue::Int32(45));
        assert_eq!(vm.call_int32_0("version").unwrap(), 1);
        domain.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn migration_publication_revalidates_selection_schema_and_version_capacity() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for boundary in 0..6 {
            let (mut vm, domain, owners) = setup(mode);
            let registry = vm.domains.clone();
            if boundary == 4 {
                registry
                    .borrow_mut()
                    .entries
                    .get_mut(&domain.handle.sequence)
                    .unwrap()
                    .revision = u64::MAX;
            }
            if boundary == 5 {
                registry
                    .borrow_mut()
                    .entries
                    .get_mut(&domain.handle.sequence)
                    .unwrap()
                    .code = u64::MAX;
            }
            let plan = prepare_new(&vm, &owners, NEW).unwrap();
            let extra = if boundary == 0 {
                Some(
                    vm.create_domain("make", &[InstanceValue::Int32(8)])
                        .unwrap(),
                )
            } else {
                None
            };
            if boundary == 1 {
                domain.close();
            }
            if boundary == 2 {
                registry
                    .borrow_mut()
                    .entries
                    .get_mut(&domain.handle.sequence)
                    .unwrap()
                    .schema = source::next_token().unwrap();
            }
            if boundary == 3 {
                vm.load_source(OLD).unwrap();
            }
            assert!(
                publish(&mut vm, &registry, &owners, plan).is_err(),
                "{mode:?} boundary={boundary}"
            );
            assert_eq!(vm.call_int32_0("version").unwrap(), 1);
            if boundary != 1 {
                assert_eq!(read(&vm, &domain, "read"), InstanceValue::Int32(42));
            }
            drop(extra);
            domain.close();
            assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
#[test]
fn migration_schema_identities_do_not_recur_after_reverse_migration() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, domain, mut owners) = setup(mode);
        let registry = vm.domains.clone();
        let first = schema(&registry, &owners).unwrap();
        let plan = prepare_new(&vm, &owners, NEW).unwrap();
        publish(&mut vm, &registry, &owners, plan).unwrap();
        owners.push(vm.script_code_owner().unwrap());
        let second = schema(&registry, &owners).unwrap();
        let reverse = format!(
            "{OLD}\nfn upgrade(old:&PreviousState)->Room {{ Room{{score:int32(old.score)}} }}"
        );
        let plan = prepare_new(&vm, &owners, &reverse).unwrap();
        publish(&mut vm, &registry, &owners, plan).unwrap();
        owners.push(vm.script_code_owner().unwrap());
        let third = schema(&registry, &owners).unwrap();
        assert_ne!(first, third);
        assert_ne!(second, third);
        assert!(prepare(&vm, &registry, &owners, first, NEW, "upgrade").is_err());
        assert_eq!(read(&vm, &domain, "read"), InstanceValue::Int32(42));
        domain.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn migration_admission_rejects_unproved_entries_and_owned_schemas() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (vm, domain, owners) = setup(mode);
        for source in [
            NEW.replace(
                "fn upgrade(old:&PreviousState)->Room {",
                "extern fn effect() fn upgrade(old:&PreviousState)->Room {effect() ",
            ),
            NEW.replace(
                "fn upgrade(old:&PreviousState)->Room {",
                "fn upgrade(old:&PreviousState)->Room {old.score=0 ",
            ),
            NEW.replace(
                "fn upgrade(old:&PreviousState)->Room",
                "fn upgrade(old:&PreviousState) @suspends ->Room",
            ),
            NEW.replace("level:Int32", "level:String")
                .replace("level:1", "level:\"owned\"")
                .replace("{room.level}", "{0}"),
        ] {
            assert!(prepare_new(&vm, &owners, &source).is_err());
        }
        let wrong = VmEngine::new();
        assert!(schema(
            &vm.domains,
            &[wrong.script_code_owner().unwrap_or_else(|| Rc::new(()))]
        )
        .is_err());
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn migration_candidate_budget_failures_release_partial_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (vm, domain, owners) = setup(mode);
        let budget = vm.domains.borrow().budget.clone();
        let baseline = budget.bytes.get();
        for extra in [
            0,
            1,
            std::mem::size_of::<PreparedMigration>(),
            std::mem::size_of::<PreparedMigration>() + 8,
            4096,
        ] {
            budget.limit.set(baseline + extra);
            let plan = prepare_new(&vm, &owners, NEW);
            if extra < std::mem::size_of::<PreparedMigration>() + 8 {
                assert!(plan.is_err());
            }
            drop(plan);
            assert_eq!(budget.bytes.get(), baseline);
        }
        budget.limit.set(usize::MAX);
        assert_eq!(read(&vm, &domain, "read"), InstanceValue::Int32(42));
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn migration_invalidates_old_read_credentials_and_retires_unneeded_modules() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, domain, owners) = setup(mode);
        let registry = vm.domains.clone();
        let old = Rc::downgrade(&owners[0]);
        let reads = {
            let d = registry.borrow();
            let d = d.get(domain.read_handle()).unwrap();
            reads::ReadSet::single(domain.read_handle(), d.revision, d.code, d.schema)
        };
        let plan = prepare_new(&vm, &owners, NEW).unwrap();
        publish(&mut vm, &registry, &owners, plan).unwrap();
        assert!(matches!(
            reads.validate(&registry.borrow()),
            Err(commits::Status::Conflict)
        ));
        drop(owners);
        assert!(old.upgrade().is_none());
        domain.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
