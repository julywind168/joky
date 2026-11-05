//! Request regions stay live through host waits and refund before the root ends.
use super::{support, InterpreterLimits, VmEngine, VmMode};
use crate::{
    runtime::{
        arena::{LIMIT, LIVE},
        borrowed_host::{
            BorrowingHostV1, BORROWING_HOST_ABI_V1, STOP_CANCELLED, STOP_COMPLETED, STOP_FAILED,
        },
        cps::CpsFrame,
    },
    FunctionSignature, Type,
};
use std::cell::{Cell, RefCell};

const SOURCE: &str = include_str!("../../../../tests/fixtures/suspending_arena.jk");
thread_local! {
    static READY: Cell<bool> = const { Cell::new(false) };
    static FAIL: Cell<bool> = const { Cell::new(false) };
    static STARTS: Cell<usize> = const { Cell::new(0) };
    static STOPS: RefCell<Vec<(u32, usize, Option<i32>)>> = const { RefCell::new(Vec::new()) };
    static REFUNDS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}
unsafe extern "C" fn poll(frame: *mut CpsFrame) -> i32 {
    if (*frame).state == 0 {
        STARTS.with(|n| n.set(n.get() + 1));
        (*frame).state = 1;
        return 1;
    }
    if !READY.with(Cell::get) {
        return 1;
    }
    if FAIL.with(Cell::get) {
        return 7;
    }
    let pointer = (&(*frame).slots)[0] as *mut i32;
    *pointer += 2;
    0
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
    let live = LIVE.with(Cell::get);
    // Check the allocation ledger before dereferencing, so early region cleanup
    // is reported as a failed assertion rather than reading freed storage.
    let value = (live >= 2).then(|| *((&(*frame).slots)[0] as *const i32));
    STOPS.with(|events| events.borrow_mut().push((reason, live, value)));
}
extern "C" fn probe(_: i32) {
    REFUNDS.with(|events| events.borrow_mut().push(LIVE.with(Cell::get)));
}
fn reset() {
    READY.with(|v| v.set(false));
    FAIL.with(|v| v.set(false));
    STARTS.with(|v| v.set(0));
    STOPS.with(|v| v.borrow_mut().clear());
    REFUNDS.with(|v| v.borrow_mut().clear());
}
fn clean() {
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
fn configured(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    // SAFETY: The static host borrows exactly one exclusive Int32. It accesses
    // the pointer only during poll/stop, and stop ends all access synchronously.
    unsafe {
        vm.register_borrowing_suspending_fn(
            "arena_wait",
            FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit),
            BorrowingHostV1 {
                abi_version: BORROWING_HOST_ABI_V1,
                poll: Some(poll),
                stop: Some(stop),
            },
            None,
        )
        .unwrap();
        vm.register_fn(
            "arena_probe",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            probe as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{e} {:?}", e.span().and_then(|s| s.slice(SOURCE))));
    vm
}
fn wait_until_host(task: &mut crate::runtime::suspension::Continuation) {
    for _ in 0..50 {
        if STARTS.with(Cell::get) != 0 {
            return;
        }
        task.resume().unwrap();
    }
    panic!("host did not start");
}
#[test]
fn suspending_arena_keeps_host_borrows_until_stop_and_refunds_on_completion() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for entry in ["request_region", "request_transfer"] {
            reset();
            let mut task = vm.call_async(entry, &[]).unwrap();
            wait_until_host(&mut task);
            let live = LIVE.with(Cell::get);
            for _ in 0..3 {
                task.resume().unwrap();
                assert_eq!(LIVE.with(Cell::get), live);
            }
            assert_eq!(STARTS.with(Cell::get), 1);
            READY.with(|v| v.set(true));
            assert_eq!(complete(task).unwrap(), 42);
            STOPS.with(|v| assert_eq!(*v.borrow(), [(STOP_COMPLETED, live, Some(42))]));
            REFUNDS.with(|v| assert!(v.borrow().iter().all(|n| *n == 0)));
            clean();
        }
        reset();
        assert_eq!(
            complete(vm.call_async("request_nested", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
}
#[test]
fn suspending_arena_stops_pending_hosts_before_cancellation_and_failure_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for fail in [false, true] {
            reset();
            let mut vm = configured(mode);
            let mut task = vm.call_async("request_region", &[]).unwrap();
            wait_until_host(&mut task);
            let live = LIVE.with(Cell::get);
            vm.load_source("fn replacement()->Int32{7}").unwrap();
            drop(vm);
            if fail {
                READY.with(|v| v.set(true));
                FAIL.with(|v| v.set(true));
                assert!(task.resume().is_err());
            }
            drop(task);
            STOPS.with(|v| {
                assert_eq!(
                    *v.borrow(),
                    [(
                        if fail { STOP_FAILED } else { STOP_CANCELLED },
                        live,
                        Some(40)
                    )]
                )
            });
            clean();
        }
        reset();
        READY.with(|v| v.set(true));
        let vm = configured(mode);
        assert!(complete(vm.call_async("request_failure", &[]).unwrap()).is_err());
        clean();
    }
}
struct Limit(usize);
impl Drop for Limit {
    fn drop(&mut self) {
        LIMIT.with(|n| n.set(self.0));
    }
}
#[test]
fn suspending_arena_reclaims_each_request_within_one_root_budget() {
    let _limit = Limit(LIMIT.with(|n| n.replace(8192)));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        reset();
        READY.with(|v| v.set(true));
        let vm = configured(mode);
        assert_eq!(
            complete(vm.call_async("request_loop", &[]).unwrap()).unwrap(),
            42
        );
        assert_eq!(STARTS.with(Cell::get), 1000);
        REFUNDS.with(|v| {
            let counts = v.borrow();
            assert_eq!(counts.len(), 1000);
            assert!(
                counts.iter().all(|n| *n == 0),
                "{mode:?}: region was not fully reclaimed: {:?}",
                &counts[..counts.len().min(8)]
            );
        });
        clean();
    }
}
fn cancel(sweep: support::CancellationSweep) {
    reset();
    READY.with(|v| v.set(true));
    let mut vm = configured(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let snapshot = || {
        (
            LIVE.with(Cell::get),
            STARTS.with(Cell::get),
            STOPS.with(|v| v.borrow().len()),
        )
    };
    let mut task = vm.call_async("request_region", &[]).unwrap();
    let mut trace = vec![snapshot()];
    while !task.is_finished() {
        assert!(trace.len() < 1000);
        task.resume().unwrap();
        trace.push(snapshot());
    }
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    clean();
    let mut cuts = std::collections::BTreeSet::from([0, trace.len() - 1]);
    for index in 1..trace.len() {
        if matches!(sweep, support::CancellationSweep::EveryInstruction)
            || trace[index] != trace[index - 1]
        {
            cuts.extend(index.saturating_sub(1)..=(index + 1).min(trace.len() - 1));
        }
    }
    // Replay through language yields as well as interpreter time slices. A
    // single larger poll cannot reach cuts beyond an explicit host Pending.
    for prefix in cuts {
        reset();
        READY.with(|v| v.set(true));
        let mut task = vm.call_async("request_region", &[]).unwrap();
        for _ in 0..prefix {
            task.resume().unwrap();
        }
        assert_eq!(snapshot(), trace[prefix]);
        drop(task);
        STOPS.with(|v| {
            assert!(v
                .borrow()
                .iter()
                .all(|(_, count, value)| *count >= 2 && value.is_some()))
        });
        clean();
    }
}

#[test]
fn suspending_arena_cancels_at_resource_boundaries() {
    cancel(support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel suspending request regions at every instruction"]
fn exhaustive_suspending_arena_cancellation() {
    cancel(support::CancellationSweep::EveryInstruction);
}

#[test]
fn suspending_arena_rejects_escape_conflicts_and_indirect_concurrency() {
    for body in [
        "fn bad() @suspends -> &mut Int32 { let a = Arena.new(); task.yield_now(); a.alloc(42) }",
        "fn bad() @suspends { let a = Arena.new(); let p = a.alloc(42); task.yield_now(); mem.drop(a); println(*p) }",
        "fn bad() @suspends { let a = Arena.new(); let p = a.alloc(42); task.yield_now(); let b = a; println(*p) }",
        "fn bad() @suspends { let a = Arena.new(); let p = a.alloc(42); task.yield_now(); alloc((&*p,)); }",
        "fn bad() @suspends { let outer = Arena.new(); { let inner = Arena.new(); let p = inner.alloc(42); task.yield_now(); outer.alloc((&*p,)); } }",
        "fn child(p: &Int32) @suspends -> Int32 { *p } fn helper(p: &Int32) @suspends { join { child(p), child(p) }; } fn forward(p: &Int32) @suspends { helper(p) } fn bad() @suspends { let a = Arena.new(); let p = a.alloc(42); forward(p) }",
        "fn child(p: &Int32) @suspends { println(*p) } fn helper(p: &Int32) @suspends { scope { branch { child(p) } } } fn bad() @suspends { let a = Arena.new(); let p = a.alloc(42); helper(p) }",
        "fn child(p: &Int32) @suspends -> Int32 { *p } fn helper(p: &Int32) @suspends -> Int32 { race { child(p), child(p) } } fn bad() @suspends { let a = Arena.new(); let p = a.alloc(42); helper(p); }",
        "fn helper(p: &Int32, n: Int32) @suspends { if n > 0 { forward(p, n - 1) } else { join { { println(*p) }, {} }; } } fn forward(p: &Int32, n: Int32) @suspends { helper(p, n) } fn bad() @suspends { let a = Arena.new(); let p = a.alloc(42); forward(p, 2) }",
        "fn bad(f: fn() @suspends -> Int32) @suspends -> Int32 { let a = Arena.new(); let p = a.alloc(42); f(); *p }",
    ] {
        let source = format!("import joky/arena.{{type Arena}} import joky/task import joky/mem {body}");
        let error = support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(matches!(error.code(), crate::ErrorCode::InvalidCall | crate::ErrorCode::InvalidAssignment | crate::ErrorCode::TypeMismatch), "{source}: {error}");
        if body.contains("fn helper") || body.contains("fn bad(f:") {
            assert!(error.message().contains("concurrency"), "{source}: {error}");
        }
    }
}

#[test]
fn suspending_arena_allocation_failure_after_yield_reclaims_partial_regions() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        let mut completed = false;
        let mut failed = false;
        for budget in (0..8192).step_by(32) {
            reset();
            READY.with(|v| v.set(true));
            let _limit = Limit(LIMIT.with(|n| n.replace(budget)));
            let result = vm.call_async("request_region", &[]).and_then(complete);
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => {
                    assert_eq!(
                        error.code(),
                        crate::ErrorCode::ResourceLimit,
                        "{mode:?} budget {budget}: {error}"
                    );
                    failed = true;
                }
            }
            clean();
            if completed {
                break;
            }
        }
        assert!(completed && failed, "{mode:?}");
    }
}

fn complete(mut task: crate::runtime::suspension::Continuation) -> Result<i32, crate::Error> {
    for _ in 0..10000 {
        if task.is_finished() {
            return Ok(task.cps_result().unwrap() as i32);
        }
        task.resume()?;
    }
    panic!("request did not finish");
}
