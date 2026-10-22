//! Scope-wide loans, task arena ownership, and structured cancellation.
use super::{support::*, VmEngine, VmMode};
use crate::runtime::{
    arena::{LIMIT, LIVE},
    scheduler::Scheduler,
    suspension::{ContinuationStatus, ResumeError},
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/arena_scope.jk");

fn live(expected: usize) {
    LIVE.with(|count| assert_eq!(count.get(), expected));
}

#[test]
fn scope_and_race_reject_overlapping_loans_and_conflicting_argument_evaluation() {
    let prefix = r#"
        struct Point { x: Int32 }
        fn read(p: &Point) @suspends {}
        fn write(p: &mut Point) @suspends {}
        fn pair(a: &mut Point, b: &Point) @suspends {}
        fn tagged(p: &Point, id: Int32) @suspends {}
        fn tick(id: Int32) @suspends {}
        fn change(p: &mut Point) -> Int32 { p.x = 2; p.x }
        fn view(p: &Point) -> &Point { p }
        fn aggregate<T: Copy>(p: (Option<&T>,)) @suspends {}
        fn nested(p: &mut Point) @suspends { parallel { | write(p) } }
    "#;
    for body in [
        "parallel { | write(p) | write(p) }",
        "parallel { | write(p) | read(p) }",
        "parallel { | read(p) | write(p) }",
        "parallel { | pair(p, p) }",
        "parallel { | read(p) | tick({ p.x = 2; 0 }) }",
        "parallel { | read(p) | tick(change(p)) }",
        "parallel { | read(p) | tick({ let q = p; q.x }) }",
        "parallel { | tagged(p, change(p)) }",
        "let q = (&*p); parallel { | read(q) | write(p) }",
        "let q = view(p); parallel { | write(p) | read(q) }",
        "parallel { | read(view(p)) | write(p) }",
        "parallel { | nested(p) | read(p) }",
        "let values = (Some((&*p)),); parallel { | aggregate(values) | write(p) }",
        "let values = (Some((&*p)),); parallel { | write(p) | aggregate(values) }",
        "parallel { | read(p) | tick({ parallel { | write(p) }; 0 }) }",
    ] {
        let source =
            format!("{prefix} fn f() @suspends {{ let p = alloc(Point {{ x: 1 }}); {body} }}");
        reject_borrow(&source);
    }
}

fn reject_borrow(source: &str) {
    // Both completion policies must retain all simultaneous branch loans,
    // even when an earlier branch would complete without suspending.
    for source in [source.to_owned(), source.replace("parallel {", "race {")] {
        reject_structured_borrow(&source);
    }
}

fn reject_structured_borrow(source: &str) {
    let error = frontend_error(source, &crate::ModuleSources::new());
    assert_eq!(
        error.code(),
        crate::ErrorCode::InvalidAssignment,
        "{error}\n{source}"
    );
    assert!(error.message().contains("borrow"), "{error}\n{source}");
    assert!(error.span().is_some(), "{error}");
}

#[test]
fn scope_and_race_retained_parameters_keep_input_loans_after_completion() {
    let prefix = r#"
        struct Point { x: Int32 }
        struct Stored { point: &Point }
        fn store(p: &Point) @suspends { alloc(Stored { point: p }); }
        fn indirect(p: &Point) @suspends { store(p) }
        fn nested(p: &Point) @suspends { parallel { | indirect(p) } }
        fn generic<T: Copy>(p: &T) @suspends { alloc((p,)); }
        fn aggregate(p: (Option<&Point>,)) @suspends { alloc(p); }
    "#;
    for body in [
        "parallel { | store(p) }; p.x = 2;",
        "parallel { | indirect(p) }; p.x = 2;",
        "parallel { | nested(p) }; p.x = 2;",
        "parallel { | generic(p) }; p.x = 2;",
        "parallel { | aggregate((Some((&*p)),)) }; p.x = 2;",
        "parallel { | store(p) | store(p) }; p.x = 2;",
    ] {
        let source =
            format!("{prefix} fn f() @suspends {{ let p = alloc(Point {{ x: 1 }}); {body} }}");
        reject_borrow(&source);
    }
}

#[test]
fn arena_scope_shared_distinct_nested_graph_and_generic_borrows_join_safely() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, allocations, events) in [
            ("scope_shared", 1, vec![20, 0, 20, 1, 20, 20]),
            ("scope_distinct", 2, vec![0, 0, 21, 21]),
            ("scope_nested", 2, vec![0, 20, 1, 20, 2, 21, 20, 20]),
            ("scope_graph", 2, vec![20, 0, 22, 1, 22, 20]),
            ("scope_aggregate", 1, vec![0, 1, 2]),
            ("scope_retained", 3, vec![0, 1, 21, 21]),
            (
                "scope_allocations",
                3,
                vec![20, 20, 22, 22, 42, 42, 20, 22, 42],
            ),
            (
                "scope_loop",
                2,
                vec![6, 6, 7, 7, 6, 6, 14, 14, 6, 6, 21, 21],
            ),
            ("scope_arguments", 2, vec![0, -1, -1, 0, 0, 21, 21]),
        ] {
            let vm = parity_vm(SOURCE, mode);
            let mut task = vm.call_async(name, &[]).unwrap();
            live(allocations);
            let before = EVENTS.with(|events| events.borrow().clone());
            for _ in 0..3 {
                task.resume().unwrap();
                assert_eq!(task.status(), ContinuationStatus::Pending);
                live(allocations);
                EVENTS.with(|events| assert_eq!(*events.borrow(), before));
            }
            READY.with(|ready| ready.set(true));
            for _ in 0..12 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42), "{mode:?}: {name}");
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events, "{mode:?}: {name}"));
            live(0);
        }
    }
}

#[test]
fn arena_scope_completed_branches_keep_allocations_until_the_root_finishes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let mut task = vm.call_async("scope_partial", &[]).unwrap();
        live(2);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.status(), ContinuationStatus::Pending);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [20, 20, 22, 1, 20, 2]));
        live(2);
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [20, 20, 22, 1, 20, 2, 22]));
        live(0);
    }
}

#[test]
fn arena_scope_nested_failure_stops_siblings_and_releases_terminal_tasks() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let mut task = vm.call_async("scope_failure", &[0]).unwrap();
        live(4);
        READY.with(|ready| ready.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected division failure")
        };
        assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
        assert_eq!(error.span().unwrap().slice(SOURCE), Some("(*p) / divisor"));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [20, 20, 22, 1, 3, 40, 40, 20, 2]));
        live(0);
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        drop(task);
        live(0);
    }
}

#[test]
fn arena_scope_cancel_reload_and_vm_drop_preserve_independent_task_ownership() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(SOURCE, mode);
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("scope_allocations", &[]).unwrap());
        let dropped = vm.call_async("scope_graph", &[]).unwrap();
        let mut resumed = vm.call_async("scope_partial", &[]).unwrap();
        live(7);
        assert!(vm.load_source("fn bad(").is_err());
        vm.load_source("fn scope_partial() @suspends -> Int32 { 99 }")
            .unwrap();
        assert_eq!(
            vm.call_async("scope_partial", &[]).unwrap().cps_result(),
            Some(99)
        );
        drop(vm);
        live(7);
        scheduler.cancel(cancelled).unwrap();
        assert!(scheduler.cancel(cancelled).is_err());
        live(4);
        drop(dropped);
        live(2);
        READY.with(|ready| ready.set(true));
        resumed.resume().unwrap();
        assert!(!resumed.is_finished());
        live(2);
        drop(resumed);
        live(0);
    }
}

#[test]
fn arena_scope_budget_is_shared_and_allocations_outlive_the_join() {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let charge = std::mem::size_of::<i32>()
        + std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let _reset = Reset(LIMIT.with(|limit| limit.replace(charge * 2)));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let error = vm.call_async("scope_allocations", &[]).unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        live(0);
        let mut task = vm.call_async("scope_budget", &[]).unwrap();
        live(2);
        READY.with(|ready| ready.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected allocation failure")
        };
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        assert_eq!(error.span().unwrap().slice(SOURCE), Some("(alloc(42))"));
        live(0);
    }
}

#[test]
fn arena_scope_argument_failure_never_starts_branches_and_releases_loans() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let mut task = vm.call_async("scope_argument_failure", &[0]).unwrap();
        live(1);
        for _ in 0..3 {
            task.resume().unwrap();
            EVENTS.with(|events| assert_eq!(*events.borrow(), [1]));
        }
        READY.with(|ready| ready.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected argument evaluation failure")
        };
        assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
        assert_eq!(
            error.span().unwrap().slice(SOURCE),
            Some("delayed_value(1) / divisor")
        );
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1]));
        live(0);
    }
}

#[test]
fn arena_scope_reference_branches_stay_private_and_cancel_without_dangling_loans() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        for name in ["read", "write", "nested", "read_node", "retain_view"] {
            assert!(vm.function_signature(name).is_none());
            assert!(vm.call_async(name, &[]).is_err());
        }
        for name in [
            "scope_nested",
            "scope_graph",
            "scope_distinct",
            "scope_retained",
        ] {
            let mut scheduler = Scheduler::new();
            let task = scheduler.suspend(vm.call_async(name, &[]).unwrap());
            scheduler.cancel(task).unwrap();
            live(0);
        }
    }
    let mut compiler = crate::aot::AotCompiler::new("arena-scope-private").unwrap();
    let exports = compiler
        .compile_module(&crate::parser::parse(SOURCE).unwrap())
        .unwrap();
    for name in ["read", "write", "nested", "read_node", "retain_view"] {
        assert!(!exports.iter().any(|export| export == name));
        assert!(compiler.cps_frame_slots(name).is_none());
    }
}

#[test]
fn arena_scope_imported_aliases_preserve_branch_and_storage_contracts() {
    let mut sources = crate::ModuleSources::new();
    sources
        .insert(
            "data",
            r#"
        pub struct Point { pub x: Int32 }
        pub type View = &Point
        pub fn make(x: Int32) -> &mut Point { alloc(Point { x: x }) }
        pub fn read(p: View) @suspends { p.x; }
        pub fn write(p: &mut Point) @suspends { p.x = p.x + 1; }
        pub fn keep<T: Copy>(p: &T) @suspends { alloc((p,)); }
        pub fn nested<T: Copy>(p: &T) @suspends { parallel { | keep(p) } }
        pub fn allocate() @suspends { alloc(42); }
    "#,
        )
        .unwrap();
    let prefix = "import data as d.{make as new, read as inspect, write as change}\n";
    for body in [
        "let p = new(20); let q = new(20); parallel { | change(p) | change(q) }; parallel { | inspect(p) | inspect(p) }; p.x + q.x",
        "let p = new(21); parallel { | d.nested(p) | d.keep(p) }; p.x + p.x",
        "parallel { | d.allocate() | d.allocate() }; 42",
    ] {
        let source = format!("{prefix} fn f() @suspends -> Int32 {{ {body} }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source_with_modules(&source, &sources).unwrap();
            assert_eq!(vm.call_async("f", &[]).unwrap().cps_result(), Some(42));
            live(0);
        }
        let ast = crate::TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &sources).unwrap();
        crate::aot::AotCompiler::new("arena-scope-imports").unwrap().compile_module(&ast).unwrap();
    }
    for body in [
        "parallel { | inspect(p) | change(p) }",
        "parallel { | change(p) | inspect(p) }",
        "parallel { | d.nested(p) }; p.x = 2;",
    ] {
        let source = format!("{prefix} fn f() @suspends {{ let p = new(1); {body} }}");
        let ast = crate::TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &sources)
            .unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let error = vm.load_source_with_modules(&source, &sources).unwrap_err();
            assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment, "{error}");
        }
        let error = crate::aot::AotCompiler::new("arena-scope-import-rejection")
            .unwrap()
            .compile_module(&ast)
            .unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment, "{error}");
    }
}

#[test]
fn arena_scope_interpreter_slices_preserve_loans_and_cleanup_on_exhaustion() {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async("scope_nested", &[]).unwrap();
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
    let mut events = EVENTS.with(|events| events.borrow().clone());
    events.sort_unstable();
    assert_eq!(events, [0, 1, 2, 20, 20, 20, 20, 21]);
    live(0);

    vm.load_source(
        r#"
        fn spin(p: &mut Int32) @suspends { while true { (*(p) = (*p) + 1); } }
        fn f() @suspends { let a = alloc(0); let b = alloc(0); parallel { | spin(a) | spin(b) } }
    "#,
    )
    .unwrap();
    vm.set_interpreter_limits(super::InterpreterLimits {
        instruction_budget: Some(100),
        poll_budget: Some(10),
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    live(2);
    let mut failed = false;
    for _ in 0..20 {
        if let Err(ResumeError::Failed(error)) = task.resume() {
            assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
            failed = true;
            break;
        }
    }
    assert!(failed);
    live(0);
}
