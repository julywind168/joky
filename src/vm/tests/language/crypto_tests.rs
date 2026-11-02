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
            "crypto_stream_empty",
        ] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42
            );
            clean();
        }
        for (name, finish) in ["crypto_pending", "crypto_stream_pending"]
            .into_iter()
            .flat_map(|name| [false, true].map(|finish| (name, finish)))
        {
            let mut task = vm.call_async(name, &[]).unwrap();
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
        for budget in (0..16384).step_by(64) {
            arena::LIMIT.with(|n| n.set(budget));
            match vm
                .call_async("crypto_stream_pending", &[])
                .and_then(|mut task| {
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
        assert_eq!(vm.call_int32_0("crypto_stream_repeated").unwrap(), 42);
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
            .prepare_parallel_task("crypto_stream_parallel", vec![], vec![])
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
        "import joky/crypto.{type Sha256} fn f() { var state = Sha256.new(); let digest = state.finish(); let again = state.finish(); }",
        "import joky/crypto.{type Sha256} fn copy<T: Copy>(value: T) {} fn f() { copy(Sha256.new()); }",
        "import joky/crypto.{type Sha256} fn f() { var state = Sha256.new(); let text = \"abc\"; state.update(text); }",
        "import joky/crypto.{type HmacSha256} fn f() { let key = \"key\"; var state = HmacSha256.new(key.bytes()); state.verify(key.bytes()); state.update(key.bytes()); }",
        "import joky/crypto.{type HmacSha256} fn f() { let key = \"key\"; var state = HmacSha256.new(key.bytes()); state.finish(); state.verify(key.bytes()); }",
        "import joky/crypto.{type Sha256} fn f() { let state = Sha256.new(); let owner = state.owner; }",

        "import joky/internal/buffer fn f() {}",
        "import joky/slice.{type Slice} #intrinsic(\"buffer.sha256\") fn fake(out: &mut Slice<UInt8>, input: &Slice<UInt8>)",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
}

#[test]
fn crypto_stream_failure_releases_live_states() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let mut task = vm.call_async("crypto_stream_failed", &[]).unwrap();
        assert!(!task.is_finished());
        let crate::runtime::suspension::ResumeError::Failed(error) = task.resume().unwrap_err()
        else {
            panic!("expected runtime failure");
        };
        assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
        drop(task);
        clean();
    }
}
