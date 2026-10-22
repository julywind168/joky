use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode};

const SOURCE: &str = include_str!("../../tests/fixtures/borrowed_arrays.jk");
const ENTRIES: &[&str] = &[
    "borrowed_array_elements",
    "borrowed_array_ranges",
    "borrowed_array_empty",
    "borrowed_array_references",
    "borrowed_array_aggregates",
    "borrowed_array_enums",
    "borrowed_array_zero_size",
    "borrowed_array_loop",
];
const FAILURES: &[&str] = &[
    "borrowed_array_bad_element",
    "borrowed_array_bad_range",
    "borrowed_array_bad_end",
    "borrowed_array_bad_index_max",
    "borrowed_array_bad_subslice",
    "borrowed_array_bad_slice_element",
];

#[test]
fn borrowed_arrays_keep_storage_live_and_release_loans_at_last_use() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source(SOURCE)
                .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
            for entry in ENTRIES {
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
            for entry in FAILURES {
                let error = vm.call_int32_0(entry).unwrap_err();
                assert_eq!(error.code(), ErrorCode::RuntimeError, "{mode:?}: {entry}");
                let span = error.span().unwrap();
                assert_eq!(
                    error.sources().get(span.source_id()).unwrap().name(),
                    "joky/internal/buffer.jk"
                );
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}

fn rejects(source: &str, message: &str) {
    let error = super::support::frontend_error(source, &crate::ModuleSources::new());
    assert!(error.message().contains(message), "{source}\n{error}");
    assert!(error.span().is_some());
}

const PREFIX: &str = r#"
    import joky/array as array.{type Array}
    import joky/element as element.{type Element}
    import joky/slice as slices.{type Slice}
    fn consume(a: Array<Int32>) {}
    fn at<T: Copy>(a: &Array<T>) -> &Element<T> { array.borrow(a, 0) }
    fn part<T>(a: &mut Array<T>) -> &mut Slice<T> { array.slice_mut(a, 0, 1) }
    fn choose(a: &Array<Int32>, b: &Array<Int32>, flag: Bool) -> &Element<Int32> {
        if flag { array.borrow(a, 0) } else { array.borrow(b, 0) }
    }
"#;

#[test]
fn borrowed_arrays_reject_conflicting_mutation_moves_and_parent_access() {
    for body in [
        "let r = at(&a); array.push(&mut a, 7); element.read(r);",
        "let r = at(&a); array.pop(&mut a); element.read(r);",
        "let r = at(&a); array.set(&mut a, 0, 7); element.read(r);",
        "let r = at(&a); array.reserve(&mut a, 0); element.read(r);",
        "let r = at(&a); consume(a); element.read(r);",
        "let r = at(&a); a = array.new::<Int32>(); element.read(r);",
        "let r = array.borrow_mut(&mut a, 0); array.get(&a, 0); element.write(r, 7)",
        "element.write(array.borrow_mut(&mut a, 0), array.get(&a, 0).unwrap_or(0))",
        "slices.set(array.slice_mut(&mut a, 0, 1), 0, array.get(&a, 0).unwrap_or(0));",
        "let r = array.slice_mut(&mut a, 0, 1); element.write(slices.borrow_mut(r, 0), slices.get(r, 0).unwrap_or(0))",
        "let r = array.borrow_mut(&mut a, 0); let s = array.borrow_mut(&mut a, 1); element.write(r, 7); element.write(s, 9)",
        "let r = array.slice(&a, 0, 1); array.push(&mut a, 7); slices.len(r);",
        "let r = array.slice(&a, 0, 0); consume(a); slices.len(r);",
        "let r = part(&mut a); array.get(&a, 0); slices.set(r, 0, 7);",
        "let r = part(&mut a); let child = slices.borrow_mut(r, 0); slices.get(r, 0); element.write(child, 7)",
        "let r = part(&mut a); let child = slices.slice(r, 0, 1); slices.set(r, 0, 7); slices.len(child);",
        "let r = array.borrow_mut(&mut a, 0); let shared = (&*r); element.write(r, 7); element.read(shared);",
        "var b = array.new::<Int32>(); array.push(&mut b, 9); let r = choose(&a, &b, true); array.push(&mut b, 7); element.read(r);",
        "let r = { var b = array.new::<Int32>(); array.push(&mut b, 7); at(&b) }; element.read(r);",
        "let r = { var b = array.new::<Int32>(); array.push(&mut b, 7); array.slice(&b, 0, 1) }; slices.len(r);",
    ] {
        let source = format!("{PREFIX} fn bad() {{ var a = array.new::<Int32>(); array.push(&mut a, 42); {body} }}");
        rejects(&source, "live borrow");
    }
    for source in [
        "fn bad(a: Array<Int32>) -> &Element<Int32> { at(&a) }",
        "fn bad(a: Array<Int32>) -> &Slice<Int32> { array.slice(&a, 0, 1) }",
        "fn bad(a: &Array<Int32>) -> &mut Element<Int32> { array.borrow_mut(a, 0) }",
    ] {
        rejects(&format!("{PREFIX}{source}"), "");
    }
    rejects(&format!("{PREFIX} fn bad(a: Array<Int32>, other: &Array<Int32>) -> &Element<Int32> {{ at(&a) }}"), "live borrow");
    rejects(&format!("{PREFIX} fn bad(a: Array<Int32>, other: &Array<Int32>) -> &Slice<Int32> {{ array.slice(&a, 0, 1) }}"), "live borrow");
    rejects(&format!("{PREFIX} fn bad(a: &mut Array<Int32>) {{ let r = array.borrow_mut(a, 0); let moved = r; element.write(r, 0) }}"), "moved");
}

#[test]
fn borrowed_arrays_preserve_storage_restrictions_through_element_and_slice_writes() {
    for (body, message) in [
        ("let x = 42; let item = array.borrow_mut(&mut a, 0); element.write(item, &x)", "local borrow"),
        ("let x = 42; let region = array.slice_mut(&mut a, 0, 1); slices.set(region, 0, &x);", "local borrow"),
        ("let p = alloc(42); let item = array.borrow_mut(&mut a, 0); element.write(item, (&*p)) ; (*(p) = 0)", "retained"),
        ("let p = alloc(42); let region = array.slice_mut(&mut a, 0, 1); slices.set(region, 0, (&*p)); array.pop(&mut a); (*(p) = 0)", "retained"),
    ] {
        rejects(&format!("{PREFIX} fn bad() {{ var a = array.new::<&Int32>(); array.push(&mut a, (&*(alloc(0)))); {body} }}"), message);
    }
}

#[test]
fn borrowed_arrays_do_not_expose_owning_values_storage_or_host_capabilities() {
    for (source, message) in [
        ("fn bad(a: Element<Int32>) {}", "borrow-only"),
        ("fn bad(a: Slice<Int32>) {}", "borrow-only"),
        ("fn bad() -> Slice<Int32> { bad() }", "borrow-only"),
        (
            "struct Stored { item: &Element<Int32> } fn bad() {}",
            "Copy value",
        ),
        (
            "struct Stored { items: &Slice<Int32> } fn bad() {}",
            "Copy value",
        ),
        (
            "fn bad(a: &Element<Int32>) { let pair = (a, 1) }",
            "copyable",
        ),
        (
            "fn id<T>(p: T) -> T { p } fn bad(a: &Slice<Int32>) { id(a); }",
            "Copy",
        ),
        ("fn bad(a: Array<&Element<Int32>>) {}", "Copy"),
        ("fn bad(a: Array<&Slice<Int32>>) {}", "Copy"),
        ("fn bad(a: &Element<Int32>) { alloc(a); }", "Copy"),
        ("fn bad(a: &Element<Int32>) { (*a); }", "value target"),
        (
            "fn bad(a: &mut Slice<Int32>) { (*(a) = 0); }",
            "value target",
        ),
        ("extern fn host(a: &Slice<Int32>) fn bad() {}", "host ABI"),
        (
            "extern fn host() -> &Element<Int32> fn bad() {}",
            "host ABI",
        ),
        (
            "unsafe extern \"C\" fn host(a: &Element<Int32>) fn bad() {}",
            "host ABI",
        ),
        (
            "fn bad(a: &Array<Int32>) { array.storage.borrow(a, 0); }",
            "Undeclared function",
        ),
    ] {
        rejects(&format!("{PREFIX}{source}"), message);
    }
}

#[test]
fn borrowed_arrays_element_borrows_do_not_allocate_and_slice_headers_are_charged() {
    use crate::runtime::arena::{LIMIT, MAX_BYTES};
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let source = r#"
        import joky/array as array
        import joky/element as element
        import joky/slice as slices
        fn direct() -> Int32 {
            var a = array.with_capacity::<Int32>(1)
            array.push(&mut a, 0)
            var i = 0
            while i < 42 {
                let item = array.borrow_mut(&mut a, 0)
                let old = element.read(item)
                element.write(item, old + 1)
                i = i + 1
            }
            array.get(&a, 0).unwrap_or(0)
        }
        fn ranged() -> Int32 {
            var a = array.with_capacity::<Int32>(1)
            array.push(&mut a, 42)
            slices.get(array.slice(&a, 0, 1), 0).unwrap_or(0)
        }
    "#;
    let record = std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let header = std::mem::size_of::<crate::runtime::buffer::Buffer>() + record;
    let base = header + std::mem::size_of::<i32>() + record;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let _restore = Restore;
        LIMIT.with(|n| n.set(base));
        assert_eq!(vm.call_int32_0("direct").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        assert_eq!(
            vm.call_int32_0("ranged").unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        LIVE.with(|n| assert_eq!(n.get(), 0));
        LIMIT.with(|n| n.set(base + header));
        assert_eq!(vm.call_int32_0("ranged").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn borrowed_arrays_cross_module_aliases_preserve_views_and_consumer_types() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "views",
            r#"
        import joky/array as array.{type Array}
        import joky/element as element.{type Element}
        import joky/slice as slices.{type Slice}
        pub type Span<T> = &Slice<T>
        pub type Item<T> = &Element<T>
        pub fn range<T>(a: &Array<T>) -> Span<T> { array.slice(a, 0, 1) }
        pub fn first<T: Copy>(s: Span<T>) -> Item<T> { slices.borrow(s, 0) }
        pub fn copy<T: Copy>(e: Item<T>) -> T { element.read(e) }
        pub fn update<T: Copy>(e: &mut Element<T>, value: T) { element.write(e, value) }
    "#,
        )
        .unwrap();
    let source = r#"
        import joky/array as array
        import views as v
        struct Point { flag: Bool, value: Int64 }
        fn concrete(a: &array.Array<Point>) -> v.Span<Point> { v.range(a) }
        fn concrete_copy(item: v.Item<Point>) -> Point { v.copy(item) }
        fn check() -> Int32 {
            var a = array.new::<Point>()
            array.push(&mut a, Point { flag: true, value: 4294967297 })
            let region: v.Span<Point> = concrete(&a)
            let item: v.Item<Point> = v.first(region)
            let copied = concrete_copy(item)
            v.update(array.borrow_mut(&mut a, 0), Point { flag: false, value: 0 })
            array.reserve(&mut a, 16)
            if copied.flag && copied.value == 4294967297 { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        for (body, message) in [
            ("var a = array.new::<Int32>(); array.push(&mut a, 42); let item = v.first(v.range(&a)); array.push(&mut a, 7); v.copy(item);", "live borrow"),
            ("var a = array.new::<&Int32>(); array.push(&mut a, (&*(alloc(0)))); let local = 42; v.update(array.borrow_mut(&mut a, 0), &local)", "local borrow"),
        ] {
            let bad = format!("import joky/array as array import views as v fn bad() {{ {body} }}");
            let error = vm.load_source_with_modules(&bad, &modules).unwrap_err();
            assert!(error.message().contains(message), "{error}");
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            let ast = crate::TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&bad).unwrap(), &modules).unwrap();
            assert!(crate::aot::AotCompiler::new("invalid-imported-views").unwrap().compile_module(&ast).is_err());
        }
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    crate::aot::AotCompiler::new("imported-views")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn borrowed_arrays_interpreter_pause_cancel_reload_and_budget_keep_views_valid() {
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    for (entry, checkpoint) in [
        ("borrowed_array_ranges", 3),
        ("borrowed_array_references", 5),
    ] {
        let source = format!("{SOURCE} fn task() @suspends -> Int32 {{ {entry}() }}");
        for outcome in ["finish", "drop", "cancel"] {
            let mut vm = VmEngine::with_mode(VmMode::Interpreter);
            vm.load_source(&source).unwrap();
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            let mut task = vm.call_async("task", &[]).unwrap();
            for _ in 0..100_000 {
                if LIVE.with(|n| n.get() >= checkpoint) {
                    break;
                }
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            assert!(LIVE.with(|n| n.get() >= checkpoint));
            for _ in 0..10 {
                task.resume().unwrap();
            }
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
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
    let source = format!("{SOURCE} fn task() @suspends -> Int32 {{ borrowed_array_loop() }}");
    for budget in [50, 100, 300] {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.load_source(&source).unwrap();
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        let mut task = vm.call_async("task", &[]).unwrap();
        let mut failed = false;
        for _ in 0..=budget {
            if let Err(ResumeError::Failed(error)) = task.resume() {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                failed = true;
                break;
            }
        }
        assert!(failed);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
