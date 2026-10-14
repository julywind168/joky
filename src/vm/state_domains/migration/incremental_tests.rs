use super::{tests::read, *};
use crate::vm::VmMode;
const OLD: &str = include_str!("../../../../tests/fixtures/migration_incremental_old.jk");
const NEW: &str = include_str!("../../../../tests/fixtures/migration_incremental_new.jk");
fn setup(mode: VmMode) -> (VmEngine, DomainOwner, Vec<Rc<dyn Any>>) {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(OLD).unwrap();
    let owner = vm
        .create_domain("make", &[InstanceValue::Int32(42)])
        .unwrap();
    let owners = vec![vm.script_code_owner().unwrap()];
    (vm, owner, owners)
}
fn start(vm: &VmEngine, owners: &[Rc<dyn Any>], source: &str) -> Result<PreparedMigration, Error> {
    begin(
        vm,
        &vm.domains,
        owners,
        schema(&vm.domains, owners)?,
        source,
        "start",
        "advance",
    )
}
#[test]
fn migration_incremental_steps_keep_old_state_visible_until_atomic_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, owner, owners) = setup(mode);
        let registry = vm.domains.clone();
        let identity = owner.write_handle();
        let old_schema = schema(&registry, &owners).unwrap();
        let baseline = registry.borrow().usage();
        let plan = start(&vm, &owners, NEW).unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert!(publish(&mut vm, &registry, &owners, plan).is_err());
        assert_eq!(registry.borrow().usage(), baseline);
        let mut plan = start(&vm, &owners, NEW).unwrap();
        for batch in 0..6 {
            assert_eq!(
                step(&vm, &registry, &owners, &mut plan, 2).unwrap(),
                batch == 5
            );
            assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(42));
            assert_eq!(schema(&registry, &owners).unwrap(), old_schema);
            assert_eq!(vm.call_int32_0("version").unwrap(), 1);
        }
        // Ready polling does not run the callback again.
        let usage = registry.borrow().usage();
        assert!(step(&vm, &registry, &owners, &mut plan, 2).unwrap());
        assert_eq!(registry.borrow().usage(), usage);
        publish(&mut vm, &registry, &owners, plan).unwrap();
        assert_eq!(owner.write_handle(), identity);
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(43));
        assert_eq!(vm.call_int32_0("version").unwrap(), 2);
        owner.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn migration_incremental_revalidates_each_step_and_discards_partial_graphs() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for conflict in [
            "write",
            "close",
            "binding",
            "selection",
            "code",
            "schema",
            "zero",
        ] {
            let (mut vm, owner, owners) = setup(mode);
            let registry = vm.domains.clone();
            let baseline = registry.borrow().usage();
            let mut plan = start(&vm, &owners, NEW).unwrap();
            assert!(!step(&vm, &registry, &owners, &mut plan, 2).unwrap());
            let mut extra = None;
            match conflict {
                "write" => {
                    let mut request = submit(
                        &registry,
                        owner.write_handle(),
                        "add",
                        &[InstanceValue::Int32(3)],
                        Default::default(),
                    )
                    .unwrap();
                    vm.poll_tasks(10).unwrap();
                    assert!(matches!(
                        request.take_result(),
                        Some(DomainOutcome::Value(InstanceValue::Int32(45)))
                    ));
                }
                "close" => {
                    owner.close();
                }
                "binding" => {
                    vm.load_source(OLD).unwrap();
                }
                "selection" => {
                    extra = Some(
                        vm.create_domain("make", &[InstanceValue::Int32(42)])
                            .unwrap(),
                    );
                }
                "code" => {
                    registry
                        .borrow_mut()
                        .entries
                        .get_mut(&owner.write_handle().sequence)
                        .unwrap()
                        .code += 1
                }
                "schema" => {
                    registry
                        .borrow_mut()
                        .entries
                        .get_mut(&owner.write_handle().sequence)
                        .unwrap()
                        .schema += 1
                }
                "zero" => (),
                _ => unreachable!(),
            }
            assert!(
                step(
                    &vm,
                    &registry,
                    &owners,
                    &mut plan,
                    if conflict == "zero" { 0 } else { 2 }
                )
                .is_err(),
                "{conflict}"
            );
            drop(plan);
            if let Some(extra) = extra {
                extra.close();
            }
            if conflict != "close" {
                assert_eq!(registry.borrow().usage(), baseline);
                assert_eq!(
                    read(&vm, &owner, "read"),
                    InstanceValue::Int32(if conflict == "write" { 45 } else { 42 })
                );
                owner.close();
            }
            assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
#[test]
fn migration_incremental_rejects_unsafe_steps_and_reclaims_runtime_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (vm, owner, owners) = setup(mode);
        let registry = vm.domains.clone();
        let baseline = registry.borrow().usage();
        for source in [
            NEW.replace("old:&PreviousState,budget", "old:&mut PreviousState,budget"),
            NEW.replace("budget:UInt64", "budget:Int32")
                .replace("count<budget", "int32(count)<budget"),
            NEW.replace("->Bool {", "@suspends ->Bool {task.yield_now() ")
                .replace("import joky/domain", "import joky/domain import joky/task"),
            NEW.replace(
                "var count:UInt64=0",
                "var count:UInt64=uint64(vm.error_code())",
            )
            .replace("import joky/domain", "import joky/domain import joky/vm"),
        ] {
            assert!(start(&vm, &owners, &source).is_err());
            assert_eq!(registry.borrow().usage(), baseline);
        }
        let failing = NEW
            .replace("import joky/domain", "import joky/domain import joky/check")
            .replace(
                "count=count+1",
                "count=count+1 let message=\"step failure\" check.assert(false,&message)",
            );
        let mut plan = start(&vm, &owners, &failing).unwrap();
        assert!(step(&vm, &registry, &owners, &mut plan, 2).is_err());
        drop(plan);
        assert_eq!(registry.borrow().usage(), baseline);
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(42));
        owner.close();
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn migration_incremental_ready_candidate_still_validates_before_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, owner, owners) = setup(mode);
        let registry = vm.domains.clone();
        let baseline = registry.borrow().usage();
        let mut plan = start(&vm, &owners, NEW).unwrap();
        assert!(step(&vm, &registry, &owners, &mut plan, 12).unwrap());
        let mut request = submit(
            &registry,
            owner.write_handle(),
            "add",
            &[InstanceValue::Int32(3)],
            Default::default(),
        )
        .unwrap();
        vm.poll_tasks(10).unwrap();
        request.take_result().unwrap();
        assert!(step(&vm, &registry, &owners, &mut plan, 2).is_err());
        assert!(publish(&mut vm, &registry, &owners, plan).is_err());
        assert_eq!(registry.borrow().usage(), baseline);
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(45));
        owner.close();
    }
}

fn allocation_budgets(limits: impl Clone + IntoIterator<Item = usize>) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (vm, owner, owners) = setup(mode);
        let plan = start(&vm, &owners, NEW).unwrap();
        let module = plan.module.clone();
        drop(plan);
        let initial = module.instance_info("start").unwrap();
        let advance = module.instance_info("advance").unwrap();
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
        let mut failed = false;
        for extra in limits.clone() {
            budget.limit.set(usize::MAX);
            let mut root = Root::construct(
                &module,
                &initial,
                "start",
                &[pointer],
                Default::default(),
                budget.clone(),
            )
            .unwrap();
            // Keep the first batch alive while a later batch may fail after mutation.
            let mut words = [0, pointer, 2];
            assert_eq!(
                root.invoke(
                    &module,
                    &initial.signature.ret,
                    "advance",
                    &advance,
                    &mut words,
                    Default::default()
                )
                .unwrap(),
                InstanceValue::Bool(false)
            );
            budget.limit.set(budget.bytes.get() + extra);
            words[2] = 10;
            match root.invoke(
                &module,
                &initial.signature.ret,
                "advance",
                &advance,
                &mut words,
                Default::default(),
            ) {
                Ok(result) => {
                    assert_eq!(result, InstanceValue::Bool(true));
                    succeeded = true;
                }
                Err(error) => {
                    assert_eq!(
                        error.code(),
                        ErrorCode::ResourceLimit,
                        "{mode:?} {extra}: {error}"
                    );
                    failed = true;
                }
            }
            drop(root);
            assert_eq!(budget.bytes.get(), baseline, "{mode:?} {extra}");
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
            if succeeded {
                break;
            }
        }
        assert!(succeeded && failed);
        budget.limit.set(usize::MAX);
        assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(42));
        owner.close();
        assert_eq!(budget.bytes.get(), 0);
    }
}
#[test]
fn migration_incremental_allocation_failure_reclaims_previous_and_partial_batches() {
    allocation_budgets([0, 1, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384]);
}
#[test]
#[ignore = "exhaustive: every byte budget through a later migration batch"]
fn exhaustive_migration_incremental_allocation_budgets() {
    allocation_budgets(0..=16384);
}
#[test]
fn migration_incremental_interpreter_instruction_limits_reclaim_partial_batches() {
    let (mut vm, owner, owners) = setup(VmMode::Interpreter);
    let registry = vm.domains.clone();
    let baseline = registry.borrow().usage();
    let mut failed = false;
    let mut succeeded = false;
    for budget in [0, 1, 16, 64, 256, 1024, 4096, 16384, 65536] {
        vm.set_interpreter_limits(Default::default()).unwrap();
        let mut plan = start(&vm, &owners, NEW).unwrap();
        assert!(!step(&vm, &registry, &owners, &mut plan, 2).unwrap());
        vm.set_interpreter_limits(InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match step(&vm, &registry, &owners, &mut plan, 10) {
            Ok(done) => {
                assert!(done);
                succeeded = true;
            }
            Err(error) => {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                failed = true;
            }
        }
        drop(plan);
        assert_eq!(registry.borrow().usage(), baseline);
    }
    assert!(succeeded && failed);
    vm.set_interpreter_limits(Default::default()).unwrap();
    assert_eq!(read(&vm, &owner, "read"), InstanceValue::Int32(42));
    owner.close();
    assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
}
