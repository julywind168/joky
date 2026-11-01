use super::*;
const DECLARED: &str = include_str!("../../../../tests/fixtures/domain_every.jk");
const AUTO: &str = r#"#state struct Room{value:Int32}
impl Room {#every(10ms) fn tick(&mut self){self.value+=1}}
fn make(value:Int32)->Room{Room{value:value}}
fn read(room:&Room)->Int32{room.value}
fn local()->Int32{var room=make(0); room.tick(); room.value}"#;
fn setup_auto(mode: VmMode, source: &str, tasks: usize) -> (VmEngine, Rc<ManualClock>) {
    let mut vm = VmEngine::with_mode(mode);
    let clock = Rc::new(ManualClock {
        time: Cell::new(Instant::now()),
        hook: RefCell::new(None),
        skip: Cell::new(0),
    });
    vm.configure_tasks(clock.clone(), tasks).unwrap();
    vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
    (vm, clock)
}
fn advance(vm: &VmEngine, clock: &ManualClock, milliseconds: u64) {
    clock
        .time
        .set(clock.time.get() + Duration::from_millis(milliseconds));
    drive(vm);
}
fn create_auto(vm: &VmEngine) -> DomainOwner {
    vm.create_domain("make", &[InstanceValue::Int32(10)])
        .unwrap()
}
#[test]
fn every_declarations_automatic_lifecycle_cadence_and_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock) = setup_auto(mode, AUTO, 64);
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.call_int32_0("local").unwrap(), 1);
        assert_eq!(vm.task_count(), 0);
        let owner = create_auto(&vm);
        assert_eq!(vm.task_count(), 1);
        assert_eq!(
            vm.domain_periodic_status(owner.read_handle(), "tick"),
            Status::Active
        );
        advance(&vm, &clock, 9);
        assert_eq!(value(&vm, &owner), 10);
        // A late poll produces one turn, never three accumulated ticks.
        advance(&vm, &clock, 26);
        assert_eq!(value(&vm, &owner), 11);
        let next = AUTO.replace("self.value+=1", "self.value+=2");
        vm.load_source(&next).unwrap();
        vm.publish_domain_code().unwrap();
        advance(&vm, &clock, 4);
        assert_eq!(value(&vm, &owner), 11);
        advance(&vm, &clock, 1);
        assert_eq!(value(&vm, &owner), 13);
        // Replacing the period cancels queued old ticks and resets the phase.
        clock.time.set(clock.time.get() + Duration::from_millis(10));
        vm.poll_tasks(1).unwrap();
        assert_eq!(vm.domains.borrow().requests.len(), 1);
        let next = next.replace("#every(10ms)", "#every(100ms)");
        vm.load_source(&next).unwrap();
        vm.publish_domain_code().unwrap();
        advance(&vm, &clock, 99);
        assert_eq!(value(&vm, &owner), 13);
        advance(&vm, &clock, 1);
        assert_eq!(value(&vm, &owner), 15);
        let next = next.replace("#every(100ms)", "");
        vm.load_source(&next).unwrap();
        vm.publish_domain_code().unwrap();
        assert_eq!(
            vm.domain_periodic_status(owner.read_handle(), "tick"),
            Status::InvalidAccess
        );
        assert_eq!(vm.task_count(), 0);
        advance(&vm, &clock, 1000);
        assert_eq!(value(&vm, &owner), 15);
        vm.load_source(AUTO).unwrap();
        vm.publish_domain_code().unwrap();
        advance(&vm, &clock, 10);
        assert_eq!(value(&vm, &owner), 16);
        let second = create_auto(&vm);
        assert_eq!(vm.task_count(), 2);
        owner.close();
        second.close();
        drive(&vm);
        assert_eq!(
            vm.domain_periodic_status(owner.read_handle(), "tick"),
            Status::Closed
        );
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn every_declarations_create_and_publication_are_atomic_under_limits() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let two = AUTO.replace(
            "fn local()",
            "impl Room{#every(10ms) fn other(&mut self){self.value+=100}} fn local()",
        );
        let (vm, _) = setup_auto(mode, &two, 1);
        assert!(vm
            .create_domain("make", &[InstanceValue::Int32(10)])
            .is_err());
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        let (mut vm, clock) = setup_auto(mode, AUTO, 4);
        let a = create_auto(&vm);
        let b = create_auto(&vm);
        let budget = vm.domains.borrow().usage();
        let code = vm.domains.borrow().entries[&a.handle.sequence].code;
        let three = two.replace(
            "fn local()",
            "impl Room{#every(10ms) fn third(&mut self){self.value+=1000}} fn local()",
        );
        vm.load_source(&three).unwrap();
        assert!(vm.publish_domain_code().is_err());
        assert_eq!(vm.domains.borrow().usage(), budget);
        assert_eq!(vm.domains.borrow().entries[&a.handle.sequence].code, code);
        assert_eq!(vm.task_count(), 2);
        advance(&vm, &clock, 10);
        assert_eq!(value(&vm, &a), 11);
        assert_eq!(value(&vm, &b), 11);
        a.close();
        b.close();
        drive(&vm);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn every_declarations_clock_reentrancy_and_failure_are_observable() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock) = setup_auto(mode, AUTO, 64);
        let owner = create_auto(&vm);
        let registry = vm.domains.clone();
        let sequence = owner.handle.sequence;
        *clock.hook.borrow_mut() = Some(Box::new(move || {
            close(&registry, sequence);
        }));
        vm.load_source(&AUTO.replace("#every(10ms)", "#every(20ms)"))
            .unwrap();
        assert!(vm.publish_domain_code().is_err());
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        let source = AUTO.replace("self.value+=1", "self.value=1/(self.value-10)");
        vm.load_source(&source).unwrap();
        let owner = create_auto(&vm);
        advance(&vm, &clock, 10);
        assert_eq!(
            vm.domain_periodic_status(owner.read_handle(), "tick"),
            Status::Failed
        );
        owner.close();
        drive(&vm);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn every_declarations_cancellation_reclaims_resources() {
    cancellation(
        DECLARED,
        [
            0, 1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        true,
    );
}
#[test]
#[ignore = "exhaustive: cancel automatic periodic ownership at every instruction"]
fn exhaustive_every_declarations_cancellation() {
    cancellation(DECLARED, 0..=32768, true);
}

#[test]
fn every_declarations_migration_starts_only_at_publication() {
    use crate::vm::state_domains::migration;
    let next = r#"#state struct Room{value:Int32,version:Int32}
impl Room{#every(20ms) fn tick(&mut self){self.value+=10}}
fn upgrade(old:&PreviousState)->Room{Room{value:old.value,version:2}}
fn read(room:&Room)->Int32{room.value}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock) = setup_auto(mode, AUTO, 64);
        let owner = create_auto(&vm);
        let owners = vec![vm.script_code_owner().unwrap()];
        let registry = vm.domains.clone();
        let schema = migration::schema(&registry, &owners).unwrap();
        let plan = migration::prepare(&vm, &registry, &owners, schema, next, "upgrade").unwrap();
        assert_eq!(vm.task_count(), 1);
        drop(plan);
        assert_eq!(vm.task_count(), 1);
        let plan = migration::prepare(&vm, &registry, &owners, schema, next, "upgrade").unwrap();
        migration::publish(&mut vm, &registry, &owners, plan).unwrap();
        assert_eq!(vm.task_count(), 1);
        advance(&vm, &clock, 19);
        assert_eq!(value(&vm, &owner), 10);
        advance(&vm, &clock, 1);
        assert_eq!(value(&vm, &owner), 20);
        owner.close();
        drive(&vm);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn every_declarations_shared_budget_failure_refunds_partial_registration() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, _) = setup_auto(mode, AUTO, 64);
        let mut succeeded = false;
        let mut failures = 0;
        for bytes in (64..=16384).step_by(127) {
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: bytes,
                ..Default::default()
            })
            .unwrap();
            match vm.create_domain("make", &[InstanceValue::Int32(10)]) {
                Ok(owner) => {
                    owner.close();
                    succeeded = true;
                }
                Err(e) => {
                    assert_eq!(e.code(), ErrorCode::ResourceLimit);
                    failures += 1;
                }
            }
            drive(&vm);
            assert_eq!(vm.task_count(), 0);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            if succeeded {
                break;
            }
        }
        assert!(succeeded && failures > 2);
    }
}

#[test]
fn every_declarations_batch_migration_prepares_all_schedules_atomically() {
    use crate::vm::state_domains::migration::batch;
    let next = r#"#state struct Room{value:Int32,version:Int32}
impl Room{#every(20ms) fn tick(&mut self){self.value+=10}}
fn upgrade(old:&PreviousRoom)->Room{Room{value:old.value,version:2}}
fn read(room:&Room)->Int32{room.value}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for limit in [3, 8] {
            let (mut vm, clock) = setup_auto(mode, AUTO, limit);
            let a = create_auto(&vm);
            let b = create_auto(&vm);
            let owners = vec![vm.script_code_owner().unwrap()];
            let registry = vm.domains.clone();
            let baseline = registry.borrow().usage();
            let mut plan = batch::begin(&vm, &registry, &owners, next).unwrap();
            batch::stage(&vm, &registry, &owners, &mut plan, "upgrade", "").unwrap();
            assert_eq!(vm.task_count(), 2);
            let result = batch::publish(&mut vm, &registry, &owners, plan);
            if limit == 3 {
                assert_eq!(result.unwrap_err().code(), ErrorCode::ResourceLimit);
                assert_eq!(registry.borrow().usage(), baseline);
                assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
                assert_eq!(vm.domain_revision(b.read_handle()).unwrap(), 0);
                assert_eq!(value(&vm, &a), 10);
                assert_eq!(value(&vm, &b), 10);
            } else {
                result.unwrap();
                assert_eq!(vm.task_count(), 2);
                advance(&vm, &clock, 20);
                assert_eq!(value(&vm, &a), 20);
                assert_eq!(value(&vm, &b), 20);
            }
            a.close();
            b.close();
            drive(&vm);
            assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
#[test]
fn every_declarations_tick_resource_failure_is_visible_and_not_restarted() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock) = setup_auto(mode, AUTO, 64);
        let owner = create_auto(&vm);
        vm.set_domain_limits(DomainLimits {
            max_request_bytes: 1,
            ..Default::default()
        })
        .unwrap();
        advance(&vm, &clock, 10);
        assert_eq!(
            vm.domain_periodic_status(owner.read_handle(), "tick"),
            Status::ResourceLimit
        );
        vm.set_domain_limits(Default::default()).unwrap();
        vm.load_source(AUTO).unwrap();
        vm.publish_domain_code().unwrap();
        assert_eq!(
            vm.domain_periodic_status(owner.read_handle(), "tick"),
            Status::ResourceLimit
        );
        advance(&vm, &clock, 100);
        assert_eq!(value(&vm, &owner), 10);
        owner.close();
        drive(&vm);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn every_declarations_prepared_tasks_cannot_run_before_atomic_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock) = setup_auto(mode, &AUTO.replace("#every(10ms)", ""), 64);
        let owner = create_auto(&vm);
        vm.load_source(AUTO).unwrap();
        let registry = vm.domains.clone();
        let ty = registry.borrow().get(owner.handle).unwrap().ty.clone();
        let module = vm.current.as_ref().unwrap();
        let plan = automatic::prepare(&registry, owner.handle, module, &ty, false).unwrap();
        advance(&vm, &clock, 100);
        assert_eq!(value(&vm, &owner), 10);
        assert_eq!(vm.domain_revision(owner.handle).unwrap(), 0);
        drop(plan);
        assert_eq!(vm.task_count(), 0);
        let mut plan = automatic::prepare(&registry, owner.handle, module, &ty, false).unwrap();
        advance(&vm, &clock, 100);
        assert_eq!(value(&vm, &owner), 10);
        {
            let mut domains = registry.borrow_mut();
            let domain = domains.entries.get_mut(&owner.handle.sequence).unwrap();
            domain.module = module.clone();
            domain.ty = module.instance_info("read").unwrap().signature.params[0]
                .reference_type()
                .unwrap()
                .target
                .clone();
            domain.code += 1;
            plan.commit(domain);
        }
        drop(plan);
        drive(&vm);
        assert_eq!(value(&vm, &owner), 11);
        owner.close();
        drive(&vm);
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn vm_source_atomic_publication_preserves_entry_and_timers_on_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let source = format!("{AUTO}\nfn version()->Int32{{1}}");
        let (mut vm, clock) = setup_auto(mode, &source, 2);
        let owner = create_auto(&vm);
        let original = vm.current.clone().unwrap();
        let usage = vm.domains.borrow().usage();
        let next = source
            .replace("{1}", "{2}")
            .replace("self.value+=1", "self.value+=2");
        let overloaded = format!("{next}\nimpl Room{{#every(10ms) fn extra(&mut self){{self.value+=100}} #every(10ms) fn third(&mut self){{self.value+=1000}}}}");
        assert!(vm.publish_source(&overloaded).is_err());
        assert!(source::same_module(vm.current.as_ref().unwrap(), &original));
        assert_eq!(vm.call_int32_0("version").unwrap(), 1);
        assert_eq!(vm.task_count(), 1);
        assert_eq!(vm.domains.borrow().usage(), usage);
        advance(&vm, &clock, 10);
        assert_eq!(value(&vm, &owner), 11);
        assert_eq!(vm.publish_source(&next).unwrap(), 1);
        assert_eq!(vm.call_int32_0("version").unwrap(), 2);
        advance(&vm, &clock, 10);
        assert_eq!(value(&vm, &owner), 13);
        owner.close();
        drive(&vm);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn vm_source_atomic_publication_revalidates_after_clock_callbacks() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let source = format!("{AUTO}\nfn version()->Int32{{1}}");
        let (mut vm, clock) = setup_auto(mode, &source, 64);
        let owner = create_auto(&vm);
        let registry = vm.domains.clone();
        let sequence = owner.handle.sequence;
        *clock.hook.borrow_mut() = Some(Box::new(move || {
            close(&registry, sequence);
        }));
        let next = source
            .replace("#every(10ms)", "#every(20ms)")
            .replace("{1}", "{2}");
        assert!(vm.publish_source(&next).is_err());
        assert_eq!(vm.call_int32_0("version").unwrap(), 1);
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn every_declarations_keep_nanosecond_cadence_across_equivalent_units() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let source = AUTO.replace("#every(10ms)", "#every(250us)");
        let (mut vm, clock) = setup_auto(mode, &source, 64);
        let owner = create_auto(&vm);
        let start = clock.time.get();
        clock.time.set(start + Duration::from_nanos(249_999));
        drive(&vm);
        assert_eq!(value(&vm, &owner), 10);
        vm.load_source(&source.replace("250us", "0.25ms")).unwrap();
        vm.publish_domain_code().unwrap();
        clock.time.set(start + Duration::from_nanos(250_000));
        drive(&vm);
        assert_eq!(value(&vm, &owner), 11);
        // Skip missed beats, retaining the original 250 us phase.
        clock.time.set(start + Duration::from_nanos(999_999));
        drive(&vm);
        assert_eq!(value(&vm, &owner), 12);
        clock.time.set(start + Duration::from_nanos(1_000_000));
        drive(&vm);
        assert_eq!(value(&vm, &owner), 13);
        owner.close();
        drive(&vm);
        assert_eq!(vm.task_count(), 0);
    }
}
