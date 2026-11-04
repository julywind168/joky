//! Suspending function values preserve effects, CPS metadata and code ownership.
use super::{support::*, InterpreterLimits, VmMode};
use crate::runtime::{arena::LIVE, suspension::ContinuationStatus};

const SOURCE: &str = include_str!("../../../../tests/fixtures/suspending_function_values.jk");
const ENTRIES: &[&str] = &[
    "suspending_basic",
    "suspending_storage",
    "suspending_order",
    "suspending_aggregates",
    "suspending_scope",
    "suspending_nested_scope",
    "suspending_recursive",
    "suspending_reload",
];

#[test]
fn suspending_function_values_preserve_storage_layouts_snapshots_and_recursion() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let vm = parity_vm_with_prelude(SOURCE, mode, prelude);
            for entry in ENTRIES {
                READY.with(|ready| ready.set(false));
                EVENTS.with(|events| events.borrow_mut().clear());
                let mut task = vm.call_async(entry, &[]).unwrap();
                let initial = EVENTS.with(|events| events.borrow().clone());
                for _ in 0..3 {
                    assert_eq!(
                        task.status(),
                        ContinuationStatus::Pending,
                        "{mode:?}: {entry}"
                    );
                    task.resume().unwrap();
                    EVENTS.with(|events| assert_eq!(*events.borrow(), initial));
                }
                READY.with(|ready| ready.set(true));
                for _ in 0..100 {
                    if task.is_finished() {
                        break;
                    }
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42), "{mode:?}: {entry}");
                if *entry == "suspending_order" {
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 40, 41, 43]));
                }
                if *entry == "suspending_scope" {
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 101, 1, 302, 201]));
                }
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}

#[test]
fn suspending_function_values_cancel_race_losers_and_propagate_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let task = vm.call_async("suspending_race", &[]).unwrap();
        assert_eq!(task.cps_result(), Some(42));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [101, 1, 302]));
        for entry in ["suspending_failure", "suspending_scope_failure"] {
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async(entry, &[]).unwrap();
            task.resume().unwrap();
            READY.with(|ready| ready.set(true));
            assert!(task.resume().is_err(), "{mode:?}: {entry}");
            assert!(matches!(task.status(), ContinuationStatus::Failed(_)));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn suspending_function_values_keep_old_code_across_reload_drop_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(SOURCE, mode);
        let mut old = vm.call_async("suspending_reload", &[]).unwrap();
        let cancelled = vm.call_async("suspending_reload", &[]).unwrap();
        assert!(vm
            .load_source("fn bad(f: fn() @suspends -> Int32) -> Int32 { f() }")
            .is_err());
        vm.load_source(&SOURCE.replace("value + 1", "value + 10"))
            .unwrap();
        let mut new = vm.call_async("suspending_reload", &[]).unwrap();
        assert_ne!(old.module_version(), new.module_version());
        drop(vm);
        drop(cancelled);
        READY.with(|ready| ready.set(true));
        old.resume().unwrap();
        new.resume().unwrap();
        assert_eq!(old.cps_result(), Some(42));
        assert_eq!(new.cps_result(), Some(51));
        drop((old, new));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn suspending_function_values_obey_interpreter_poll_and_frame_budgets() {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(5),
        ..Default::default()
    })
    .unwrap();
    READY.with(|ready| ready.set(true));
    for entry in [
        "suspending_order",
        "suspending_recursive",
        "suspending_scope",
    ] {
        let mut task = vm.call_async(entry, &[]).unwrap();
        for _ in 0..1000 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42), "{entry}");
    }
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 4,
        ..Default::default()
    })
    .unwrap();
    assert!(vm.call_async("suspending_recursive", &[]).is_err());
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn suspending_function_values_reject_effect_erasure_and_invalid_boundaries() {
    for (source, message) in [
        ("fn bad(f: fn() @suspends -> Int32) -> Int32 { f() }", "@suspends"),
        ("fn a() @suspends -> Int32 { 42 } fn bad() { let f: fn() -> Int32 = a }", "mismatch"),
        ("fn a() -> Int32 { 42 } fn bad() { let f: fn() @suspends -> Int32 = a }", "mismatch"),
        ("fn a() @suspends -> Int32 { 42 } fn bad() @suspends { var f = a; f = bad; }", "mismatch"),
        ("fn same<T>(a: T, b: T) {} fn a() @suspends {} fn b() {} fn bad() { same(a, b) }", "expected fn() @suspends -> Unit, got fn() -> Unit"),
        ("fn apply<T>(f: fn(T) -> T, x: T) -> T { f(x) } fn a(x: Int32) @suspends -> Int32 { x } fn bad() { apply(a, 1); }", "mismatch"),
        ("fn bad<T>(x: T, f: fn(T) @suspends -> T) -> T { f(x) }", "@suspends"),
        ("extern fn delayed_value(x: Int32) @suspends -> Int32 fn bad() { let f = delayed_value }", "language function"),
        ("extern fn host(f: fn() @suspends -> Int32)", "host ABI"),
        ("struct Holder { f: fn() @suspends -> Int32 } unsafe extern \"C\" fn host(x: &Holder)", "host ABI"),
        ("fn a() {} fn bad() @suspends { let f = a; race { | f() } }", "@suspends"),
        ("fn bad(f: fn(&mut Int32) @suspends -> Unit) @suspends -> Int32 { let p = alloc(1); f(p) ; (*p) }", "borrow"),
        ("trait Mark {} fn bounded<T: Mark>(x: T) @suspends -> T { x } fn bad() { let f = bounded::<Int32> }", "does not satisfy"),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = parity_vm("fn previous() -> Int32 { 42 }", mode);
            let error = vm.load_source(source).expect_err(source);
            assert!(error.message().to_lowercase().contains(&message.to_lowercase()), "{mode:?}: {error}\n{source}");
            assert_eq!(vm.call_int32_0("previous").unwrap(), 42);
        }
        let error = crate::aot::AotCompiler::new("bad-suspending-value").unwrap()
            .compile_module(&crate::parser::parse(source).unwrap()).expect_err(source);
        assert!(error.message().to_lowercase().contains(&message.to_lowercase()), "{error}\n{source}");
    }
    for source in [
        "fn bad(f: fn() @unknown -> Int32) {}",
        "fn bad(f: fn() @suspends @suspends -> Int32) {}",
    ] {
        assert!(crate::parser::parse(source).is_err());
    }
}

#[test]
fn suspending_function_values_preserve_alias_effects_and_private_import_scopes() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("helpers", "pub type Async<T> = fn(T) @suspends -> T fn hidden(x: Int32) @suspends -> Int32 { x + 1 } pub fn get() -> Async<Int32> { hidden } pub fn identity<T>(x: T) @suspends -> T { x }").unwrap();
    let source = "import helpers fn main() @suspends -> Int32 { let get = helpers.get; let f = get(); let same = helpers.identity::<helpers.Async<Int32>>; let value = same(f); value(41) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_async("main", &[]).unwrap().cps_result(), Some(42));
        assert!(vm
            .load_source_with_modules(
                "import helpers fn bad() { let f = helpers.hidden }",
                &modules
            )
            .is_err());
    }
    let mut compiler = crate::aot::AotCompiler::new("imported-suspending-value").unwrap();
    let module = crate::TypeRegistry::new()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    compiler.compile_module(&module).unwrap();
}
