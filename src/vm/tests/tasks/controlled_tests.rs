use super::*;
use crate::runtime::executor::controlled::ControlledExecutor;

fn fair_run(vm: &VmEngine, control: &ControlledExecutor) -> Vec<i32> {
    reset();
    let group = TaskGroup::new();
    for n in 1..=4 {
        control
            .executor()
            .spawn(&group, vm.prepare_async("fair", &[V::Int32(n)]).unwrap())
            .unwrap();
    }
    assert_eq!(control.executor().poll(0).unwrap().polled, 0);
    // Every root is polled once per batch, even when it immediately wakes itself.
    assert_eq!(control.executor().poll(4).unwrap().polled, 4);
    let first = EVENTS.with(|v| v.borrow().clone());
    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(sorted, [1, 2, 3, 4]);
    assert_eq!(control.executor().poll(4).unwrap().polled, 4);
    assert_eq!(control.executor().task_count(), 0);
    control.finish().unwrap();
    EVENTS.with(|v| v.borrow().clone())
}

#[test]
fn controlled_generated_roots_repeat_seed_and_replay_independent_of_process_ids() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, SOURCE);
        let mut orders = std::collections::BTreeSet::new();
        for seed in 0..8 {
            let control = ControlledExecutor::new(seed, 4, 16).unwrap();
            let events = fair_run(&vm, &control);
            orders.insert(events.clone());
            let again = ControlledExecutor::new(seed, 4, 16).unwrap();
            assert_eq!(fair_run(&vm, &again), events);
            assert_eq!(again.trace(), control.trace());
            let replay = ControlledExecutor::replay(control.trace(), 4).unwrap();
            assert_eq!(fair_run(&vm, &replay), events);
        }
        assert!(orders.len() > 1);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

fn timed_run(vm: &VmEngine, control: &ControlledExecutor) {
    reset();
    let executor = control.executor();
    let group = TaskGroup::new();
    executor
        .spawn(&group, vm.prepare_async("work", &[]).unwrap())
        .unwrap();
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    let token = TOKEN.with(|v| v.borrow().clone().unwrap());
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    control
        .inject(100, || {
            READY.with(|v| v.set(true));
            token.wake();
            token.wake();
        })
        .unwrap();
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    control.advance(Duration::from_millis(9)).unwrap();
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    control.advance(Duration::from_millis(1)).unwrap();
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    assert_eq!(executor.poll(1).unwrap().completed[0].result, Ok(42));
    assert_eq!(EVENTS.with(|v| v.borrow().clone()), [1, 2, 3]);
    READY.with(|v| v.set(false));
    executor
        .spawn(&group, vm.prepare_async("branches", &[]).unwrap())
        .unwrap();
    executor.poll(1).unwrap();
    let stale = TOKEN.with(|v| v.borrow().clone().unwrap());
    control
        .inject(101, || {
            assert!(executor.cancel_group(&group));
            stale.wake();
        })
        .unwrap();
    control.advance(Duration::from_millis(5)).unwrap();
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    assert_eq!(executor.task_count(), 0);
    control.finish().unwrap();
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn controlled_generated_timers_events_cancel_and_stale_wakes_replay() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, SOURCE);
        let control = ControlledExecutor::new(99, 4, 32).unwrap();
        timed_run(&vm, &control);
        timed_run(
            &vm,
            &ControlledExecutor::replay(control.trace(), 4).unwrap(),
        );
    }
}

#[test]
fn controlled_trace_limits_and_divergence_stop_before_generated_code() {
    let vm = vm(VmMode::Interpreter, SOURCE);
    let recorded = ControlledExecutor::new(0, 4, 16).unwrap();
    fair_run(&vm, &recorded);
    let trace = recorded.trace();
    for (script, limit, roots) in [
        (None, 1, 1),
        (Some(trace.clone()), 1, 4),
        (Some(trace), 0, 3),
    ] {
        reset();
        let control = match script {
            Some(trace) => ControlledExecutor::replay(trace, 4).unwrap(),
            None => ControlledExecutor::new(0, 4, 0).unwrap(),
        };
        let group = TaskGroup::new();
        for n in 0..roots {
            control
                .executor()
                .spawn(&group, vm.prepare_async("fair", &[V::Int32(n)]).unwrap())
                .unwrap();
        }
        assert!(control.executor().poll(limit).is_err());
        assert!(EVENTS.with(|v| v.borrow().is_empty()));
        assert_eq!(control.executor().task_count(), roots as usize);
        assert!(control.executor().cancel_group(&group));
        assert_eq!(control.executor().task_count(), 0);
        assert!(control.finish().is_err());
    }
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
