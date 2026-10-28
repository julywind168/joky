//! Owning dynamic objects keep concrete storage, dispatch and cleanup evidence.
use super::*;
use crate::{
    runtime::parallel::ParallelExecutor,
    vm::{InterpreterLimits, VmEngine, VmMode},
};
use std::time::{Duration, Instant};
const SOURCE: &str = include_str!("../../../tests/fixtures/owned_dyn_values.jk");
fn wait(task: &ParallelTask) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "dynamic task did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn owned_dyn_values_preserve_storage_dispatch_and_frozen_code() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(17),
                ..InterpreterLimits::default()
            })
            .unwrap();
        }
        vm.load_source(SOURCE)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        let mut local = vm.call_async("main", &[]).unwrap();
        for _ in 0..10000 {
            if local.is_finished() {
                break;
            }
            local.resume().unwrap();
        }
        assert!(local.is_finished());
        assert_eq!(local.cps_result(), Some(0));
        drop(local);
        let budget = TaskBudget::new(16 << 20);
        let mut prepared = Vec::new();
        for _ in 0..2 {
            prepared.push(
                vm.prepare_parallel_task("main", vec![], vec![budget.clone()])
                    .unwrap_or_else(|e| panic!("{mode:?}: {e}")),
            );
        }
        vm.load_source("fn replaced() -> Int32 { 99 }").unwrap();
        drop(vm);
        for (prepared, workers) in prepared.into_iter().zip([1, 3]) {
            let mut pool = ParallelExecutor::new(workers, 32).unwrap();
            let task = prepared.submit(&pool).unwrap();
            wait(&task);
            assert_eq!(
                task.take_result()
                    .unwrap_or_else(|e| panic!("{mode:?}: {e}"))
                    .value(),
                &TaskValue::Scalar(InstanceValue::Int32(0))
            );
            drop(task);
            pool.wait_closed();
        }
        assert_eq!(budget.used(), 0, "{mode:?}");
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
#[test]
fn owned_dyn_rejects_unqualified_methods_destructors_and_host_packets() {
    unsafe extern "C" fn local() -> i32 {
        1
    }
    for source in [
        "trait Read { fn read(&self) @noescape -> Int32; fn unused(&self) @noescape -> Int32 } struct A { n: Int32 } impl Read for A { fn read(&self) -> Int32 { 1 } fn unused(&self) -> Int32 { local() } } fn main() @suspends { let v: dyn Read = A { n: 1 }; v.read(); }",
        "import joky/drop.{type Drop} trait Read { fn read(&self) @noescape -> Int32 } struct A { text: String } impl Read for A { fn read(&self) -> Int32 { 1 } } impl Drop for A { fn drop(&mut self) { local(); } } fn main() @suspends { let v: dyn Read = A { text: \"x\" }; v.read(); }",
    ] {
        let mut vm = VmEngine::new();
        unsafe { vm.register_fn("local", FunctionSignature::new(vec![], Type::Int32), local as *const u8).unwrap(); }
        vm.load_source(&format!("{source} extern fn local() -> Int32")).unwrap();
        let error = vm.prepare_parallel_task("main", vec![], vec![]).err().expect("reject Local target");
        assert!(error.message().contains("local"), "{error}");
    }
    let mut vm = VmEngine::new();
    vm.load_source("trait Mark {} impl Mark for Int32 {} fn object() @suspends -> dyn Mark { 1 }")
        .unwrap();
    assert!(vm.prepare_parallel_task("object", vec![], vec![]).is_err());
}
#[test]
#[ignore = "exhaustive: owning dyn allocation, transport and failed installation budgets"]
fn exhaustive_owned_dyn_value_budgets() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let mut success = false;
        for bytes in (0..(1 << 20)).step_by(512) {
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let result = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .and_then(|p| p.submit(&pool))
                .and_then(|t| {
                    wait(&t);
                    t.take_result()
                });
            success = result.is_ok();
            drop(result);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?} bytes={bytes}");
            if success {
                break;
            }
        }
        assert!(success);
    }
}

#[test]
fn owned_dyn_preserves_move_borrow_and_erasure_boundaries() {
    let base = "trait Read { fn read(&self) @noescape -> Int32; fn change(&mut self) @noescape {} } struct A { n: Int32 } impl Read for A { fn read(&self) -> Int32 { self.n } } fn consume(v: dyn Read) {} fn inspect(v: &dyn Read) @noescape -> Int32 { v.read() }";
    for (body, expected) in [
        ("fn f() { let v: dyn Read = A { n: 1 }; consume(v); consume(v) }", "moved"),
        ("fn f() { let v: dyn Read = A { n: 1 }; v.change() }", "var binding"),
        ("fn f() { let v: dyn Read = A { n: 1 }; let r = &v; consume(v); inspect(r); }", "borrow"),
        ("extern fn f(v: dyn Read)", "host ABI"),
        ("fn f(v: dyn Read) -> dyn Read { let r = &v; r }", "reference-free"),
        ("struct Borrowed { n: &Int32 } impl Read for Borrowed { fn read(&self) -> Int32 { *self.n } } fn f(n: &Int32) { let v: dyn Read = Borrowed { n }; }", "reference-free"),
    ] {
        let mut vm = VmEngine::new();
        let error = vm.load_source(&format!("{base} {body}")).expect_err("reject invalid ownership or erasure");
        assert!(error.message().contains(expected), "{expected}: {error}\n{body}");
    }
}
