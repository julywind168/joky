use super::*;
use crate::runtime::executor::{Clock, HostExecutor};
use crate::vm::VmMode;
const SOURCE: &str = include_str!("../../../../tests/fixtures/domain_timers.jk");
struct ManualClock {
    time: Cell<Instant>,
    hook: RefCell<Option<Box<dyn FnOnce()>>>,
    skip: Cell<usize>,
}
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        if self.skip.get() > 0 {
            self.skip.set(self.skip.get() - 1);
        } else {
            let hook = self.hook.borrow_mut().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        self.time.get()
    }
}
fn setup(mode: VmMode) -> (VmEngine, Rc<ManualClock>, DomainOwner, Owner) {
    setup_source(mode, SOURCE)
}
fn setup_source(mode: VmMode, source: &str) -> (VmEngine, Rc<ManualClock>, DomainOwner, Owner) {
    let mut vm = VmEngine::with_mode(mode);
    let clock = Rc::new(ManualClock {
        time: Cell::new(Instant::now()),
        hook: RefCell::new(None),
        skip: Cell::new(0),
    });
    vm.configure_tasks(clock.clone(), 64).unwrap();
    vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
    let domain = vm
        .create_domain("make", &[InstanceValue::Int32(10)])
        .unwrap();
    let timer = create(&vm.domains).unwrap();
    (vm, clock, domain, timer)
}
fn arm(
    vm: &VmEngine,
    domain: &DomainOwner,
    timer: &Owner,
    periodic: bool,
    milliseconds: u64,
) -> Result<(), u64> {
    start(
        &vm.domains,
        timer.token,
        Schedule {
            target: domain.write_handle(),
            module: vm.current.as_ref().unwrap(),
            name: "tick",
            input: Input::Scalar(2),
            periodic,
            nanoseconds: milliseconds * 1_000_000,
            limits: Default::default(),
        },
    )
}
fn drive(vm: &VmEngine) {
    for _ in 0..6 {
        vm.poll_tasks(100).unwrap();
    }
}
fn value(vm: &VmEngine, domain: &DomainOwner) -> i32 {
    let mut request = submit(
        &vm.domains,
        domain.read_handle(),
        "read",
        &[],
        Default::default(),
    )
    .unwrap();
    drive(vm);
    match request.take_result().unwrap() {
        DomainOutcome::Value(InstanceValue::Int32(n)) => n,
        _ => panic!("read failed"),
    }
}
#[test]
fn domain_timers_skip_missed_periods_and_rebind_queued_turns() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock, domain, timer) = setup(mode);
        arm(&vm, &domain, &timer, true, 10).unwrap();
        drive(&vm);
        assert_eq!(value(&vm, &domain), 10);
        clock.time.set(clock.time.get() + Duration::from_millis(35));
        // One scheduler poll admits exactly one pending short turn.
        vm.poll_tasks(1).unwrap();
        assert_eq!(vm.domains.borrow().requests.len(), 1);
        let old = match vm.current.as_ref().unwrap() {
            LoadedModule::Jit(m) => Rc::downgrade(&(m.clone() as Rc<dyn std::any::Any>)),
            LoadedModule::Interpreter(m) => Rc::downgrade(&(m.clone() as Rc<dyn std::any::Any>)),
        };
        vm.load_source(&SOURCE.replace("room.value+amount", "room.value+amount*10"))
            .unwrap();
        assert_eq!(vm.publish_domain_code().unwrap(), 1);
        assert!(old.upgrade().is_none(), "timer pinned old code");
        drive(&vm);
        assert_eq!(value(&vm, &domain), 30);
        // Original cadence is 10,20,30,40. Skip 20/30, next deadline is 40.
        clock.time.set(clock.time.get() + Duration::from_millis(4));
        drive(&vm);
        assert_eq!(value(&vm, &domain), 30);
        clock.time.set(clock.time.get() + Duration::from_millis(1));
        drive(&vm);
        assert_eq!(value(&vm, &domain), 50);
        cancel(&vm.domains, &timer.state, Status::Cancelled);
        assert_eq!(timer.state.borrow().status, Status::Cancelled);
        assert_eq!(vm.domains.borrow().requests.len(), 0);
        drop(timer);
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn domain_timers_cancel_close_rearm_and_shutdown() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for queued in [false, true] {
            let (vm, _, domain, timer) = setup(mode);
            arm(&vm, &domain, &timer, false, 0).unwrap();
            if queued {
                vm.poll_tasks(1).unwrap();
            }
            cancel(&vm.domains, &timer.state, Status::Cancelled);
            drive(&vm);
            assert_eq!(value(&vm, &domain), 10);
            arm(&vm, &domain, &timer, false, 0).unwrap();
            drive(&vm);
            assert_eq!(timer.state.borrow().status, Status::Completed);
            assert_eq!(value(&vm, &domain), 12);
            arm(&vm, &domain, &timer, true, 100).unwrap();
            domain.close();
            assert_eq!(timer.state.borrow().status, Status::Closed);
            drop(timer);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
        let (vm, _, domain, timer) = setup(mode);
        arm(&vm, &domain, &timer, false, 0).unwrap();
        vm.poll_tasks(1).unwrap();
        shutdown(&vm.domains);
        assert_eq!(timer.state.borrow().status, Status::Closed);
        assert_eq!(value(&vm, &domain), 10);
        drop(timer);
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn domain_timers_recheck_cancellation_after_reentrant_clock() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for phase in 0..3 {
            let (vm, clock, domain, timer) = setup(mode);
            if phase > 0 {
                arm(&vm, &domain, &timer, false, 0).unwrap();
            }
            let registry = vm.domains.clone();
            let state = timer.state.clone();
            *clock.hook.borrow_mut() =
                Some(Box::new(move || cancel(&registry, &state, Status::Closed)));
            if phase == 0 {
                assert_eq!(arm(&vm, &domain, &timer, false, 0), Err(1));
            } else {
                // Skip executor's clock read to reenter inside the native worker.
                clock.skip.set(usize::from(phase == 2));
                drive(&vm);
            }
            assert!(clock.hook.borrow().is_none());
            assert_eq!(timer.state.borrow().status, Status::Closed);
            assert_eq!(value(&vm, &domain), 10);
            drop(timer);
            domain.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
fn cancellation(
    source: &str,
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        let baseline = crate::runtime::arena::LIVE.with(Cell::get);
        let mut complete = false;
        for boundary in boundaries.clone() {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut done = false;
            for _ in 0..boundary {
                for result in executor.poll(1).unwrap().completed {
                    assert_eq!(
                        result
                            .result
                            .unwrap_or_else(|e| panic!("{mode:?} boundary={boundary}: {e}")),
                        0
                    );
                    done = true;
                }
                if done {
                    break;
                }
            }
            executor.cancel_group(&group);
            executor.poll(100).unwrap();
            assert_eq!(executor.task_count(), 0);
            assert!(executor.domains().borrow().timers.is_empty());
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            drop(executor);
            assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), baseline);
            if done {
                complete = true;
                break;
            }
        }
        if exhaustive {
            assert!(complete, "{mode:?}: missed completion");
        }
    }
}
#[test]
fn domain_timers_generated_lifetimes_and_cancellation() {
    cancellation(
        SOURCE,
        [
            0, 1, 2, 3, 10, 20, 50, 100, 200, 500, 1000, 2000, 4000, 8000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel timer ownership and queued turns at every interpreter instruction"]
fn exhaustive_domain_timers_cancellation() {
    cancellation(SOURCE, 0..=16384, true);
}

#[test]
fn domain_timers_reject_changed_contracts_without_mutation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for queued in [false, true] {
            for replacement in [
                r#"fn unused(){}"#,
                r#"fn tick(room:&mut Room,amount:UInt64) {room.value=999}
fn wire(owner:domain.WriteHandle<Room>) @suspends {let amount:UInt64=1; domain.write_to(owner,tick,amount).unwrap()}"#,
            ] {
                let (mut vm, _, domain, timer) = setup(mode);
                arm(&vm, &domain, &timer, false, 0).unwrap();
                if queued {
                    vm.poll_tasks(1).unwrap();
                }
                vm.load_source(&format!(
                    r#"import joky/domain
struct Room {{value:Int32}}
fn make(value:Int32)->Room {{Room{{value:value}}}}
fn read(room:&Room)->Int32 {{room.value}}
fn main() @suspends {{ let room=domain.create(make,0); domain.read_from(room.read_handle(),read).unwrap(); }}
{replacement}"#
                ))
                .unwrap_or_else(|e| panic!("{e}"));
                vm.publish_domain_code().unwrap();
                drive(&vm);
                assert_eq!(timer.state.borrow().status, Status::InvalidAccess);
                assert_eq!(value(&vm, &domain), 10);
                assert_eq!(vm.domain_revision(domain.read_handle()).unwrap(), 0);
                drop(timer);
                domain.close();
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
}
#[test]
fn domain_timers_failure_quarantines_target_and_stops_periodic_work() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock, domain, timer) = setup(mode);
        vm.load_source(&SOURCE.replace(
            "room.value=room.value+amount",
            "let message=\"tick failed\"; check.assert(false,&message); room.value=amount",
        ))
        .unwrap();
        vm.publish_domain_code().unwrap();
        arm(&vm, &domain, &timer, true, 1).unwrap();
        clock.time.set(clock.time.get() + Duration::from_millis(1));
        drive(&vm);
        assert_eq!(timer.state.borrow().status, Status::Failed);
        assert!(vm.domains.borrow().entries[&domain.handle.sequence]
            .failure
            .is_some());
        assert!(vm.domains.borrow().requests.is_empty());
        assert!(timer.state.borrow().job.is_none());
        drop(timer);
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn domain_timers_limits_refund_admission_and_queued_work() {
    let (vm, _, domain, timer) = setup(VmMode::Interpreter);
    let baseline = vm.domains.borrow().budget.bytes.get();
    let budget = vm.domains.borrow().budget.clone();
    budget.limit.set(baseline);
    assert!(create(&vm.domains).is_err());
    assert_eq!(arm(&vm, &domain, &timer, true, 1), Err(5));
    assert_eq!(timer.state.borrow().status, Status::Idle);
    assert_eq!(budget.bytes.get(), baseline);
    budget.limit.set(usize::MAX);
    let other = VmEngine::new();
    assert!(state(&other.domains, timer.token).is_none());
    assert_eq!(arm(&vm, &domain, &timer, true, 0), Err(3));
    assert_eq!(budget.bytes.get(), baseline);
    vm.domains.borrow_mut().limits.max_requests = 0;
    arm(&vm, &domain, &timer, false, 0).unwrap();
    drive(&vm);
    assert_eq!(timer.state.borrow().status, Status::ResourceLimit);
    assert_eq!(budget.bytes.get(), baseline);
    drop(timer);
    domain.close();
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}

#[test]
fn migration_invalidates_sleeping_and_queued_timers_without_running_old_code() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for queued in [false, true] {
            let (mut vm, _, domain, timer) = setup(mode);
            let registry = vm.domains.clone();
            arm(&vm, &domain, &timer, false, if queued { 0 } else { 1000 }).unwrap();
            vm.poll_tasks(1).unwrap();
            let owners = vec![vm.script_code_owner().unwrap()];
            let new = SOURCE
                .replace(
                    "struct Room { value:Int32 }",
                    "struct Room { value:Int32, level:Int32 }",
                )
                .replace("Room{value:value}", "Room{value:value,level:1}");
            let new = format!(
                "{new}\nfn upgrade(old:&PreviousState)->Room{{Room{{value:old.value,level:1}}}}"
            );
            let schema = migration::schema(&registry, &owners).unwrap();
            let plan =
                migration::prepare(&vm, &registry, &owners, schema, &new, "upgrade").unwrap();
            migration::publish(&mut vm, &registry, &owners, plan).unwrap();
            assert_eq!(timer.state.borrow().status, Status::InvalidAccess);
            assert!(timer.state.borrow().job.is_none());
            assert!(registry.borrow().requests.is_empty());
            assert_eq!(value(&vm, &domain), 10);
            assert_eq!(vm.domain_revision(domain.read_handle()).unwrap(), 1);
            drop(timer);
            domain.close();
            assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
        }
    }
}

#[test]
fn migration_batch_invalidates_all_changed_timers_and_preserves_compatible_target() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock, first, first_timer) = setup(mode);
        // Retain the original module family across a compatible load.
        let mut owners = vec![vm.script_code_owner().unwrap()];
        let old=format!("{SOURCE}\nstruct Rules {{n:Int32}} fn rules(n:Int32)->Rules {{Rules{{n:n}}}} fn rules_tick(s:&mut Rules,n:Int32) {{s.n=s.n+n}}");
        vm.load_source(&old).unwrap();
        owners.push(vm.script_code_owner().unwrap());
        let second = vm
            .create_domain("make", &[InstanceValue::Int32(20)])
            .unwrap();
        let second_timer = create(&vm.domains).unwrap();
        let rules = vm
            .create_domain("rules", &[InstanceValue::Int32(1)])
            .unwrap();
        let rules_timer = create(&vm.domains).unwrap();
        arm(&vm, &first, &first_timer, false, 0).unwrap();
        arm(&vm, &second, &second_timer, false, 1000).unwrap();
        start(
            &vm.domains,
            rules_timer.token,
            Schedule {
                target: rules.write_handle(),
                module: vm.current.as_ref().unwrap(),
                name: "rules_tick",
                input: Input::Scalar(2),
                periodic: false,
                nanoseconds: 1_000_000_000,
                limits: Default::default(),
            },
        )
        .unwrap();
        vm.poll_tasks(1).unwrap();
        let new = old
            .replace(
                "struct Room { value:Int32 }",
                "struct Room { value:Int32, level:Int32 }",
            )
            .replace("Room{value:value}", "Room{value:value,level:1}");
        let new = format!(
            "{new}\nfn upgrade(old:&PreviousRoom)->Room {{Room{{value:old.value,level:1}}}}"
        );
        let registry = vm.domains.clone();
        let mut batch = migration::batch::begin(&vm, &registry, &owners, &new).unwrap();
        migration::batch::stage(&vm, &registry, &owners, &mut batch, "upgrade", "").unwrap();
        migration::batch::publish(&mut vm, &registry, &owners, batch).unwrap();
        for timer in [&first_timer, &second_timer] {
            assert_eq!(timer.state.borrow().status, Status::InvalidAccess);
            assert!(timer.state.borrow().job.is_none());
        }
        assert_eq!(rules_timer.state.borrow().status, Status::Active);
        assert_eq!(value(&vm, &first), 10);
        assert_eq!(value(&vm, &second), 20);
        clock
            .time
            .set(clock.time.get() + Duration::from_millis(1001));
        drive(&vm);
        assert_eq!(rules_timer.state.borrow().status, Status::Completed);
        assert_eq!(vm.domain_revision(rules.read_handle()).unwrap(), 1);
        drop((first_timer, second_timer, rules_timer, first, second, rules));
        assert_eq!(registry.borrow().usage(), (0, 0, 0, 0));
    }
}

const VALUES: &str = include_str!("../../../../tests/fixtures/domain_timer_values.jk");
fn payload() -> Vec<u64> {
    vec![
        2,
        u64::MAX,
        InstanceValue::Int8(-128).bits(),
        9221120237041090626,
        1,
        '界' as u64,
    ]
}
fn arm_values(
    vm: &VmEngine,
    domain: &DomainOwner,
    timer: &Owner,
    periodic: bool,
    milliseconds: u64,
) -> Result<(), u64> {
    start(
        &vm.domains,
        timer.token,
        Schedule {
            target: domain.write_handle(),
            module: vm.current.as_ref().unwrap(),
            name: "tick",
            input: Input::Values(payload()),
            periodic,
            nanoseconds: milliseconds * 1_000_000,
            limits: Default::default(),
        },
    )
}
#[test]
fn domain_timer_values_cancellation_reclaims_storage() {
    cancellation(
        VALUES,
        [
            0, 1, 2, 3, 10, 20, 50, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        true,
    );
}
#[test]
#[ignore = "exhaustive: cancel timer record input and queued turns at every instruction"]
fn exhaustive_domain_timer_values_cancellation() {
    cancellation(VALUES, 0..=32768, true);
}
#[test]
fn domain_timer_values_repeat_rebind_and_release_code() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock, domain, timer) = setup_source(mode, VALUES);
        let baseline = vm.domains.borrow().budget.bytes.get();
        arm_values(&vm, &domain, &timer, true, 10).unwrap();
        drive(&vm);
        clock.time.set(clock.time.get() + Duration::from_millis(35));
        vm.poll_tasks(1).unwrap();
        assert_eq!(vm.domains.borrow().requests.len(), 1);
        let old = match vm.current.as_ref().unwrap() {
            LoadedModule::Jit(m) => Rc::downgrade(&(m.clone() as Rc<dyn std::any::Any>)),
            LoadedModule::Interpreter(m) => Rc::downgrade(&(m.clone() as Rc<dyn std::any::Any>)),
        };
        vm.load_source(&VALUES.replace("room.value+p.amount", "room.value+p.amount*10"))
            .unwrap();
        assert_eq!(vm.publish_domain_code().unwrap(), 1);
        assert!(old.upgrade().is_none(), "timer pinned old code");
        drive(&vm);
        assert_eq!(value(&vm, &domain), 30);
        clock.time.set(clock.time.get() + Duration::from_millis(4));
        drive(&vm);
        assert_eq!(value(&vm, &domain), 30);
        clock.time.set(clock.time.get() + Duration::from_millis(1));
        drive(&vm);
        assert_eq!(value(&vm, &domain), 50);
        cancel(&vm.domains, &timer.state, Status::Cancelled);
        assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
        arm_values(&vm, &domain, &timer, false, 0).unwrap();
        drive(&vm);
        assert_eq!(value(&vm, &domain), 70);
        assert_eq!(timer.state.borrow().status, Status::Completed);
        assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
        drop(timer);
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn domain_timer_values_reject_changed_contracts_before_and_after_queueing() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for queued in [false, true] {
            for (from, to) in [
                ("amount:Int32,bits", "pub amount:Int32,bits"),
                (
                    "struct Bits {wide:UInt64,tiny:Int8",
                    "struct Bits {tiny:Int8,wide:UInt64",
                ),
                ("fn tick(", "fn replacement("),
            ] {
                let (mut vm, _, domain, timer) = setup_source(mode, VALUES);
                arm_values(&vm, &domain, &timer, false, 0).unwrap();
                if queued {
                    vm.poll_tasks(1).unwrap();
                }
                let mut source = VALUES.replace(from, to);
                if from == "fn tick(" {
                    source = source
                        .replace(",tick,", ",replacement,")
                        .replace(", tick,", ", replacement,");
                }
                vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
                vm.publish_domain_code().unwrap();
                drive(&vm);
                assert_eq!(timer.state.borrow().status, Status::InvalidAccess);
                assert_eq!(value(&vm, &domain), 10);
                assert_eq!(vm.domain_revision(domain.read_handle()).unwrap(), 0);
                drop(timer);
                domain.close();
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
}

#[test]
fn domain_timer_values_refund_admission_and_queued_budgets() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (vm, _, domain, timer) = setup_source(mode, VALUES);
        let budget = vm.domains.borrow().budget.clone();
        let baseline = budget.bytes.get();
        arm_values(&vm, &domain, &timer, true, 100).unwrap();
        let charge = budget.bytes.get() - baseline;
        assert!(charge >= payload().len() * 8);
        cancel(&vm.domains, &timer.state, Status::Cancelled);
        for extra in (0..=charge + 16).step_by(16) {
            budget.limit.set(baseline + extra);
            let before = timer.state.borrow().status;
            let result = arm_values(&vm, &domain, &timer, true, 100);
            if extra < charge {
                assert_eq!(result, Err(5));
                assert_eq!(timer.state.borrow().status, before);
            } else {
                result.unwrap();
                cancel(&vm.domains, &timer.state, Status::Cancelled);
            }
            assert_eq!(budget.bytes.get(), baseline);
            assert_eq!(vm.task_count(), 0);
        }
        budget.limit.set(usize::MAX);
        for bytes in [false, true] {
            vm.domains.borrow_mut().limits = DomainLimits {
                max_requests: if bytes { 4096 } else { 0 },
                max_request_bytes: if bytes { 1 } else { 4 * 1024 * 1024 },
                ..Default::default()
            };
            arm_values(&vm, &domain, &timer, false, 0).unwrap();
            drive(&vm);
            assert_eq!(timer.state.borrow().status, Status::ResourceLimit);
            assert_eq!(budget.bytes.get(), baseline);
            assert_eq!(vm.task_count(), 0);
            assert!(vm.domains.borrow().requests.is_empty());
            vm.domains.borrow_mut().limits = DomainLimits::default();
            assert_eq!(value(&vm, &domain), 10);
        }
        drop(timer);
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn domain_timer_values_cancel_close_and_quarantine_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for queued in [false, true] {
            let (vm, _, domain, timer) = setup_source(mode, VALUES);
            arm_values(&vm, &domain, &timer, false, 0).unwrap();
            if queued {
                vm.poll_tasks(1).unwrap();
            }
            cancel(&vm.domains, &timer.state, Status::Cancelled);
            drive(&vm);
            assert_eq!(value(&vm, &domain), 10);
            arm_values(&vm, &domain, &timer, false, 0).unwrap();
            if queued {
                vm.poll_tasks(1).unwrap();
            }
            domain.close();
            assert_eq!(timer.state.borrow().status, Status::Closed);
            drop(timer);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
        let source = VALUES.replace(
            "room.value=room.value+p.amount",
            "room.value=99; room.value=room.value/(p.amount-p.amount)",
        );
        let (vm, clock, domain, timer) = setup_source(mode, &source);
        arm_values(&vm, &domain, &timer, true, 1).unwrap();
        clock.time.set(clock.time.get() + Duration::from_millis(1));
        drive(&vm);
        assert_eq!(timer.state.borrow().status, Status::Failed);
        assert!(vm.domains.borrow().entries[&domain.handle.sequence]
            .failure
            .is_some());
        assert!(vm.domains.borrow().requests.is_empty());
        assert_eq!(vm.task_count(), 0);
        drop(timer);
        domain.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn domain_timer_values_restore_caller_budget() {
    use crate::runtime::arena;
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(Cell::get));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(VALUES).unwrap();
        let baseline = arena::LIVE.with(Cell::get);
        let mut successes = 0;
        let mut failures = 0;
        for limit in (0..=8192).step_by(128) {
            arena::LIMIT.with(|v| v.set(limit));
            let group = TaskGroup::new();
            let result = (|| {
                let id = vm
                    .executor
                    .spawn(&group, vm.prepare_async("budget", &[])?)?;
                for _ in 0..1000 {
                    for done in vm.poll_tasks(1)?.completed {
                        if done.id == id {
                            return done.result;
                        }
                    }
                }
                panic!("timer budget case stalled");
            })();
            match result {
                Ok(12) => successes += 1,
                Ok(4) => failures += 1,
                Err(e) => {
                    assert_eq!(e.code(), ErrorCode::ResourceLimit, "{e}");
                    failures += 1;
                }
                other => panic!("{other:?}"),
            }
            vm.executor.cancel_group(&group);
            assert_eq!(vm.task_count(), 0);
            assert!(vm.domains.borrow().timers.is_empty());
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            assert_eq!(arena::LIVE.with(Cell::get), baseline);
        }
        assert!(successes > 0 && failures > 0);
        arena::LIMIT.with(|v| v.set(arena::MAX_BYTES));
    }
}

#[path = "every_tests.rs"]
mod every_tests;

#[test]
fn domain_timers_typed_delays_preserve_nanoseconds_through_generated_adapters() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for source in [SOURCE, VALUES] {
            let (vm, clock, domain, timer) = setup_source(mode, source);
            let start = clock.time.get();
            let group = TaskGroup::new();
            let task = vm
                .executor
                .spawn(
                    &group,
                    vm.prepare_async("precision", &[InstanceValue::UInt64(250_000)])
                        .unwrap(),
                )
                .unwrap();
            for _ in 0..10 {
                vm.poll_tasks(100).unwrap();
            }
            let report = vm.poll_tasks(100).unwrap();
            assert_eq!(
                report.next_deadline,
                Some(start + Duration::from_nanos(250_000))
            );
            clock.time.set(start + Duration::from_nanos(249_999));
            assert_eq!(vm.poll_tasks(100).unwrap().polled, 0);
            clock.time.set(start + Duration::from_nanos(250_000));
            let mut result = None;
            for _ in 0..20 {
                for done in vm.poll_tasks(100).unwrap().completed {
                    if done.id == task {
                        result = Some(done.result);
                    }
                }
            }
            assert_eq!(result, Some(Ok(42)));
            drop(timer);
            domain.close();
            assert_eq!(vm.task_count(), 0);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
