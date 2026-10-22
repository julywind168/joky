//! Local place identity, borrow lifetimes and stable activation storage.
use super::{VmEngine, VmMode};
use crate::runtime::{
    arena::{LIVE, LOCAL_LIMIT},
    scheduler::Scheduler,
    suspension::{ContinuationStatus, ResumeError},
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/local_borrows.jk");
const CASES: &[&str] = &[
    "local_record",
    "local_wide",
    "local_nested",
    "local_enum_tuple",
    "local_snapshot",
    "local_shadow",
    "local_reborrow",
    "local_branches",
    "local_loop",
    "local_jumps",
    "local_early_return",
    "local_recursion",
    "local_empty",
    "local_no_arena_contract",
];

fn live(expected: usize) {
    LIVE.with(|n| assert_eq!(n.get(), expected));
}

#[test]
fn local_borrows_read_and_update_the_original_binding() {
    let prefix = r#"
        struct Point { x: Int32 }
        fn increase(p: &mut Point) { p.x = p.x + 1; }
        fn bump(p: &mut Int32) { (*(p) = (*p) + 1); }
        fn view(p: &Point) -> &Point { p }
        fn copy<T: Copy>(value: T) -> T { (*(&value)) }
    "#;
    for body in [
        "var p = Point { x: 41 }; increase(&mut p); p.x",
        "var p = 41; bump(&mut p); p",
        "let p = 42; (*(&p))",
        "var p = Point { x: 20 }; let old = p; increase(&mut p); p.x = p.x + 1; old.x + p.x",
        "var p = Point { x: 41 }; let q = view(&p); let n = q.x; increase(&mut p); n + p.x - 41",
        "copy(Point { x: 42 }).x",
        "var p = 0; var i = 0; while i < 42 { bump(&mut p); i = i + 1; }; p",
        "var sum = 0; var i = 0; while i < 3 { var p = 13; bump(&mut p); sum = sum + p; i = i + 1; }; sum",
        "let p = (42, true); let (n, yes) = (*(&p)); if yes { n } else { 0 }",
        "let value: Option<Int32> = Some(42); match value { Some(n) => (*(&n)), None => 0 }",
        "var (a, b) = (20, 20); bump(&mut a); bump(&mut b); a + b",
        "let u = {}; (*(&u)); 42",
        "var p = 41; if true { bump(&mut p); }; p",
        "var p = 41; if false { bump(&mut p); } else { p = 42; }; p",
        "let a = 42; let b = 42; let r = if true { &a } else { &b }; (*r)",
        "if (let x = 42; (*(&x)) == 42) { 42 } else { 0 }",
        "if true && (let x = 42; (*(&x)) == 42) { 42 } else { 0 }",
        "let a = 63; let b = 42; a &\n b",
    ] {
        let source = format!("{prefix} fn f() -> Int32 {{ {body} }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap_or_else(|e| panic!("{mode:?}: {e}\n{source}"));
            assert_eq!(vm.call_int32_0("f").unwrap(), 42, "{mode:?}: {body}");
        }
    }
}

#[test]
fn local_borrows_reject_conflicts_escapes_and_unsupported_places() {
    let prefix = r#"
        struct Point { x: Int32 }
        struct Holder { view: &Int32 }
        fn same(p: &Int32) -> &Int32 { p }
        fn pair(a: &mut Int32, b: &Int32) {}
        fn allocate(p: &Int32) { alloc(Holder { view: p }); }
        fn indirect(p: &Int32) { allocate(p); }
        fn store<T: Copy>(p: &T) { alloc((p,)); }
        fn copy<T: Copy>(value: T) -> T { (*(&value)) }
    "#;
    for (body, message) in [
        ("fn f() { var x = 1; let r = &x; x = 2; (*r); }", "borrow"),
        ("fn f() { var x = 1; let r = &mut x; x; (*(r) = 2); }", "borrow"),
        ("fn f() { var x = 1; pair(&mut x, &x); }", "borrow"),
        ("fn f() { var p = Point { x: 1 }; let r = &p; p.x = 2; r.x; }", "borrow"),
        ("fn f() { var p = Point { x: 1 }; let r = &mut p; p.x; r.x = 2; }", "borrow"),
        ("fn f() -> &Int32 { let x = 42; &x }", "local"),
        ("fn f() -> &Int32 { let x = 42; return &x }", "local"),
        ("fn f(x: Int32) -> &Int32 { &x }", "local"),
        ("fn f() -> Option<&Int32> { let x = 42; Some(&x) }", "local"),
        ("fn f() -> Holder { let x = 42; Holder { view: &x } }", "local"),
        ("fn f(p: &Int32, condition: Bool) -> &Int32 { let x = 42; if condition { p } else { &x } }", "local"),
        ("fn f(condition: Bool) { var a = 1; var b = 2; let r = if condition { &a } else { &b }; a = 3; (*r); }", "borrow"),
        ("fn f(condition: Bool) { var a = 1; var b = 2; let r = if condition { &mut a } else { &mut b }; b; (*(r) = 3); }", "borrow"),
        ("fn f() -> Int32 { let value: Option<Int32> = Some(42); let r = match value { Some(x) => &x, None => (&*(alloc(0))) }; (*r) }", "local"),
        ("fn f() -> Int32 { let r = { let x = 42; same(&x) }; (*r) }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); if (let x = 42; r = &x; true) { (*r); } }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); while (let x = 42; r = &x; false) {}; (*r); }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); true && (let x = 42; r = &x; true); (*r); }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); match (let x = 42; r = &x; Some(1)) { Some(_) => (*r), None => 0 }; }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); graph { let x = 42; r = &x; let node = graph.alloc(Point { x: 1 }); node }; (*r); }", "local"),
        ("fn f() { let x = 42; alloc(Holder { view: &x }); }", "retained"),
        ("fn f() { let x = 42; let holder = alloc(Holder { view: (&*(alloc(1))) }); holder.view = &x; }", "retained"),
        ("fn f() { let x = 42; allocate(&x); }", "retained"),
        ("fn f() { let x = 42; indirect(&x); }", "retained"),
        ("fn f() { let x = 42; store(&x); }", "retained"),
        ("fn f() { var r: &Int32 = alloc(0); while true { let x = 42; r = &x; break; }; (*r); }", "local"),
        ("fn f() { var r: &Int32 = alloc(0); var i = 0; while i < 2 { let x = 42; r = &x; i = i + 1; continue; }; (*r); }", "local"),
        ("fn f() { let x = 42; (*(&mut x) = 1); }", "var binding"),
        ("fn f() { &42; }", "temporary borrowing"),
        ("fn f() { let x = \"hello\"; let r = &x; x.release(); r.byte_length(); }", "borrowed"),
        ("fn f() { let p = alloc(42); &p; }", "Copy data"),
        ("fn f() { let x = 42; let h = Holder { view: &x }; &h; }", "Copy data"),
        ("fn f() { let x = alloc(42); copy((&*x)); }", "Copy data"),
    ] {
        let source = format!("{prefix}{body}");
        let error = super::support::frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{error}\n{source}");
        assert!(error.span().is_some());
    }
}

#[test]
fn local_borrows_preserve_layout_snapshots_control_flow_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        for _ in 0..3 {
            for name in CASES {
                assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
                live(0);
            }
            for name in ["local_failure", "local_mixed_failure"] {
                let error = vm.call_int32_1(name, 0).unwrap_err();
                assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
                assert_eq!(error.span().unwrap().slice(SOURCE), Some("(*r) / divisor"));
                live(0);
                assert_eq!(vm.call_int32_0("local_record").unwrap(), 42);
            }
        }
    }
}

#[test]
fn local_borrows_release_callee_storage_before_a_later_suspension() {
    use super::support::*;
    let source = format!("{SOURCE} extern fn delayed_value(x: Int32) @suspends -> Int32 fn task() @suspends -> Int32 {{ delayed_value(local_loop()) - 1 }}");
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
        READY.with(|r| r.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        live(0);
    }
}

#[test]
fn local_borrows_interpreter_slices_cancel_reload_and_exhaustion_keep_places_valid() {
    for entry in ["local_loop", "local_jumps", "local_recursion"] {
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
        assert_eq!(peak, if entry == "local_recursion" { 43 } else { 1 });
        live(0);

        let mut pending = vm.call_async("task", &[]).unwrap();
        for _ in 0..100 {
            if LIVE.with(|n| n.get() > 0) {
                break;
            }
            pending.resume().unwrap();
        }
        assert!(LIVE.with(|n| n.get() > 0));
        assert!(vm.load_source("fn invalid(").is_err());
        vm.load_source("fn task() @suspends -> Int32 { 99 }")
            .unwrap();
        drop(vm);
        pending.resume().unwrap();
        let mut scheduler = Scheduler::new();
        let id = scheduler.suspend(pending);
        scheduler.cancel(id).unwrap();
        live(0);
    }
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(&format!(
        "{SOURCE} fn task() @suspends -> Int32 {{ local_work(10000) }}"
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
fn local_borrows_bound_frame_storage_and_report_allocation_failure() {
    let fields = (0..64)
        .map(|i| format!("f{i}: Int64"))
        .collect::<Vec<_>>()
        .join(", ");
    let init = (0..64)
        .map(|i| format!("f{i}: 0"))
        .collect::<Vec<_>>()
        .join(", ");
    let locals = (0..128)
        .map(|i| format!("let x{i} = seed; (*(&x{i}));"))
        .collect::<Vec<_>>()
        .join(" ");
    let source =
        format!("struct Large {{ {fields} }} fn f() {{ let seed = Large {{ {init} }}; {locals} }}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = VmEngine::with_mode(mode).load_source(&source).unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit, "{error}");
        assert!(error.message().contains("64 KiB"), "{error}");
    }
    let error = crate::aot::AotCompiler::new("local-budget")
        .unwrap()
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    assert!(error.message().contains("64 KiB"));

    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            LOCAL_LIMIT.with(|n| n.set(self.0));
        }
    }
    let _reset = Reset(LOCAL_LIMIT.with(|n| n.replace(0)));
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(SOURCE).unwrap();
    let error = vm.call_int32_0("local_record").unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    assert_eq!(
        error.span().unwrap().slice(SOURCE),
        Some("var point = Point { x: 41 }")
    );
    live(0);
    let mut native = VmEngine::with_mode(VmMode::Jit);
    native.load_source(SOURCE).unwrap();
    assert_eq!(native.call_int32_0("local_record").unwrap(), 42);
    live(0);
}

#[test]
fn local_borrows_imports_and_aliases_preserve_local_origins() {
    let mut sources = crate::ModuleSources::new();
    sources
        .insert(
            "data",
            r#"
        pub struct Point { pub x: Int32 }
        pub type View = &Point
        pub fn update(p: &mut Point) { p.x = p.x + 1; }
        pub fn same(p: View) -> View { p }
        pub fn echo<T: Copy>(value: T) -> T { (*(&value)) }
        pub fn allocate<T: Copy>(value: &T) { alloc((value,)); }
    "#,
        )
        .unwrap();
    let prefix = "import data as d.{update as change, same as view}\n";
    let source = format!("{prefix} fn f() -> Int32 {{ var p = d.Point {{ x: 41 }}; change(&mut p); let r: d.View = view(&p); d.echo(r.x) }}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(&source, &sources).unwrap();
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        live(0);
        for body in [
            "fn bad() -> d.View { let p = d.Point { x: 1 }; view(&p) }",
            "fn bad() { let p = d.Point { x: 1 }; d.allocate(&p); }",
        ] {
            let bad = format!("{prefix}{body}");
            let error = vm.load_source_with_modules(&bad, &sources).unwrap_err();
            assert_eq!(error.code(), crate::ErrorCode::InvalidAssignment, "{error}");
            assert!(error.message().contains("local"));
            assert_eq!(vm.call_int32_0("f").unwrap(), 42);
            let ast = crate::TypeRegistry::default()
                .resolve_with_modules(&crate::parser::parse(&bad).unwrap(), &sources)
                .unwrap();
            assert!(crate::aot::AotCompiler::new("local-import-rejection")
                .unwrap()
                .compile_module(&ast)
                .is_err());
        }
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &sources)
        .unwrap();
    crate::aot::AotCompiler::new("local-imports")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn cps_local_borrows_keep_places_across_resume_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../../../tests/fixtures/cps_local_borrows.jk"
        ))
        .unwrap();
        let mut task = vm.call_async("exercise", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        let mut resumes = 0;
        while !task.is_finished() {
            task.resume().unwrap();
            resumes += 1;
            assert!(resumes < 100);
        }
        assert!(resumes >= 6, "{mode:?}: {resumes}");
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        live(0);
    }
}

fn cps_local_cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(include_str!(
        "../../../../tests/fixtures/cps_local_borrows.jk"
    ))
    .unwrap();
    super::support::assert_cancellation(
        &mut vm,
        "exercise",
        2000,
        sweep,
        || {},
        || LIVE.with(|live| live.get()),
        || live(0),
    );
}

#[test]
fn cps_local_borrows_cancel_at_resource_boundaries() {
    cps_local_cancellation(super::support::CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel at every CPS local instruction"]
fn exhaustive_cps_local_borrows_cancel_at_every_instruction() {
    cps_local_cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn reference_flow_snapshots_preserve_branches_and_loop_fixed_points() {
    let source = r#"
        fn choose(condition: Bool) -> Int32 {
            var a = 20; var b = 22; var view = &a;
            if condition { view = &b; } else { view = &a; }
            let result = (*view);
            a = 21; b = 21;
            result + if condition { 20 } else { 22 }
        }
        fn repeated() -> Int32 {
            let a = 20; let b = 22; var view = &a; var i = 0;
            while i < 4 {
                if i == 0 || i == 2 { view = &b; } else { view = &a; }
                i = i + 1;
            }
            (*view) + 22
        }
        fn test() -> Int32 { choose(true) + choose(false) + repeated() - 84 }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
        let invalid = r#"
            fn invalid(condition: Bool) -> Int32 {
                var a = 1; var b = 2; var view = &a; var i = 0;
                while i < 2 { if condition { view = &b; }; i = i + 1; }
                b = 3;
                (*view)
            }
        "#;
        let error = vm.load_source(invalid).unwrap_err();
        assert!(error.message().contains("borrow"), "{error}");
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
        let error = crate::aot::AotCompiler::new("flow-conflict")
            .unwrap()
            .compile_module(&crate::parser::parse(invalid).unwrap())
            .unwrap_err();
        assert!(error.message().contains("borrow"), "{error}");
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "test",
        42,
    );
}
