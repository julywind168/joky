use super::*;
use crate::runtime::executor::controlled::ControlledExecutor;
use crate::vm::VmMode;

fn run(vm: &mut VmEngine, control: &ControlledExecutor) -> Vec<String> {
    vm.executor = Rc::new(control.executor().clone());
    vm.domains = vm.executor.domains();
    let a = vm
        .create_domain("make", &[InstanceValue::Int32(100)])
        .unwrap();
    let b = vm
        .create_domain("make", &[InstanceValue::Int32(200)])
        .unwrap();
    let mut requests = Vec::new();
    for n in 0..12 {
        let (handles, method, args) = match n % 3 {
            0 => (
                [a.write_handle(), b.write_handle()],
                "transfer",
                vec![InstanceValue::Int32(1)],
            ),
            1 => (
                [b.write_handle(), a.write_handle()],
                "transfer",
                vec![InstanceValue::Int32(2)],
            ),
            _ => ([a.read_handle(), b.read_handle()], "sum", vec![]),
        };
        requests.push(vm.queue_domain_pair(handles, method, &args).unwrap());
    }
    vm.poll_tasks(1).unwrap();
    control.inject(1, || requests[0].cancel()).unwrap();
    vm.poll_tasks(4).unwrap();
    // Each successful transfer advances both roots, and each shared read sees
    // the conserved total, regardless of the selected ready order.
    let revision = vm.domain_revision(a.read_handle()).unwrap();
    assert_eq!(revision, vm.domain_revision(b.read_handle()).unwrap());
    control
        .inject(2, || {
            a.close();
        })
        .unwrap();
    vm.poll_tasks(32).unwrap();
    let mut transfers = 0;
    let mut results = Vec::new();
    for (n, request) in requests.iter_mut().enumerate() {
        let result = request.take_result().expect("one terminal result");
        match &result {
            DomainOutcome::Value(InstanceValue::Int32(value)) if n % 3 == 2 => {
                assert_eq!(*value, 300)
            }
            DomainOutcome::Value(InstanceValue::Int32(_)) => transfers += 1,
            DomainOutcome::Cancelled => assert_eq!(n, 0),
            DomainOutcome::Closed => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
        results.push(format!("{result:?}"));
        assert!(request.take_result().is_none());
    }
    assert_eq!(revision, transfers);
    b.close();
    assert_eq!(vm.task_count(), 0);
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    control.finish().unwrap();
    results
}
fn seeds(count: u64) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../../../tests/fixtures/state_domain_pairs.jk"
        ))
        .unwrap();
        for seed in 0..count {
            let control = ControlledExecutor::new(seed, 32, 16).unwrap();
            let result = run(&mut vm, &control);
            let replay = ControlledExecutor::replay(control.trace(), 32).unwrap();
            assert_eq!(run(&mut vm, &replay), result, "{mode:?} seed={seed}");
        }
    }
}
#[test]
fn controlled_multi_domain_commit_cancel_close_replay() {
    seeds(4);
}
#[test]
#[ignore = "exhaustive: replay multi-domain lifecycle operations across 256 seeds"]
fn exhaustive_controlled_multi_domain_lifecycle_seeds() {
    seeds(256);
}
