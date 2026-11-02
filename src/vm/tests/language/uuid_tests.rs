//! UUID values, entropy failure propagation and bounded storage.
use super::{support::*, VmMode};
use crate::runtime::arena::{self, LIVE};
const SOURCE: &str = include_str!("../../../../tests/fixtures/uuid.jk");
const V7_SOURCE: &str = include_str!("../../../../tests/fixtures/uuid_v7.jk");
fn clean() {
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn uuid_values_rejections_and_suspension() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for name in ["uuid_values", "uuid_rejections", "uuid_secure"] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42
            );
            clean();
        }
        for finish in [false, true] {
            let mut task = vm.call_async("uuid_pending", &[]).unwrap();
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
fn uuid_budget_failures_and_repeated_calls_release_outputs() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
        }
    }
    let _restore = Restore;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let _entropy = crate::runtime::buffer::random::testing::Guard::new(usize::MAX);
        let mut completed = false;
        for budget in (0..8192).step_by(64) {
            arena::LIMIT.with(|n| n.set(budget));
            match vm.call_async("uuid_pending", &[]).and_then(|mut task| {
                task.resume()?;
                assert_eq!(task.cps_result(), Some(42));
                Ok(task)
            }) {
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
        arena::LIMIT.with(|n| n.set(8192));
        assert_eq!(vm.call_int32_0("uuid_repeated").unwrap(), 42);
        clean();
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
    }
}
#[test]
fn uuid_parallel_workers_preserve_results() {
    use crate::{
        runtime::parallel::ParallelExecutor,
        vm::{InstanceValue, TaskValue},
    };
    for (source, name) in [(SOURCE, "uuid_parallel"), (V7_SOURCE, "uuid_v7_parallel")] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let vm = parity_vm_with_prelude(source, mode, false);
            let prepared = vm.prepare_parallel_task(name, vec![], vec![]).unwrap();
            let mut pool = ParallelExecutor::new(2, 8).unwrap();
            let task = prepared.submit(&pool).unwrap();
            task.wait().unwrap();
            assert_eq!(
                task.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::Int32(42))
            );
            drop(task);
            pool.wait_closed();
        }
    }
}

#[test]
fn uuid_entropy_pattern_and_failure() {
    use crate::runtime::buffer::random::testing::Guard;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for (successes, name) in [(0, "uuid_failure"), (usize::MAX, "uuid_pattern")] {
            let guard = Guard::new(successes);
            assert_eq!(vm.call_int32_0(name).unwrap(), 42);
            assert_eq!(guard.calls(), 1);
            clean();
        }
    }
}

#[test]
fn uuid_fields_helpers_and_fixed_byte_length_are_checked() {
    for source in [
        "import joky/uuid fn f() { uuid.Uuid { high: 0, low: 0 }; }",
        "import joky/uuid fn f() { uuid.nibble(0); }",
        "import joky/uuid import joky/fixed_array fn f() { let bytes = fixed_array.repeat::<UInt8, 15>(0); uuid.Uuid.from_bytes(&bytes); }",
    ] {
        frontend_error(source, &crate::ModuleSources::new());
    }
}

#[test]
fn uuid_v7_timestamps_ordering_and_suspension() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(V7_SOURCE, mode, false);
        for name in ["uuid_v7_values", "uuid_v7_invalid_time", "uuid_v7_secure"] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42
            );
            clean();
        }
        for finish in [false, true] {
            let mut task = vm.call_async("uuid_v7_pending", &[]).unwrap();
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
fn uuid_v7_entropy_failures_and_bounded_generation() {
    use crate::runtime::buffer::random::testing::Guard;
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(V7_SOURCE, mode, false);
        {
            let guard = Guard::new(0);
            assert_eq!(vm.call_int32_0("uuid_v7_invalid_time").unwrap(), 42);
            assert_eq!(guard.calls(), 0);
            assert_eq!(vm.call_int32_0("uuid_v7_failure").unwrap(), 42);
            assert_eq!(guard.calls(), 2);
            clean();
        }
        let guard = Guard::new(usize::MAX);
        assert_eq!(vm.call_int32_0("uuid_v7_pattern").unwrap(), 42);
        assert_eq!(guard.calls(), 2);
        clean();
        arena::LIMIT.with(|n| n.set(8192));
        assert_eq!(vm.call_int32_0("uuid_v7_repeated").unwrap(), 42);
        assert_eq!(guard.calls(), 130);
        clean();
        let mut completed = false;
        for budget in (0..8192).step_by(64) {
            arena::LIMIT.with(|n| n.set(budget));
            match vm.call_async("uuid_v7_pending", &[]).and_then(|mut task| {
                task.resume()?;
                assert_eq!(task.cps_result(), Some(42));
                Ok(task)
            }) {
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
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
    }
}
