//! Password hash ABI, byte preservation, cost limits and owning inputs.
use super::{support::*, VmMode};
use crate::runtime::{
    arena::{self, LIVE},
    driver::block_on,
    password::TEST_LOCK,
};
const SOURCE: &str = include_str!("../../../../tests/fixtures/password.jk");

#[test]
fn password_vm_vectors_roundtrip_limits_and_parallel_jobs() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(
            block_on(vm.prepare_async("password_suite", &[]).unwrap()).unwrap(),
            42
        );
        LIVE.with(|n| assert_eq!(n.get(), 0));
        let prepared = vm
            .prepare_parallel_task("password_parallel", vec![], vec![])
            .unwrap();
        let mut pool = crate::runtime::parallel::ParallelExecutor::new(2, 8).unwrap();
        let task = prepared.submit(&pool).unwrap();
        task.wait().unwrap();
        assert_eq!(
            task.take_result().unwrap().value(),
            &crate::vm::TaskValue::Scalar(crate::vm::InstanceValue::Int32(42))
        );
        drop(task);
        pool.wait_closed();
    }
}
#[test]
fn password_vm_budget_failure_refunds_before_retry() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let mut completed = false;
        for limit in (0..2 * 1024 * 1024).step_by(16384) {
            arena::LIMIT.with(|n| n.set(limit));
            match vm.prepare_async("password_once", &[]).and_then(block_on) {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit, "{error}"),
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
            if completed {
                break;
            }
        }
        assert!(completed);
        arena::LIMIT.with(|n| n.set(8192));
        assert_eq!(vm.call_int32_0("password_metadata_repeated").unwrap(), 42);
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
        assert_eq!(
            block_on(vm.prepare_async("password_failed", &[]).unwrap())
                .unwrap_err()
                .code(),
            crate::ErrorCode::RuntimeError
        );
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn password_api_checks_ownership_suspension_and_private_runtime_signatures() {
    for source in [
        "import joky/password fn f() { password.hash(\"secret\"); }",
        "import joky/password fn f() @suspends { let text = \"secret\"; password.hash(text); password.hash(text); }",
        "import joky/password fn f() @suspends { password.verify(42, \"hash\"); }",
        "import joky/password fn f() { password.error(1); }",
        "extern fn __password_compute() @suspends",
        "fn __password_compute() {}",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
}
