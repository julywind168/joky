use super::{VmEngine, VmMode};
use crate::{
    runtime::arena::{LIMIT, LIVE, MAX_BYTES},
    ErrorCode,
};

const SOURCE: &str = include_str!("../../tests/fixtures/released_arrays.jk");
const ENTRIES: &[&str] = &[
    "released_array_reuse",
    "released_array_growth",
    "released_array_references",
    "released_array_zero_size",
    "released_array_local_effect",
    "released_array_branches",
];

#[test]
fn released_arrays_reuse_storage_and_keep_copied_referents_in_all_vm_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(SOURCE).unwrap();
            for entry in ENTRIES {
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
            for (entry, code) in [
                ("released_array_failure", ErrorCode::RuntimeError),
                ("released_array_growth_failure", ErrorCode::ResourceLimit),
            ] {
                let error = vm.call_int32_0(entry).unwrap_err();
                assert_eq!(error.code(), code, "{mode:?}: {entry}");
                assert!(error.span().is_some());
                LIVE.with(|n| assert_eq!(n.get(), 0));
                assert_eq!(vm.call_int32_0("released_array_local_effect").unwrap(), 42);
            }
        }
    }
}

#[test]
fn released_arrays_refund_exact_header_payload_and_bookkeeping_charges() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let source = r#"
        import joky/array as array
        fn check() -> Int32 {
            var i = 0
            while i < 100 {
                var a = array.with_capacity::<Int32>(1)
                array.push(&mut a, i)
                if array.get(&a, 0).unwrap_or(-1) != i { return 0 }
                array.release(a)
                i = i + 1
            }
            42
        }
        fn growth() -> Int32 {
            var a = array.new::<Int32>()
            var i = 0
            while i < 33 { array.push(&mut a, i) i = i + 1 }
            array.release(a)
            42
        }
    "#;
    let record = std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let header = std::mem::size_of::<crate::runtime::buffer::Buffer>() + record;
    let allocation = header + std::mem::size_of::<i32>() + record;
    // Peak migration requires both old (32) and new (64) payloads and headers.
    let peak = header * 2 + record * 2 + (32 + 64) * std::mem::size_of::<i32>();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let _restore = Restore;
        for (entry, budget) in [("check", allocation), ("growth", peak)] {
            LIMIT.with(|n| n.set(budget));
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
            LIVE.with(|n| assert_eq!(n.get(), 0));
            LIMIT.with(|n| n.set(budget - 1));
            assert_eq!(
                vm.call_int32_0(entry).unwrap_err().code(),
                ErrorCode::ResourceLimit
            );
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

fn rejects(source: &str, message: &str) {
    let ast = crate::parser::parse(source).unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let error = vm.load_module(&ast).expect_err(source);
        assert!(error.message().contains(message), "{source}\n{error}");
        assert!(error.span().is_some());
    }
    let mut aot = crate::aot::AotCompiler::new("invalid-release").unwrap();
    let error = aot.compile_module(&ast).expect_err(source);
    assert!(error.message().contains(message), "{source}\n{error}");
}

#[test]
fn released_arrays_reject_double_release_views_and_live_borrows() {
    let prefix = r#"
        import joky/array as array
        import joky/element as element
        import joky/slice as slices
    "#;
    for (body, message) in [
        ("array.release(a) array.release(a)", "moved"),
        ("array.release(a) array.len(&a);", "moved"),
        ("let moved = a array.release(a)", "moved"),
        ("let r = &a array.release(a) array.len(r);", "live borrow"),
        (
            "let r = array.borrow(&a, 0) array.release(a) element.read(r);",
            "live borrow",
        ),
        (
            "let r = array.slice_mut(&mut a, 0, 1) array.release(a) slices.set(r, 0, 1);",
            "live borrow",
        ),
        ("array.release(&a)", ""),
        ("array.release(array.slice(&a, 0, 1))", ""),
        ("array.release(array.borrow_mut(&mut a, 0))", ""),
        ("if true { array.release(a) } array.release(a)", "moved"),
    ] {
        rejects(&format!("{prefix} fn bad() {{ var a = array.new::<Int32>() array.push(&mut a, 42) {body} }}"), message);
    }
    rejects(&format!("{prefix} fn bad() {{ let p = alloc(42) var a = array.new::<&Int32>() array.push(&mut a, share(p)) array.release(a) memory.write(p, 0) }}"), "retained");
}

#[test]
fn released_arrays_cross_module_aliases_preserve_consumption_without_allocation_effects() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "dispose",
            r#"
        import joky/array as array.{type Array}
        pub type Batch<T> = Array<T>
        pub fn discard<T>(a: Batch<T>) { array.release(a) }
        pub fn update(a: Batch<Int32>, p: &mut Int32) { discard(a) memory.write(p, 42) }
    "#,
        )
        .unwrap();
    let source = r#"
        import joky/array as array
        import dispose as d
        struct Point { flag: Bool, wide: Int64 }
        fn check() -> Int32 {
            var a: d.Batch<Point> = array.new::<Point>()
            array.push(&mut a, Point { flag: true, wide: 4294967297 })
            d.discard(a)
            var n = 0
            let b = array.new::<Int32>()
            d.update(b, &mut n)
            n
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        let bad = "import joky/array as array import dispose as d fn bad() { let a = array.new::<Int32>() d.discard(a) d.discard(a) }";
        assert!(vm
            .load_source_with_modules(bad, &modules)
            .unwrap_err()
            .message()
            .contains("moved"));
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    crate::aot::AotCompiler::new("imported-release")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn released_arrays_pause_before_and_after_release_and_survive_cancellation() {
    use crate::runtime::scheduler::Scheduler;
    let source = r#"
        import joky/array as array
        fn helper() -> Int32 {
            var a = array.with_capacity::<Int32>(1)
            array.push(&mut a, 42)
            let result = array.get(&a, 0).unwrap_or(0)
            array.release(a)
            result
        }
        fn task() @suspends -> Int32 { helper() }
    "#;
    for after_release in [false, true] {
        for outcome in ["finish", "drop", "cancel"] {
            let mut vm = VmEngine::with_mode(VmMode::Interpreter);
            vm.load_source(source).unwrap();
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            let mut saw_allocation = false;
            let mut reached = false;
            for _ in 0..100_000 {
                let live = LIVE.with(|n| n.get());
                saw_allocation |= live == 2;
                if saw_allocation && live == if after_release { 0 } else { 2 } {
                    reached = true;
                    break;
                }
                assert!(!task.is_finished());
                task.resume().unwrap();
            }
            assert!(reached && !task.is_finished());
            vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
            drop(vm);
            match outcome {
                "finish" => {
                    for _ in 0..100_000 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42));
                }
                "drop" => drop(task),
                "cancel" => {
                    let mut scheduler = Scheduler::new();
                    let id = scheduler.suspend(task);
                    scheduler.cancel(id).unwrap();
                }
                _ => unreachable!(),
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
