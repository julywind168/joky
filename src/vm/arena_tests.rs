use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;

#[test]
fn arena_objects_reborrow_and_release_in_both_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/arena.jk"))
            .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
        for (name, expected) in [
            ("arena_compute", 25),
            ("arena_scalars", 42),
            ("arena_nested", 22),
            ("arena_loop", 33),
            ("arena_return", 42),
            ("arena_copy_sum", 42),
            ("arena_unsigned", 42),
        ] {
            assert_eq!(vm.call_int32_0(name).unwrap(), expected, "{mode:?}: {name}");
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        assert!(vm.call_int32_1("arena_fail", 0).is_err());
        LIVE.with(|live| assert_eq!(live.get(), 0));
        assert_eq!(vm.call_int32_1("arena_fail", 2).unwrap(), 21);
        LIVE.with(|live| assert_eq!(live.get(), 0));
        assert!(vm.function_signature("make").is_none());
        assert!(vm.function_signature("forward").is_none());
        assert!(vm.call_int32_0("make").is_err());
    }
}

#[test]
fn arena_rejects_aliasing_moves_escapes_and_suspension_in_all_backends() {
    let prefix = "struct Point { x: Int32 } fn make() -> &mut Point { alloc(Point { x: 1 }) } ";
    let rejected = [
        "fn f() { let p = make(); let q = p; p.x = 2; q.x = 3; }",
        "fn f() { let p = make(); let q = share(p); p.x = 2; let n = q.x; }",
        "fn f() { let p = make(); let q: &Point = p; q.x = 2; }",
        "fn f() { let p = make(); let q = share(p); let r: &mut Point = q; }",
        "fn twice(a: &mut Point, b: &mut Point) {} fn f() { let p = make(); twice(p, p) }",
        "fn both(a: &mut Point, b: &Point) {} fn f() { let p = make(); both(p, p) }",
        "fn both(a: &Point, b: &mut Point) {} fn f() { let p = make(); both(p, p) }",
        "fn first(a: &Point, b: &Point) -> &Point { a } fn f() -> Int32 { let a = make(); let b = make(); let v = first(a, b); b.x = 2; v.x }",
        "fn pass(a: &mut Point) -> &mut Point { a } fn f() -> Int32 { let p = make(); let q = pass(p); p.x = 3; q.x }",
        "fn pass(a: &mut Point) -> &mut Point { a } fn f() -> Int32 { let p = make(); let q = pass(p); let n = p.x; q.x + n }",
        "fn f(flag: Bool) { let p = make(); if flag { let q = p; }; p.x = 2; }",
        "fn f() { let p = make(); var i = 0; while i < 2 { let q = p; q.x = i; i = i + 1; } }",
        "fn f() { let p = make(); let q = alloc(p); }",
        "struct Link { next: &Point }",
        "type R = &Point; type Nested = &R; fn f(p: Nested) {}",
        "fn f(p: &&Point) {}",
        "fn f() { let p = alloc(\"text\"); }",
        "fn f(p: &String) {}",
        "fn f() { let p = make(); let xs = (p,); }",
        "fn f() { let p = make(); let maybe = Some(p); }",
        "extern fn host(p: &Point) fn f() { let p = make(); host(p); }",
        "extern fn host() -> &mut Point",
        "fn f() @suspends { let p = make(); }",
        "fn inner() { let p = make(); } fn f() @suspends { inner(); }",
        "fn f(p: &Point) @suspends {}",
        "fn f<T>(p: &T) @suspends {}",
        "fn inner() { let p = make(); } fn f<T>() @suspends { inner(); }",
        "fn inner<T>() { let p = make(); } fn f<T>() @suspends { inner::<Int32>(); }",
        "fn f() { let local = 2; let p = &local; }",
        "fn alloc(x: Int32) -> Int32 { x }",
        "fn share(x: Int32) -> Int32 { x }",
    ];
    for body in rejected {
        let source = format!("{prefix}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let error = vm
                .load_source(&source)
                .expect_err(&format!("accepted {mode:?}: {body}"));
            assert!(error.span().is_some(), "{body}: {error}");
        }
        let mut aot = crate::aot::AotCompiler::new("arena-rejection").unwrap();
        if let Ok(ast) = crate::parser::parse(&source) {
            assert!(aot.compile_module(&ast).is_err(), "AOT accepted {body}");
        }
    }
}

#[test]
fn arena_moves_shares_branches_and_repeated_calls_remain_usable() {
    let source = r#"
        type Read = &Int32
        fn read(p: Read) -> Int32 { memory.read(p) }
        fn choose(flag: Bool, a: &mut Int32, b: &mut Int32) -> &mut Int32 {
            if flag { a } else { b }
        }
        fn work(flag: Bool) -> Int32 {
            let a = alloc(10)
            let b = alloc(20)
            let q = choose(flag, a, b)
            memory.write(q, 30)
            let view = share(a)
            let copy = view
            let n = read(view) + read(copy)
            memory.write(a, 5)
            var current = a
            current = b
            var i = 0
            while i < 3 { memory.write(current, memory.read(current) + 1); i = i + 1; }
            n + memory.read(current)
        }
        fn f() -> Int32 { work(true) + work(false) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source)
            .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
        assert_eq!(vm.call_int32_0("f").unwrap(), 136);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn arena_allocation_failure_keeps_span_and_releases_the_task() {
    use crate::runtime::arena::LIMIT;
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let source = "fn f() -> Int32 { let first = alloc(42); var i = 0; while i < 100 { let next = alloc(i); i = i + 1; }; memory.read(first) }";
    let _reset = Reset(LIMIT.with(|limit| limit.replace(128)));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let error = vm.call_int32_0("f").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        assert_eq!(error.span().unwrap().slice(source), Some("alloc(i)"));
        LIVE.with(|live| assert_eq!(live.get(), 0));
        vm.load_source("fn f() -> Int32 { memory.read(alloc(42)) }")
            .unwrap();
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn arena_imported_helpers_and_aliases_share_the_callers_storage() {
    let mut sources = crate::ModuleSources::new();
    sources
        .insert(
            "data",
            r#"
        pub struct Point { x: Int32 }
        pub type View = &Point
        pub fn make(x: Int32) -> &mut Point { alloc(Point { x: x }) }
        pub fn read(p: View) -> Int32 { p.x }
        pub fn update(p: &mut Point) { p.x = p.x + 1 }
    "#,
        )
        .unwrap();
    let source = "import data as d.{make as new, read as value, update as change}\n fn f() -> Int32 { let p = new(41); change(p); value(p) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(source, &sources)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    let mut compiler = crate::aot::AotCompiler::new("arena-imports").unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &sources)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
}

#[test]
fn arena_argument_reservations_and_shadowing_preserve_evaluation_order() {
    let source = r#"
        struct Point { x: Int32 }
        struct Memory { value: Int32 }
        impl Memory { fn read(self) -> Int32 { self.value } }
        fn add(a: &Point, n: Int32) -> Int32 { a.x + n }
        fn f() -> Int32 {
            var p = alloc(Point { x: 1 })
            let result = add(p, { p = alloc(Point { x: 10 }); p.x })
            p.x = { p = alloc(Point { x: 20 }); 30 }
            // The assignment wrote the old object; p now refers to the new one.
            let memory = Memory { value: 11 }
            result + p.x + memory.read()
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source)
            .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    let bad = [
        "fn f() { let p = alloc(1); let (q) = p; memory.write(p, 2); memory.write(q, 3); }",
        "fn f() { let p = alloc(1); var (q) = p; memory.write(p, 2); memory.write(q, 3); }",
        "fn use(a: &Int32, b: Int32) {} fn f() { let p = alloc(1); use(p, { memory.write(p, 2); 0 }); }",
        "fn use(a: &mut Int32, b: Int32) {} fn f() { let p = alloc(1); use(p, memory.read(p)); }",
        "fn use(a: &mut Int32, b: Int32) {} fn f() { let p = alloc(1); use(p, { let q = p; memory.read(q) }); }",
    ];
    for source in bad {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(
                vm.load_source(source).is_err(),
                "{mode:?} accepted {source}"
            );
        }
    }
}
