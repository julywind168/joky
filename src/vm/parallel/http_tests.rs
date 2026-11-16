use super::*;
use crate::runtime::{cps::CpsFrame, parallel::ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

struct Probe {
    budget: TaskBudget,
    rows: Vec<(u64, usize)>,
}
static PROBE: Mutex<Option<Probe>> = Mutex::new(None);
// The registered host ABI supplies one Int32 argument in a live frame. The test
// retains PROBE until both local execution and the worker pool have drained.
unsafe extern "C" fn observe(frame: *mut CpsFrame) -> i32 {
    let mut probe = PROBE.lock().unwrap();
    let probe = probe.as_mut().unwrap();
    probe.rows.push(((&(*frame).slots)[0], probe.budget.used()));
    0
}

#[test]
fn http_route_index_preserves_dispatch_and_charges_only_handles_for_connections() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let budget = TaskBudget::new(8 << 20);
        *PROBE.lock().unwrap() = Some(Probe {
            budget: budget.clone(),
            rows: vec![],
        });
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "observe",
                FunctionSignature::new(vec![Type::Int32], Type::Unit),
                observe,
                1,
                Arc::new(()),
            )
            .unwrap();
        }
        let source = include_str!("../../../tests/fixtures/http_route_index.jk").replace(
            "fn observe(stage: Int32) @suspends {}",
            "extern fn observe(stage: Int32) @suspends",
        );
        vm.load_source(&source)
            .unwrap_or_else(|error| panic!("{error}"));
        {
            let mut task = vm
                .call_async("main", &[])
                .unwrap_or_else(|error| panic!("{error}"));
            while !task.is_finished() {
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(0));
        }
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        PROBE.lock().unwrap().as_mut().unwrap().rows.clear();
        let prepared = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap_or_else(|error| panic!("{error}"));
        drop(vm);
        let mut pool = ParallelExecutor::new(2, 32).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !task.is_finished() {
            assert!(
                Instant::now() < deadline,
                "HTTP route index task did not finish: {mode:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let result = task.take_result().unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result.value(), &TaskValue::Scalar(InstanceValue::Int32(0)));
        drop((task, result));
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
        let rows = PROBE.lock().unwrap().take().unwrap().rows;
        assert_eq!(
            rows.iter().map(|(stage, _)| *stage).collect::<Vec<_>>(),
            [0, 1, 2, 3, 0, 1, 2, 3]
        );
        let small = rows[1].1 - rows[0].1;
        let large = rows[5].1 - rows[4].1;
        assert!(
            rows[4].1 > rows[0].1 + 32_768,
            "fixture must retain a materially larger index: {rows:?}"
        );
        assert!(
            large > 0 && large < 131_072 && large.abs_diff(small) < 4096,
            "connection handles must not copy the index: {rows:?}"
        );
        for rows in rows.as_chunks::<4>().0 {
            assert!(
                rows[2].1 >= rows[0].1,
                "dropping the original must retain the payload: {rows:?}"
            );
            assert!(
                rows[1].1 - rows[2].1 < 4096,
                "dropping one handle must refund only its metadata: {rows:?}"
            );
            assert!(
                rows[3].1 < rows[0].1,
                "last handle must refund the index: {rows:?}"
            );
        }
    }
}
