use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;
use crate::{FunctionSignature, Type};

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}

extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}

const SOURCE: &str = include_str!("../../tests/fixtures/dropped_owners.jk");

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record_drop",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record_drop as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
    vm
}

#[test]
fn drop_hooks_cover_scopes_moves_replacement_and_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for (entry, expected) in [
            ("drop_scope", vec![3, 1]),
            ("drop_moves", vec![1]),
            ("drop_replace", vec![1, 3]),
            ("drop_loops", vec![0, 1, 2]),
            ("drop_failure", vec![2, 1]),
            ("drop_early", vec![2, 1]),
            ("drop_nested", vec![1, 2]),
            ("drop_generic", vec![4]),
            ("drop_discard", vec![5]),
            ("drop_partial", vec![2]),
            ("drop_failing_hook", vec![2, 1]),
            ("drop_failed_return", vec![-1, 2, 1]),
            ("drop_leading_owner", vec![1, 2]),
            ("drop_shadow", vec![2, 1]),
            ("drop_reinitialize", vec![1, 2]),
            ("drop_reinitialize_failure", vec![1, 2]),
            ("drop_branch", vec![2, 1]),
            ("drop_original_failure", vec![2, 1]),
            ("drop_mutated_child", vec![1, 9]),
            ("drop_reuse_budget", vec![]),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let result = vm.call_int32_0(entry);
            if [
                "drop_failure",
                "drop_partial",
                "drop_failing_hook",
                "drop_failed_return",
                "drop_original_failure",
                "drop_reinitialize_failure",
            ]
            .contains(&entry)
            {
                assert!(result.is_err());
            } else {
                assert_eq!(result.as_ref().unwrap(), &42);
            }
            if entry == "drop_original_failure" {
                assert!(result.unwrap_err().message().contains("Shift"));
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected, "{mode:?}: {entry}"));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn drop_hooks_survive_internal_pause_reload_and_cancellation() {
    for outcome in ["finish", "cancel"] {
        let mut vm = vm(VmMode::Interpreter);
        EVENTS.with(|events| events.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        let mut task = vm.call_async("drop_task", &[]).unwrap();
        for _ in 0..1000 {
            if LIVE.with(|n| n.get() >= 3) {
                break;
            }
            task.resume().unwrap();
        }
        assert!(!task.is_finished());
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        if outcome == "finish" {
            for _ in 0..10000 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42));
            EVENTS.with(|events| assert_eq!(*events.borrow(), vec![3, 1]));
        } else {
            drop(task);
            EVENTS.with(|events| assert!(!events.borrow().is_empty()));
        }
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn drop_transfers_are_atomic_at_every_interpreter_pause() {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut reached_completion = false;
    for pause in 0..2000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("drop_transition_task", &[]).unwrap();
        for _ in 0..pause {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        reached_completion = task.is_finished();
        drop(task);
        EVENTS.with(|events| {
            let events = events.borrow();
            for id in 1..=6 {
                let drops = events.iter().filter(|&&event| event == id).count();
                assert!(drops <= 1, "pause {pause}, duplicate drop: {events:?}");
                if events.contains(&-id) {
                    assert_eq!(drops, 1, "pause {pause}, lost owner: {events:?}");
                }
            }
        });
        LIVE.with(|n| assert_eq!(n.get(), 0, "pause {pause}"));
        if reached_completion {
            break;
        }
    }
    assert!(reached_completion);
}

#[test]
fn drop_contracts_reject_explicit_calls_and_partial_moves() {
    let prefix = "import joky/array.{type Array} import joky/drop.{type Drop} struct R { values: Array<Int32> } impl Drop for R { fn drop(&mut self) {} } fn make() -> R { R { values: Array.new::<Int32>() } }";
    let cases = [
        format!("{prefix} fn bad() {{ var r = make() r.drop() }}"),
        format!("{prefix} fn bad() {{ var r = make() Drop.drop(&mut r) }}"),
        format!("{prefix} fn bad() {{ var r = make() let f = R.drop f(&mut r) }}"),
        format!("{prefix} fn bad() {{ let r = make() let v = r.values }}"),
        format!("{prefix} fn bad() {{ let v = make().values }}"),
        format!("{prefix} fn bad() {{ var r = make() r.values = Array.new::<Int32>() }}"),
        format!("{prefix} struct Outer {{ inner: R }} fn bad() {{ var r = Outer {{ inner: make() }} r.inner.values = Array.new::<Int32>() }}"),
        "import joky/drop.{type Drop} struct R { id: Int32 } impl Drop for R { fn drop(&mut self) {} }".into(),
        "import joky/drop.{type Drop} import joky/array.{type Array} struct R { a: Array<Int32> } impl Drop for R { fn drop(&self) {} }".into(),
        "import joky/drop.{type Drop} import joky/array.{type Array} struct R { a: Array<Int32> } impl Drop for R { fn drop(&mut self) -> Int32 { 1 } }".into(),
        "import joky/drop.{type Drop} import joky/array.{type Array} struct R { a: Array<Int32> } impl Drop for R { fn drop(&mut self) @suspends {} }".into(),
        "import joky/drop.{type Drop} import joky/array.{type Array} trait Bound { fn value(&self) } struct R<T> { a: Array<T> } impl<T: Bound> Drop for R<T> { fn drop(&mut self) {} }".into(),
    ];
    for source in cases {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let error = vm.load_source(&source).expect_err(&source);
            assert!(error.span().is_some(), "{source}: {error}");
        }
        let ast = crate::parser::parse(&source).unwrap();
        let mut compiler = crate::aot::AotCompiler::new("invalid-drop").unwrap();
        assert!(compiler.compile_module(&ast).is_err(), "{source}");
    }
}

#[test]
fn user_trait_named_drop_is_ordinary_and_builtin_needs_no_prelude() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source("trait Drop { fn drop(&self) -> Int32 } struct R { id: Int32 } impl Drop for R { fn drop(&self) -> Int32 { self.id } } fn check() -> Int32 { let r = R { id: 42 } r.drop() }").unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        unsafe {
            vm.register_fn(
                "record_drop",
                FunctionSignature::new(vec![Type::Int32], Type::Unit),
                record_drop as *const u8,
            )
            .unwrap();
        }
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("drop_nested").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), vec![1, 2]));
    }
}

#[test]
fn drop_hooks_preserve_private_generic_definition_scope() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "resource",
            r#"
        import joky/array.{type Array}
        import joky/drop.{type Drop as Cleanup}
        extern fn record_drop(value: Int32)
        fn report(id: Int32) { record_drop(id) }
        pub struct Owned<T> { private id: Int32 private values: Array<T> }
        impl<T> Cleanup for Owned<T> { fn drop(&mut self) { report(self.id) } }
        pub fn make<T>(id: Int32) -> Owned<T> { Owned { id: id, values: Array.new::<T>() } }
    "#,
        )
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        vm.load_source_with_modules(
            "import resource fn check() -> Int32 { let r = resource.make::<Int32>(7) 42 }",
            &modules,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), vec![7]));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn recursive_drop_is_bounded_and_releases_every_buffer() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"
            import joky/array.{type Array}
            import joky/drop.{type Drop}
            struct R { values: Array<Int32> }
            impl Drop for R { fn drop(&mut self) { let next = make() } }
            fn make() -> R { R { values: Array.new::<Int32>() } }
            fn check() -> Int32 { let value = make() 42 }
        "#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let error = vm.call_int32_0("check").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        assert!(error.message().contains("Destructor nesting"));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn interpreter_drop_cannot_bypass_instruction_limits() {
    let mut vm = vm(VmMode::Interpreter);
    vm.load_source(&format!(
        "{SOURCE} struct Looping {{ values: Array<Int32> }} impl Drop for Looping {{ fn drop(&mut self) {{ while true {{}} }} }} fn check() -> Int32 {{ let first = resource(1) let second = Looping {{ values: Array.new::<Int32>() }} 42 }}"
    )).unwrap_or_else(|e| panic!("{e}"));
    EVENTS.with(|events| events.borrow_mut().clear());
    let error = vm.call_int32_0("check").unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    EVENTS.with(|events| assert_eq!(*events.borrow(), vec![1]));
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn drop_runs_at_every_interpreter_instruction_limit() {
    let mut vm = vm(VmMode::Interpreter);
    let mut completed = false;
    for budget in 1..2000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match vm.call_int32_0("drop_transitions") {
            Ok(value) => {
                assert_eq!(value, 42);
                completed = true;
            }
            Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
        }
        EVENTS.with(|events| {
            let events = events.borrow();
            for id in 1..=6 {
                let drops = events.iter().filter(|&&event| event == id).count();
                assert!(drops <= 1, "budget {budget}: {events:?}");
                if events.contains(&-id) {
                    assert_eq!(drops, 1, "budget {budget}: {events:?}");
                }
            }
        });
        LIVE.with(|n| assert_eq!(n.get(), 0, "budget {budget}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}
