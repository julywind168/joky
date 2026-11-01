//! Reproducible streams and fallible OS entropy execute through generated code.
use super::{support::*, VmMode};
use crate::runtime::{
    arena::{self, LIVE},
    buffer::random::testing::Guard,
};
const SOURCE: &str = include_str!("../../../../tests/fixtures/random.jk");
fn clean() {
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn random_values_ranges_shuffle_and_suspension() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for name in [
            "random_vectors",
            "random_ranges",
            "random_shuffle",
            "random_secure",
        ] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42
            );
            clean();
        }
        for finish in [false, true] {
            let mut task = vm.call_async("random_pending", &[]).unwrap();
            assert!(!task.is_finished());
            if finish {
                task.resume().unwrap();
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            clean();
        }
    }
}
#[test]
fn random_entropy_failures_return_no_partial_owner_and_zero_needs_no_entropy() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for successes in [0, 1] {
            let guard = Guard::new(successes);
            assert_eq!(vm.call_int32_0("random_zero").unwrap(), 42);
            assert_eq!(guard.calls(), 0);
            assert_eq!(vm.call_int32_0("random_failure").unwrap(), 42);
            assert_eq!(guard.calls(), successes + 1);
            clean();
        }
        let guard = Guard::new(usize::MAX);
        assert_eq!(vm.call_int32_0("random_pattern").unwrap(), 42);
        assert_eq!(guard.calls(), 2);
        clean();
        let error = vm.call_int32_0("random_huge").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        assert_eq!(guard.calls(), 2);
        clean();
    }
}
#[test]
fn random_allocation_failures_release_partial_arrays() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
        }
    }
    let _restore = Restore;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let _entropy = Guard::new(usize::MAX);
        let mut completed = false;
        for budget in (0..2048).step_by(32) {
            arena::LIMIT.with(|n| n.set(budget));
            match vm.call_async("random_pending", &[]) {
                Ok(task) => {
                    drop(task);
                    completed = true;
                }
                Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
            }
            clean();
            if completed {
                break;
            }
        }
        assert!(completed);
        // A descriptor or payload retained on each call would exceed this budget.
        arena::LIMIT.with(|n| n.set(8192));
        assert_eq!(vm.call_int32_0("random_repeated").unwrap(), 42);
        clean();
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
    }
}
#[test]
fn random_parallel_workers_use_owned_state_and_entropy() {
    use crate::{
        runtime::parallel::ParallelExecutor,
        vm::{InstanceValue, TaskValue},
    };
    use std::time::{Duration, Instant};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let prepared = vm
            .prepare_parallel_task("random_parallel", vec![], vec![])
            .unwrap();
        let mut pool = ParallelExecutor::new(2, 8).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let limit = Instant::now() + Duration::from_secs(10);
        while !task.is_finished() {
            assert!(Instant::now() < limit);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Int32(42))
        );
        drop(task);
        pool.wait_closed();
    }
}
#[test]
fn random_state_and_entropy_primitive_are_private() {
    for source in [
        "import joky/random fn f() { random.Rng { a: 0, b: 0, c: 0, d: 0 }; }",
        "import joky/internal/buffer fn f() {}",
        "import joky/slice.{type Slice} #intrinsic(\"buffer.random_fill\") fn fake(b: &mut Slice<UInt8>) -> Bool",
        "import joky/random fn f() { var rng = random.Rng.seeded(0); rng.range(-1, 5); }",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
}
