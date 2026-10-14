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
pub(super) fn read(vm: &VmEngine, domain: &DomainOwner, name: &str) -> InstanceValue {
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
            NEW.replace("fn upgrade(old:&PreviousState)->Room {", "fn upgrade(old:&PreviousState)->Room {let message=\"failed migration\" check.assert(false,&message) ").replace("import joky/domain","import joky/domain import joky/check"),
            format!("{NEW}\nstruct PreviousState {{score:Int32}}"),
            NEW.replace("old:&PreviousState","old:&mut PreviousState"),
        ] {
            assert!(prepare_new(&vm,&owners,&source).is_err(),"{source}");
            assert_eq!(registry.borrow().usage(),before);
            assert_eq!(vm.call_int32_0("version").unwrap(),1);
            assert_eq!(read(&vm,&domain,"read"),InstanceValue::Int32(42));
        }
        // Scalar migrations do not inject unrelated container import aliases.
        let plain =
            format!("{NEW} struct PreviousArrays {{ n:Int32 }} struct PreviousMaps {{ n:Int32 }}");
        drop(prepare_new(&vm, &owners, &plain).unwrap());
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
fn migration_admission_rejects_unproved_entries_and_cleanup_hooks() {
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
            format!("{NEW}\nimpl Drop for Room {{ fn drop(&mut self) {{}} }}"),
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

const OWNED_OLD: &str = include_str!("../../../../tests/fixtures/migration_owned_old.jk");
const OWNED_NEW: &str = include_str!("../../../../tests/fixtures/migration_owned_new.jk");
#[test]
fn migration_owned_graph_preserves_nested_values_and_retires_old_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(OWNED_OLD).unwrap_or_else(|e| panic!("{e}"));
        let domain = vm
            .create_domain("make", &[InstanceValue::Int32(42)])
            .unwrap();
        let registry = vm.domains.clone();
        let owners = vec![vm.script_code_owner().unwrap()];
        let before = registry.borrow().usage();
        let plan = prepare_new(&vm, &owners, OWNED_NEW).unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert!(registry.borrow().usage().3 > before.3);
        assert_eq!(read(&vm, &domain, "read"), InstanceValue::Int32(42));
        drop(plan);
        assert_eq!(registry.borrow().usage(), before);
        let plan = prepare_new(&vm, &owners, OWNED_NEW).unwrap();
        publish(&mut vm, &registry, &owners, plan).unwrap();
        assert_eq!(read(&vm, &domain, "read"), InstanceValue::Int32(43));
        domain.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn migration_single_buffer_wrapper_uses_borrowed_owner_representation() {
    let old = r#"struct Room {text:String} fn make(n:Int32)->Room {Room{text:"旧文本"}} fn read(s:&Room)->Int32 {int32(s.text.byte_length())}"#;
    let next = r#"struct Room {text:String,level:Int32} fn upgrade(s:&PreviousState)->Room {Room{text:s.text.clone(),level:1}} fn read(s:&Room)->Int32 {int32(s.text.byte_length())+s.level}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(old).unwrap();
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(0)])
            .unwrap();
        let owners = vec![vm.script_code_owner().unwrap()];
        let plan = prepare_new(&vm, &owners, next).unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        let registry = vm.domains.clone();
        publish(&mut vm, &registry, &owners, plan).unwrap();
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(10));
        owner.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn migration_owned_failures_and_intervening_writes_preserve_the_old_graph() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(OWNED_OLD).unwrap();
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(42)])
            .unwrap();
        let owners = vec![vm.script_code_owner().unwrap()];
        let registry = vm.domains.clone();
        let before = registry.borrow().usage();
        let failing=OWNED_NEW.replace("import joky/domain","import joky/domain import joky/check").replace("    let mode=", "    let reason=\"migration failed after allocation\" check.assert(false,&reason)\n    let mode=");
        assert!(prepare_new(&vm, &owners, &failing).is_err());
        assert_eq!(registry.borrow().usage(), before);
        let plan = prepare_new(&vm, &owners, OWNED_NEW).unwrap();
        let mut change = submit(
            &registry,
            owner.write_handle(),
            "add",
            &[InstanceValue::Int32(3)],
            Default::default(),
        )
        .unwrap();
        vm.poll_tasks(10).unwrap();
        change.take_result().unwrap();
        assert!(publish(&mut vm, &registry, &owners, plan).is_err());
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(45));
        assert_eq!(registry.borrow().usage(), before);
        // Rebuild from the updated nested values, including both container roots.
        let plan = prepare_new(&vm, &owners, OWNED_NEW).unwrap();
        publish(&mut vm, &registry, &owners, plan).unwrap();
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(46));
        owner.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

fn owned_allocation_budgets(limits: impl Clone + IntoIterator<Item = usize>) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(OWNED_OLD).unwrap();
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(42)])
            .unwrap();
        let owners = vec![vm.script_code_owner().unwrap()];
        // Compile once; sweep the exact candidate construction used by prepare.
        let plan = prepare_new(&vm, &owners, OWNED_NEW).unwrap();
        let module = plan.module.clone();
        drop(plan);
        let info = module.instance_info("upgrade").unwrap();
        let (budget, pointer) = {
            let domains = vm.domains.borrow();
            let domain = domains.get(owner.read_handle()).unwrap();
            (
                domains.budget.clone(),
                domain.root.as_ref().unwrap().borrow_pointer(&domain.ty),
            )
        };
        let baseline = budget.bytes.get();
        let live = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut succeeded = false;
        for extra in limits.clone() {
            budget.limit.set(baseline + extra);
            let result = Root::construct(
                &module,
                &info,
                "upgrade",
                &[pointer],
                Default::default(),
                budget.clone(),
            );
            match result {
                Ok(root) => {
                    drop(root);
                    succeeded = true;
                }
                Err(e) => assert_eq!(e.code(), ErrorCode::ResourceLimit, "{mode:?}: {extra}: {e}"),
            }
            assert_eq!(budget.bytes.get(), baseline, "{mode:?}: {extra}");
            assert_eq!(
                crate::runtime::arena::LIVE.with(|n| n.get()),
                live,
                "{mode:?}: {extra}"
            );
            if succeeded {
                break;
            }
        }
        assert!(succeeded);
        budget.limit.set(usize::MAX);
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(42));
        owner.close();
        assert_eq!(budget.bytes.get(), 0);
    }
}
#[test]
fn migration_owned_candidate_allocation_failures_leave_old_graph_live() {
    owned_allocation_budgets([
        0, 1, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
    ]);
}
#[test]
#[ignore = "exhaustive: every byte budget through owning migration construction"]
fn exhaustive_migration_owned_allocation_budgets() {
    owned_allocation_budgets(0..=65536);
}

#[test]
fn migration_schema_change_only_behind_array_storage_is_visible() {
    let old = r#"import joky/array.{type Array}
struct Player { name:String,score:Int32 }
struct Room {players:Array<Player>}
fn make(n:Int32)->Room {var players=Array.new::<Player>() players.push(Player{name:"test",score:n}) Room{players:players}}
fn read(s:&Room)->Int32 {s.players.borrow_ref(0).score}"#;
    let next = r#"import joky/array as items.{type Array}
struct Player { name:String,score:Int64 }
struct Room {players:Array<Player>}
fn upgrade(s:&PreviousState)->Room {
    let old=s.players.borrow_ref(0)
    var players=Array.new::<Player>() players.push(Player{name:old.name.clone(),score:int64(old.score)}) Room{players:players}
}
fn read(s:&Room)->Int64 {s.players.borrow_ref(0).score}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(old).unwrap();
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(42)])
            .unwrap();
        let owners = vec![vm.script_code_owner().unwrap()];
        let plan = prepare_new(&vm, &owners, next).unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        let registry = vm.domains.clone();
        publish(&mut vm, &registry, &owners, plan).unwrap();
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int64(42));
        owner.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn migration_interpreter_budget_reclaims_partial_owned_graphs() {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(OWNED_OLD).unwrap();
    let owner = vm
        .create_domain("make", &[InstanceValue::Int32(42)])
        .unwrap();
    let owners = vec![vm.script_code_owner().unwrap()];
    let baseline = vm.domains.borrow().usage();
    let mut success = false;
    for budget in [0, 1, 16, 64, 256, 1024, 4096, 16384, 65536] {
        vm.set_interpreter_limits(InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match prepare_new(&vm, &owners, OWNED_NEW) {
            Ok(plan) => {
                drop(plan);
                success = true;
            }
            Err(e) => assert_eq!(e.code(), ErrorCode::ResourceLimit, "budget={budget}: {e}"),
        }
        assert_eq!(vm.domains.borrow().usage(), baseline);
        if success {
            break;
        }
    }
    assert!(success);
    vm.set_interpreter_limits(Default::default()).unwrap();
    assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(42));
    owner.close();
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}
