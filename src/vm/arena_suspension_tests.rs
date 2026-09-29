//! Task arena lifetime, sequential suspension, and reference boundary checks.
use super::{support::*, VmEngine, VmMode};
use crate::runtime::{
    arena::{LIMIT, LIVE},
    scheduler::Scheduler,
    suspension::{ContinuationStatus, ResumeError},
};

const SOURCE: &str = include_str!("../../tests/fixtures/arena_suspension.jk");

fn live(expected: usize) {
    LIVE.with(|count| assert_eq!(count.get(), expected));
}

#[test]
fn arena_suspension_keeps_objects_without_replaying_allocation_or_arguments() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let mut task = vm.call_async("arena_suspend", &[]).unwrap();
        live(1);
        for _ in 0..4 {
            assert_eq!(task.status(), ContinuationStatus::Pending);
            task.resume().unwrap();
            live(1);
            EVENTS.with(|events| assert_eq!(*events.borrow(), [10, 30]));
        }
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42), "{mode:?}");
        live(0); // Terminal continuations may stay in a scheduler.
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        drop(task);
        live(0);
    }
}

#[test]
fn arena_suspension_returns_references_and_sealed_graphs_across_child_completion() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, allocations, events) in [
            ("arena_return_suspend", 1, vec![20, 0, 20, 0, 0]),
            ("arena_cycle_suspend", 2, vec![20, 22, 0]),
            ("arena_loop_suspend", 2, vec![13, 0, 13, 0, 13, 0]),
            ("arena_scope_wait", 1, vec![42, 0, 0]),
        ] {
            let vm = parity_vm(SOURCE, mode);
            let mut task = vm.call_async(name, &[]).unwrap();
            live(allocations);
            READY.with(|ready| ready.set(true));
            for _ in 0..8 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42), "{mode:?}: {name}");
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
            live(0);
        }
    }
}

#[test]
fn arena_suspension_failure_releases_before_continuation_collection() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for name in ["arena_fail_suspend", "arena_host_fail_suspend"] {
            let vm = parity_vm(SOURCE, mode);
            let args: &[i32] = if name == "arena_fail_suspend" {
                &[0]
            } else {
                &[]
            };
            let mut task = vm.call_async(name, args).unwrap();
            live(1);
            READY.with(|ready| ready.set(true));
            let error = task.resume().unwrap_err();
            if name == "arena_fail_suspend" {
                let ResumeError::Failed(error) = error else {
                    panic!("{error:?}")
                };
                assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
                assert_eq!(error.message(), "Division by zero");
                assert_eq!(error.span().unwrap().slice(SOURCE), Some("p.x / divisor"));
            }
            assert!(matches!(task.status(), ContinuationStatus::Failed(_)));
            assert_eq!(task.cps_result(), None);
            live(0);
            drop(task);
            live(0);
        }
    }
}

#[test]
fn arena_suspension_cancel_drop_reload_and_vm_drop_keep_task_ownership() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(SOURCE, mode);
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("arena_suspend", &[]).unwrap());
        let dropped = vm.call_async("arena_cycle_suspend", &[]).unwrap();
        let mut resumed = vm.call_async("arena_suspend", &[]).unwrap();
        live(4);
        assert!(vm.load_source("fn broken(").is_err());
        vm.load_source("fn arena_suspend() @suspends -> Int32 { 99 }")
            .unwrap();
        assert_eq!(
            vm.call_async("arena_suspend", &[]).unwrap().cps_result(),
            Some(99)
        );
        drop(vm);
        live(4);
        scheduler.cancel(cancelled).unwrap();
        live(3);
        drop(dropped);
        live(1);
        READY.with(|ready| ready.set(true));
        resumed.resume().unwrap();
        assert_eq!(resumed.cps_result(), Some(42));
        live(0);
    }
}

#[test]
fn arena_suspension_allocation_budget_is_task_wide_and_keeps_failure_span() {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let charge = std::mem::size_of::<i32>()
        + std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let _reset = Reset(LIMIT.with(|limit| limit.replace(charge * 2)));
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn make(x: Int32) -> &mut Int32 { alloc(x) }
        fn f() @suspends -> Int32 {
            let first = make(20)
            delayed_value(0)
            let second = make(22)
            let third = alloc(0)
            memory.read(first) + memory.read(second) + memory.read(third)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("f", &[]).unwrap();
        live(1);
        READY.with(|ready| ready.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected allocation failure")
        };
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        assert_eq!(error.span().unwrap().slice(source), Some("alloc(0)"));
        live(0);
    }
}

#[test]
fn arena_suspension_preserves_borrow_checks_and_rejects_scope_and_host_escapes() {
    let prefix = "struct Point { x: Int32 } fn wait() @suspends {} ";
    for body in [
        "fn f() @suspends -> Int32 { let p = alloc(Point { x: 1 }); let q = share(p); wait(); p.x = 2; q.x }",
        "fn f() @suspends { let p = alloc(Point { x: 1 }); let q = p; wait(); p.x = 2; q.x = 3; }",
        "fn use(a: &mut Point, b: &Point) @suspends {} fn f() @suspends { let p = alloc(Point { x: 1 }); use(p, p) }",
        "struct Stored { p: &Point } fn store(p: &Point) @suspends { let s = alloc(Stored { p: p }); wait(); } fn f() @suspends { let p = alloc(Point { x: 1 }); store(p); p.x = 2; }",
        "fn branch(p: &mut Point) @suspends { wait(); p.x = 2; } fn f() @suspends { let p = alloc(Point { x: 1 }); scope { | branch(p) } }",
        "fn branch(p: Option<&Point>) @suspends {} fn f() @suspends { scope { | branch(None) } }",
        "fn inner() { let p = alloc(1); } fn branch() @suspends { inner(); } fn f() @suspends { scope { | branch() } }",
        "fn inner<T>() { let p = alloc(1); } fn branch<T>() @suspends { inner::<T>(); } fn f() @suspends { scope { | branch::<Int32>() } }",
        "extern fn host(p: &Point) @suspends",
        "extern fn host() @suspends -> &mut Point",
        "extern fn host(p: Option<&Point>) @suspends",
        "fn f() @suspends { graph { let a = graph.alloc(Point { x: { wait(); 1 } }); a }; }",
    ] {
        let source = format!("{prefix}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let error = vm.load_source(&source).expect_err(&format!("{mode:?} accepted {body}"));
            assert!(error.span().is_some(), "{error}");
            if body.contains("scope {") {
                assert!(error.message().contains("scope branches"), "{body}: {error}");
            }
        }
        let mut aot = crate::aot::AotCompiler::new("arena-suspension-rejection").unwrap();
        let ast = crate::parser::parse(&source).unwrap();
        assert!(aot.compile_module(&ast).is_err(), "AOT accepted {body}");
    }
}

#[test]
fn arena_suspension_reference_helpers_stay_private_and_immediate_tasks_release() {
    let source = r#"
        fn make<T>(value: T) -> &mut T { alloc(value) }
        fn read<T>(view: &T) @suspends -> T { memory.read(view) }
        fn empty() @suspends -> Option<&Int32> { None }
        fn aggregate(view: (Option<&Int32>,)) @suspends {}
        fn f() @suspends -> Int32 { let p = make(42); read(p) }
        fn fail() @suspends -> Int32 { let p = make(42); memory.read(p) / 0 }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        for name in ["empty", "aggregate"] {
            assert!(vm.function_signature(name).is_none());
            assert!(vm.call_async(name, &[]).is_err());
        }
        assert_eq!(vm.call_async("f", &[]).unwrap().cps_result(), Some(42));
        live(0);
        assert!(vm.call_async("fail", &[]).is_err());
        live(0);
    }
}

#[test]
fn arena_suspension_interpreter_instruction_slices_keep_objects_and_cleanup_on_exhaustion() {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async("arena_cycle_suspend", &[]).unwrap();
    READY.with(|ready| ready.set(true));
    let mut peak = 0;
    for _ in 0..2000 {
        LIVE.with(|count| peak = peak.max(count.get()));
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert_eq!(task.cps_result(), Some(42));
    assert_eq!(peak, 2);
    EVENTS.with(|events| assert_eq!(*events.borrow(), [20, 22, 0]));
    live(0);

    vm.load_source("fn f() @suspends { let p = alloc(0); while true { memory.write(p, memory.read(p) + 1); } }").unwrap();
    vm.set_interpreter_limits(super::InterpreterLimits {
        instruction_budget: Some(100),
        poll_budget: Some(10),
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    live(1);
    for _ in 0..20 {
        if let Err(error) = task.resume() {
            let ResumeError::Failed(error) = error else {
                panic!("{error:?}")
            };
            assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
            break;
        }
    }
    assert!(matches!(task.status(), ContinuationStatus::Failed(_)));
    live(0);
}
