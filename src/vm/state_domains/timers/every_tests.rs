use super::*;
const DECLARED: &str = include_str!("../../../../tests/fixtures/domain_every.jk");
const HOLD: &str = r#"
fn hold() @suspends {
    let room=domain.create(make,10) let timer=domain.timer()
    var p=Input{value:Option.Some((2,true))}
    domain.schedule_with(timer.handle(),room.handle(),tick,p).unwrap()
    p.value=Option.Some((99,true))
    while true {task.yield_now()}
}
"#;
fn observe(vm: &VmEngine, target: DomainHandle) -> i32 {
    drive(vm);
    let mut request = submit(
        &vm.domains,
        target.read_only(),
        "read",
        &[],
        Default::default(),
    )
    .unwrap();
    drive(vm);
    match request.take_result().unwrap() {
        DomainOutcome::Value(InstanceValue::Int32(n)) => n,
        _ => panic!(),
    }
}

#[test]
fn every_declarations_capture_once_preserve_cadence_and_rebind() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let clock = Rc::new(ManualClock {
            time: Cell::new(Instant::now()),
            hook: RefCell::new(None),
            skip: Cell::new(0),
        });
        vm.configure_tasks(clock.clone(), 64).unwrap();
        let source = format!("{DECLARED}\n{HOLD}");
        vm.load_source(&source).unwrap();
        assert_eq!(vm.task_count(), 0);
        assert!(vm.domains.borrow().timers.is_empty());
        let group = TaskGroup::new();
        vm.executor
            .spawn(&group, vm.prepare_async("hold", &[]).unwrap())
            .unwrap();
        drive(&vm);
        let timer = vm
            .domains
            .borrow()
            .timers
            .values()
            .next()
            .unwrap()
            .upgrade()
            .unwrap();
        let target = timer.borrow().target.unwrap();
        assert_eq!(observe(&vm, target), 10);
        clock.time.set(clock.time.get() + Duration::from_millis(9));
        assert_eq!(observe(&vm, target), 10);
        // Skip two deadlines, then queue exactly one turn. The old captured
        // value remains 2, and publication changes only the implementation.
        clock.time.set(clock.time.get() + Duration::from_millis(26));
        for _ in 0..10 {
            vm.poll_tasks(1).unwrap();
            if !vm.domains.borrow().requests.is_empty() {
                break;
            }
        }
        assert_eq!(vm.domains.borrow().requests.len(), 1);
        let next = source
            .replace("#every(10)", "#every(1000)")
            .replace("room.value+=amount", "room.value+=amount*10");
        vm.load_source(&next).unwrap();
        assert_eq!(vm.publish_domain_code().unwrap(), 1);
        drive(&vm);
        assert_eq!(observe(&vm, target), 30);
        clock.time.set(clock.time.get() + Duration::from_millis(4));
        assert_eq!(observe(&vm, target), 30);
        clock.time.set(clock.time.get() + Duration::from_millis(1));
        assert_eq!(observe(&vm, target), 50);
        vm.executor.cancel_group(&group);
        drop(timer);
        drive(&vm);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        assert_eq!(vm.task_count(), 0);
        // Explicit registration from the new module uses the new period.
        let group = TaskGroup::new();
        vm.executor
            .spawn(&group, vm.prepare_async("hold", &[]).unwrap())
            .unwrap();
        drive(&vm);
        let timer = vm
            .domains
            .borrow()
            .timers
            .values()
            .next()
            .unwrap()
            .upgrade()
            .unwrap();
        let target = timer.borrow().target.unwrap();
        clock
            .time
            .set(clock.time.get() + Duration::from_millis(999));
        assert_eq!(observe(&vm, target), 10);
        clock.time.set(clock.time.get() + Duration::from_millis(1));
        assert_eq!(observe(&vm, target), 30);
        vm.executor.cancel_group(&group);
        drop(timer);
        drive(&vm);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn every_declarations_remove_or_change_entries_before_writes() {
    // A removed attribute removes its timer entry but retains the ordinary function.
    let source = r#"struct Room{value:Int32} fn make(value:Int32)->Room{Room{value:value}} fn read(room:&Room)->Int32{room.value}
#every(10) fn tick(room:&mut Room,p:(Int32,UInt32)){room.value+=int32(p.0)+int32(p.1)}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for queued in [false, true] {
            for next in [
                source.replace("#every(10)", ""),
                source.replace("Int32,UInt32", "UInt32,Int32"),
                source.replace("fn tick(", "fn renamed("),
            ] {
                let (mut vm, clock, domain, timer) = setup_source(mode, source);
                start(
                    &vm.domains,
                    timer.token,
                    Schedule {
                        target: domain.write_handle(),
                        module: vm.current.as_ref().unwrap(),
                        name: "$every/4:tick",
                        input: Input::Values(vec![2, 3]),
                        milliseconds: 10,
                        periodic: true,
                        limits: Default::default(),
                    },
                )
                .unwrap();
                clock.time.set(clock.time.get() + Duration::from_millis(10));
                if queued {
                    vm.poll_tasks(1).unwrap();
                    assert_eq!(vm.domains.borrow().requests.len(), 1);
                }
                vm.load_source(&next).unwrap();
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
#[ignore = "exhaustive: cancel declared periodic registrations at every instruction"]
fn exhaustive_every_declarations_cancellation() {
    cancellation(DECLARED, 0..=32768, true);
}
