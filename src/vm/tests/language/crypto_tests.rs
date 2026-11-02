//! Cryptographic byte APIs, strict verification and bounded owning outputs.
use super::{support::*, VmMode};
use crate::runtime::arena::{self, LIVE};
const SOURCE: &str = include_str!("../../../../tests/fixtures/crypto.jk");
fn clean() {
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn crypto_vectors_verification_and_suspension() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for name in [
            "crypto_vectors",
            "crypto_verification_and_views",
            "crypto_boundaries",
        ] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42
            );
            clean();
        }
        for finish in [false, true] {
            let mut task = vm.call_async("crypto_pending", &[]).unwrap();
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
fn crypto_budget_failures_and_repeated_calls_release_outputs() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
        }
    }
    let _restore = Restore;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let mut completed = false;
        for budget in (0..8192).step_by(64) {
            arena::LIMIT.with(|n| n.set(budget));
            match vm.call_async("crypto_pending", &[]).and_then(|mut task| {
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
        assert_eq!(vm.call_int32_0("crypto_repeated").unwrap(), 42);
        clean();
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
    }
}
#[test]
fn crypto_parallel_workers_preserve_results() {
    use crate::{
        runtime::parallel::ParallelExecutor,
        vm::{InstanceValue, TaskValue},
    };
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let prepared = vm
            .prepare_parallel_task("crypto_parallel", vec![], vec![])
            .unwrap();
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

#[test]
fn crypto_requires_byte_views_and_keeps_intrinsics_private() {
    for source in [
        "import joky/crypto fn f() { crypto.sha256(\"abc\"); }",
        "import joky/crypto import joky/array.{type Array} fn f() { let values: Array<Int32> = [1]; crypto.sha256(values.slice(0, 1)); }",
        "import joky/crypto fn f() { crypto.output(); }",
        "import joky/internal/buffer fn f() {}",
        "import joky/slice.{type Slice} #intrinsic(\"buffer.sha256\") fn fake(out: &mut Slice<UInt8>, input: &Slice<UInt8>)",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
}
