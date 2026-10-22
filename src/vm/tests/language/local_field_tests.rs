//! Field paths preserve local identity, disjoint access and lexical lifetimes.
use super::{VmEngine, VmMode};
use crate::runtime::{
    arena::{LIVE, LOCAL_LIMIT},
    scheduler::Scheduler,
    suspension::{ContinuationStatus, ResumeError},
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/local_fields.jk");
const CASES: &[&str] = &[
    "field_siblings",
    "field_nested",
    "field_wide",
    "field_zero_size",
    "field_aggregates",
    "field_snapshot",
    "field_reservation",
    "field_reborrow",
    "field_branches",
    "field_loop",
    "field_loop_merge",
    "field_jumps",
    "field_shadow",
    "field_parameters",
    "field_pattern",
    "field_early_return",
    "field_recursion",
];

fn live(expected: usize) {
    LIVE.with(|n| assert_eq!(n.get(), expected));
}

const PREFIX: &str = r#"
    struct Point { x: Int32, y: Int32 }
    struct Pair { left: Point, right: Point }
    struct Holder { view: &Int32 }
    fn bump(value: &mut Int32) { (*(value) = (*value) + 1); }
    fn both(a: &mut Int32, b: &mut Int32) { bump(a); bump(b); }
    fn inspect(value: &Int32) -> Int32 { (*value) }
    fn same(value: &mut Int32) -> &mut Int32 { value }
    fn view(value: &Int32) -> &Int32 { value }
    fn allocate(value: &Int32) { alloc((value,)); }
    fn copy<T: Copy>(value: &T) -> T { (*value) }
    fn store<T: Copy>(value: &T) { alloc((value,)); }
    fn choose(a: &Int32, b: &Int32, flag: Bool) -> &Int32 { if flag { a } else { b } }
"#;

#[test]
fn local_field_borrows_modify_original_fields_and_allow_disjoint_access() {
    for body in [
        "var p = Point { x: 20, y: 20 }; both(&mut p.x, &mut p.y); p.x + p.y",
        "var p = Point { x: 20, y: 20 }; let x = &mut p.x; p.y = p.y + 1; bump(x); p.x + p.y",
        "var p = Point { x: 20, y: 20 }; let x = &p.x; p.y = p.y + 2; inspect(x) + p.y",
        "var p = Point { x: 20, y: 21 }; let y = &p.y; bump(&mut p.x); p.x + inspect(y)",
        "var p = Point { x: 20, y: 20 }; let x = same(&mut p.x); p.y = p.y + 1; bump(x); p.x + p.y",
        "var p = Point { x: 20, y: 20 }; let x = view(&p.x); p.y = p.y + 2; inspect(x) + p.y",
        "var p = Point { x: 20, y: 20 }; p.x = { bump(&mut p.y); p.x + 1 }; p.x + p.y",
        "var p = Pair { left: Point { x: 20, y: 20 }, right: Point { x: 0, y: 0 } }; let x = &mut p.left.x; p.left.y = p.left.y + 1; p.right = Point { x: 1, y: 2 }; bump(x); p.left.x + p.left.y",
        "let p = Point { x: 42, y: 0 }; copy(&p.x)",
        "let p = Point { x: 21, y: 0 }; let a = &p.x; let b = &p.x; inspect(a) + inspect(b)",
        "var p = Point { x: 20, y: 20 }; let h = Holder { view: &p.x }; p.y = 22; inspect(h.view) + p.y",
        "var p = Pair { left: Point { x: 20, y: 20 }, right: Point { x: 20, y: 20 } }; let r = choose(&p.left.x, &p.right.y, true); p.left.y = 22; inspect(r) + p.left.y",
        "var p = Point { x: 0, y: 0 }; bump(&mut p.x); p = Point { x: 20, y: 22 }; p.x + p.y",
        "var p = Point { x: 41, y: 41 }; let x = if true { &mut p.x } else { &mut p.y }; bump(x); p.x + p.y - 41",
        "var p = Point { x: 20, y: 20 }; let x = &mut p.x; let y = &mut p.y; let n = inspect(x); bump(y); bump(x); n + p.y + p.x - 20",
    ] {
        let source = format!("{PREFIX} fn f() -> Int32 {{ {body} }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap_or_else(|error| panic!("{mode:?}: {error}\n{source}"));
            assert_eq!(vm.call_int32_0("f").unwrap(), 42, "{mode:?}: {body}");
        }
    }
}

#[test]
fn local_field_borrows_reject_overlap_escapes_and_unsupported_roots() {
    for (body, message) in [
        ("fn f() { var p = Point { x: 1, y: 2 }; both(&mut p.x, &mut p.x); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = &p.x; p.x = 3; inspect(x); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = &mut p.x; p.x; bump(x); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = &p.x; p = Point { x: 3, y: 4 }; inspect(x); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = &mut p.x; let old = p; bump(x); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = &mut p.x; let whole = &p; bump(x); (*whole); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = &p.x; let whole = &mut p; inspect(x); whole.y = 0; }", "borrow"),
        ("fn f() { var p = Pair { left: Point { x: 1, y: 2 }, right: Point { x: 3, y: 4 } }; let left = &mut p.left; bump(&mut p.left.x); left.x; }", "borrow"),
        ("fn f() { var p = Pair { left: Point { x: 1, y: 2 }, right: Point { x: 3, y: 4 } }; let x = &p.left.x; p.left = Point { x: 3, y: 4 }; inspect(x); }", "borrow"),
        ("fn f() { var p = Pair { left: Point { x: 1, y: 2 }, right: Point { x: 3, y: 4 } }; let x = &mut p.left.x; let left = &mut p.left; bump(x); left.y = 0; }", "borrow"),
        ("struct Empty {} struct Zeros { a: Empty, b: Empty } fn f() { var p = Zeros { a: Empty {}, b: Empty {} }; let a = &mut p.a; let b = &mut p.a; (*(a) = Empty {}); (*(b) = Empty {}); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = &mut p.x; let y = x; bump(x); bump(y); }", "moved"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let h = Holder { view: &p.x }; p.x = 3; inspect(h.view); }", "borrow"),
        ("fn f(flag: Bool) { var p = Pair { left: Point { x: 1, y: 2 }, right: Point { x: 3, y: 4 } }; let r = choose(&p.left.x, &p.right.y, flag); p.right.y = 0; inspect(r); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let held = &p.y; p.x = { p = Point { x: 3, y: 4 }; 0 }; inspect(held); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; var h = Holder { view: (&*(alloc(0))) }; p.x = { h.view = &p.x; 3 }; inspect(h.view); }", "borrow"),
        ("fn f() { var p = Point { x: 1, y: 2 }; let x = same(&mut p.x); bump(&mut p.x); bump(x); }", "borrow"),
        ("fn f(flag: Bool) { var p = Point { x: 1, y: 2 }; let x = if flag { &p.x } else { &p.y }; p.y = 3; inspect(x); }", "borrow"),
        ("fn f() -> &Int32 { let p = Point { x: 1, y: 2 }; &p.x }", "local"),
        ("fn f(p: Point) -> &Int32 { &p.y }", "local"),
        ("fn f() -> Holder { let p = Point { x: 1, y: 2 }; Holder { view: &p.x } }", "local"),
        ("fn f() { let r = { let p = Point { x: 1, y: 2 }; &p.x }; inspect(r); }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); while true { let p = Point { x: 1, y: 2 }; r = &p.x; break }; inspect(r); }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); var i = 0; while i < 2 { let p = Point { x: 1, y: 2 }; r = &p.x; i = i + 1; continue }; inspect(r); }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); if (let p = Point { x: 1, y: 2 }; r = &p.x; true) { inspect(r); } }", "local"),
        ("fn f() { let r = match Some(Point { x: 1, y: 2 }) { Some(p) => &p.x, None => (&*(alloc(0))) }; inspect(r); }", "local"),
        ("fn f() { let p = Point { x: 1, y: 2 }; alloc(Holder { view: &p.x }); }", "retained"),
        ("fn f() { let p = Point { x: 1, y: 2 }; allocate(&p.y); }", "retained"),
        ("fn f() { let p = Point { x: 1, y: 2 }; store(&p.x); }", "retained"),
        ("fn f() { let p = Point { x: 1, y: 2 }; &mut p.x; }", "var binding"),
        ("fn f(p: Point) { &mut p.x; }", "var binding"),
        ("fn f() { let p = alloc(Point { x: 1, y: 2 }); &p.x; }", "Copy data"),
        ("fn f(p: &Point) { &p.x; }", "Copy data"),
        ("fn f() { let p = Holder { view: (&*(alloc(1))) }; &p.view; }", "Copy data"),
        ("fn f() { &(Point { x: 1, y: 2 }).x; }", "temporary borrowing"),
        ("fn f() { let p = Point { x: 1, y: 2 }; &p.absent; }", "Unknown field"),
    ] {
        let source = format!("{PREFIX}{body}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{error}\n{source}");
        assert!(error.span().is_some());
    }
}

#[test]
fn local_field_borrows_preserve_layout_control_flow_snapshots_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        for _ in 0..3 {
            for name in CASES {
                assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
                live(0);
            }
            for name in ["field_failure", "field_mixed_failure"] {
                let error = vm.call_int32_1(name, 0).unwrap_err();
                assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
                assert_eq!(error.span().unwrap().slice(SOURCE), Some("(*x) / divisor"));
                live(0);
                assert_eq!(vm.call_int32_0("field_siblings").unwrap(), 42);
            }
        }
    }
}

#[test]
fn local_field_borrows_release_storage_before_host_pending_without_replay() {
    use super::support::*;
    let source = format!("{SOURCE} extern fn delayed_value(x: Int32) @suspends -> Int32 fn task() @suspends -> Int32 {{ delayed_value(field_loop()) - 1 }}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(&source, mode);
        let mut task = vm.call_async("task", &[]).unwrap();
        live(0);
        for _ in 0..3 {
            task.resume().unwrap();
            assert_eq!(task.status(), ContinuationStatus::Pending);
            EVENTS.with(|events| assert_eq!(*events.borrow(), [42]));
            live(0);
        }
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        live(0);
    }
}

#[test]
fn local_field_borrows_survive_slices_reload_and_independent_task_cancellation() {
    for entry in [
        "field_loop",
        "field_loop_merge",
        "field_jumps",
        "field_recursion",
    ] {
        let source = format!("{SOURCE} fn task() @suspends -> Int32 {{ {entry}() }}");
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.load_source(&source).unwrap();
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        let mut task = vm.call_async("task", &[]).unwrap();
        let mut peak = 0;
        for _ in 0..10000 {
            LIVE.with(|n| peak = peak.max(n.get()));
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42), "{entry}");
        assert_eq!(peak, if entry == "field_recursion" { 43 } else { 1 });
        live(0);

        let mut cancelled = vm.call_async("task", &[]).unwrap();
        for _ in 0..100 {
            if LIVE.with(|n| n.get() == 1) {
                break;
            }
            cancelled.resume().unwrap();
        }
        live(1);
        let mut kept = vm.call_async("task", &[]).unwrap();
        for _ in 0..100 {
            if LIVE.with(|n| n.get() == 2) {
                break;
            }
            kept.resume().unwrap();
        }
        live(2);
        assert!(vm.load_source("fn invalid(").is_err());
        vm.load_source("fn task() @suspends -> Int32 { 99 }")
            .unwrap();
        drop(vm);
        let mut scheduler = Scheduler::new();
        let id = scheduler.suspend(cancelled);
        scheduler.cancel(id).unwrap();
        live(1);
        for _ in 0..10000 {
            if kept.is_finished() {
                break;
            }
            kept.resume().unwrap();
        }
        assert_eq!(kept.cps_result(), Some(42));
        live(0);
    }
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(&format!(
        "{SOURCE} fn task() @suspends -> Int32 {{ field_loop() }}"
    ))
    .unwrap();
    vm.set_interpreter_limits(super::InterpreterLimits {
        instruction_budget: Some(100),
        poll_budget: Some(10),
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async("task", &[]).unwrap();
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

#[test]
fn local_field_borrows_report_storage_failure_without_task_arena_allocation() {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            LOCAL_LIMIT.with(|n| n.set(self.0));
        }
    }
    let _reset = Reset(LOCAL_LIMIT.with(|n| n.replace(0)));
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(SOURCE).unwrap();
    let error = vm.call_int32_0("field_siblings").unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    assert_eq!(
        error.span().unwrap().slice(SOURCE),
        Some("var p = Point { x: 20, y: 20 }")
    );
    live(0);
    let mut vm = VmEngine::with_mode(VmMode::Jit);
    vm.load_source(SOURCE).unwrap();
    assert_eq!(vm.call_int32_0("field_siblings").unwrap(), 42);
    live(0);
}

#[test]
fn local_field_borrows_preserve_imported_alias_and_generic_contracts() {
    let mut sources = crate::ModuleSources::new();
    sources
        .insert(
            "data",
            r#"
        pub struct Stats<T> { pub hp: T, pub mp: T }
        pub type Player = Stats<Int32>
        pub type View = &Int32
        pub fn bump(value: &mut Int32) { (*(value) = (*value) + 1); }
        pub fn view(value: View) -> View { value }
        pub fn first<T: Copy>(value: Stats<T>) -> T { (*(&value.hp)) }
        pub fn allocate(value: View) { alloc((value,)); }
    "#,
        )
        .unwrap();
    let prefix = "import data as d.{bump as increase}\n";
    let source = format!("{prefix} fn f() -> Int32 {{ var p: d.Player = d.Stats {{ hp: 20, mp: 20 }}; let hp = &mut p.hp; increase(&mut p.mp); increase(hp); d.first(p) + p.mp }}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(&source, &sources)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        live(0);
        for body in [
            "fn bad() -> d.View { let p: d.Player = d.Stats { hp: 1, mp: 2 }; d.view(&p.hp) }",
            "fn bad() { let p: d.Player = d.Stats { hp: 1, mp: 2 }; d.allocate(&p.mp); }",
            "fn bad() { let p = d.Stats { hp: (&*(alloc(1))), mp: (&*(alloc(2))) }; d.first(p); }",
        ] {
            let bad = format!("{prefix}{body}");
            let error = vm.load_source_with_modules(&bad, &sources).unwrap_err();
            assert!(
                error.message().contains("local") || error.message().contains("Copy data"),
                "{error}"
            );
            assert_eq!(vm.call_int32_0("f").unwrap(), 42);
            let ast = crate::TypeRegistry::default()
                .resolve_with_modules(&crate::parser::parse(&bad).unwrap(), &sources)
                .unwrap();
            assert!(crate::aot::AotCompiler::new("field-import-rejection")
                .unwrap()
                .compile_module(&ast)
                .is_err());
        }
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &sources)
        .unwrap();
    crate::aot::AotCompiler::new("field-import")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}
