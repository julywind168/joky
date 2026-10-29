//! Portable destruction retains code and finishes before the task tree closes.
use super::*;
use crate::runtime::parallel::{HostCode, ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::{
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};
const SOURCE: &str = include_str!("../../../tests/fixtures/parallel_drop_values.jk");
static OBSERVED: Mutex<Vec<(String, std::thread::ThreadId)>> = Mutex::new(Vec::new());
static SERIAL: Mutex<()> = Mutex::new(());
unsafe extern "C" fn trace(text: *const u8) -> i32 {
    let text = crate::runtime::string::borrow(text).unwrap();
    OBSERVED
        .lock()
        .unwrap()
        .push((text.to_owned(), std::thread::current().id()));
    0
}
fn engine(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    let host = Host {
        name: "trace".into(),
        signature: FunctionSignature::new(vec![Type::reference(Type::String, false)], Type::Int32),
        code: HostCode::TextView(trace),
        _owner: Arc::new(()),
    };
    unsafe {
        vm.register_owned_text_fn(
            "trace",
            host.signature.clone(),
            trace as *const u8,
            Some(Rc::new(host)),
        )
        .unwrap();
    }
    vm.load_source(&SOURCE.replace(
        "fn trace(text: &String) -> Int32 { println(text); 0 }",
        "extern fn trace(text: &String) -> Int32",
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    vm
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < deadline, "portable cleanup did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn scoped_borrow_failed_worker_root_defers_destructors_until_children_stop() {
    use std::sync::{Condvar, OnceLock};
    static GATE: OnceLock<(Mutex<(bool, bool)>, Condvar)> = OnceLock::new();
    unsafe extern "C" fn hold(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        let (state, wake) = GATE.get().unwrap();
        let mut state = state.lock().unwrap();
        state.0 = true;
        wake.notify_all();
        let (state, timeout) = wake
            .wait_timeout_while(state, Duration::from_secs(15), |s| !s.1)
            .unwrap();
        if timeout.timed_out() && !state.1 {
            crate::runtime::error::record_error(Error::new(
                ErrorPhase::Runtime,
                "Reader release timed out",
            ));
            return 3;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("reader stopped".into(), std::thread::current().id()));
        0
    }
    unsafe extern "C" fn fail(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        if !GATE.get().unwrap().0.lock().unwrap().0 {
            return 1;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("parent failed".into(), std::thread::current().id()));
        crate::runtime::error::record_error(Error::new(
            ErrorPhase::Runtime,
            "Parent probe failure",
        ));
        3
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            let (state, wake) = GATE.get().unwrap();
            state.lock().unwrap().1 = true;
            wake.notify_all();
        }
    }
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        *GATE
            .get_or_init(|| (Mutex::new((false, false)), Condvar::new()))
            .0
            .lock()
            .unwrap() = (false, false);
        OBSERVED.lock().unwrap().clear();
        let mut vm = engine(mode);
        for (name, entry) in [
            ("hold_reader", hold as crate::runtime::cps::CpsEntry),
            ("fail_parent", fail),
        ] {
            unsafe {
                vm.register_parallel_suspending_fn(
                    name,
                    FunctionSignature::new(vec![], Type::Unit),
                    entry,
                    0,
                    Arc::new(()),
                )
                .unwrap();
            }
        }
        vm.load_source(
            r#"
            import joky/drop.{type Drop}
            extern fn trace(text: &String) -> Int32
            extern fn hold_reader() @suspends
            extern fn fail_parent() @suspends
            struct Source { text: String }
            impl Drop for Source { fn drop(&mut self) { trace(&self.text); } }
            fn root() @suspends {
                let source = Source { text: "source destroyed" }
                scope { branch { hold_reader() }; fail_parent() }
            }
        "#,
        )
        .unwrap();
        let budget = TaskBudget::new(4 << 20);
        let prepared = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 16).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let release = Release;
        // Stopping is observed after Job::poll has returned the error. Merely
        // seeing the failing callback would race the generated unwind itself.
        wait(|| task.handle.stopping());
        let before_release = OBSERVED.lock().unwrap().clone();
        assert_eq!(
            before_release
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
            ["parent failed"],
            "{mode:?}: early source destruction"
        );
        drop(release);
        task.wait().unwrap();
        assert!(task
            .take_result()
            .err()
            .unwrap()
            .message()
            .contains("Parent probe failure"));
        drop(task);
        pool.wait_closed();
        let events = OBSERVED.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["parent failed", "reader stopped", "source destroyed"],
            "{mode:?}"
        );
        assert_eq!(events[0].1, events[2].1);
        assert_ne!(events[0].1, events[1].1);
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn parallel_drop_values_transfer_return_cancel_and_retain_code() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        OBSERVED.lock().unwrap().clear();
        let mut vm = engine(mode);
        let budget = TaskBudget::new(16 << 20);
        let main = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap_or_else(|e| panic!("{e}"));
        let failed = vm
            .prepare_parallel_task("failed", vec![], vec![budget.clone()])
            .unwrap();
        let parked = vm
            .prepare_parallel_task("parked", vec![], vec![budget.clone()])
            .unwrap();
        let pending_result = vm
            .prepare_parallel_task("pending_result", vec![], vec![budget.clone()])
            .unwrap();
        let cleanup_failed = vm
            .prepare_parallel_task("cleanup_failed", vec![], vec![budget.clone()])
            .unwrap();
        vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 32).unwrap();
        let task = main.submit(&pool).unwrap();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result()
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"))
                .value(),
            &TaskValue::Scalar(InstanceValue::Int32(0))
        );
        drop(task);
        let mut names: Vec<_> = OBSERVED
            .lock()
            .unwrap()
            .iter()
            .map(|(s, t)| {
                assert_ne!(*t, std::thread::current().id());
                s.clone()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["a", "array", "b", "closure", "loser", "winner"],
            "{mode:?}"
        );
        let task = failed.submit(&pool).unwrap();
        wait(|| task.is_finished());
        assert!(task.take_result().is_err());
        drop(task);
        assert!(OBSERVED.lock().unwrap().iter().any(|(s, _)| s == "failed"));
        let task = parked.submit(&pool).unwrap();
        wait(|| OBSERVED.lock().unwrap().iter().any(|(s, _)| s == "ready"));
        task.request_cancel();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result().err().unwrap().code(),
            ErrorCode::Cancelled
        );
        drop(task);
        let task = pending_result.submit(&pool).unwrap();
        wait(|| {
            OBSERVED
                .lock()
                .unwrap()
                .iter()
                .any(|(s, _)| s == "produced")
        });
        task.request_cancel();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result().err().unwrap().code(),
            ErrorCode::Cancelled
        );
        drop(task);
        let task = cleanup_failed.submit(&pool).unwrap();
        wait(|| task.is_finished());
        let error = task
            .take_result()
            .err()
            .expect("destructor failure reaches parent");
        assert!(error.message().contains("destructor failure"), "{error}");
        drop(task);
        let events = OBSERVED.lock().unwrap();
        for name in [
            "failed",
            "cancelled",
            "cleanup-failed",
            "unclaimed",
            "pending-peer",
        ] {
            assert_eq!(
                events.iter().filter(|(s, _)| s == name).count(),
                1,
                "{mode:?}: {name}"
            );
        }
        drop(events);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}
#[test]
#[ignore = "exhaustive: portable cleanup preparation and failed installation budgets"]
fn exhaustive_parallel_drop_value_budgets() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = engine(mode);
        let mut success = false;
        for bytes in (0..(1 << 20)).step_by(256) {
            OBSERVED.lock().unwrap().clear();
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let result = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .and_then(|p| p.submit(&pool))
                .and_then(|t| {
                    wait(|| t.is_finished());
                    t.take_result()
                });
            success = result.is_ok();
            drop(result);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?} bytes={bytes}");
            let mut names: Vec<_> = OBSERVED
                .lock()
                .unwrap()
                .iter()
                .map(|(s, _)| s.clone())
                .collect();
            names.sort();
            let count = names.len();
            names.dedup();
            assert_eq!(
                count,
                names.len(),
                "duplicate destruction at {mode:?} bytes={bytes}"
            );
            if success {
                break;
            }
        }
        assert!(success);
    }
}

#[test]
#[ignore = "exhaustive: each instruction of owning dyn conversion, replacement and cancellation"]
fn exhaustive_owned_dyn_instruction_cancellation() {
    let _serial = SERIAL.lock().unwrap();
    let mut vm = engine(VmMode::Interpreter);
    vm.set_interpreter_limits(crate::vm::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for boundary in 0..2048 {
        OBSERVED.lock().unwrap().clear();
        let mut task = vm.call_async("dynamic_lifecycle", &[]).unwrap();
        for _ in 0..boundary {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.is_finished();
        if completed {
            assert_eq!(task.cps_result(), Some(11));
        }
        drop(task);
        let events = OBSERVED.lock().unwrap();
        for name in ["first", "second"] {
            let count = events.iter().filter(|(s, _)| s == name).count();
            assert!(count <= 1, "{boundary}: {name} released {count} times");
            if completed {
                assert_eq!(count, 1);
            }
        }
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0, "boundary {boundary}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}
