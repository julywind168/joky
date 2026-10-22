use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;

#[test]
fn arena_objects_reborrow_and_release_in_both_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../../tests/fixtures/arena.jk"))
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
fn arena_rejects_aliasing_moves_and_escapes_in_all_backends() {
    let prefix = "struct Point { x: Int32 } fn make() -> &mut Point { alloc(Point { x: 1 }) } ";
    let rejected = [
        "fn f() { let p = make(); let q = p; p.x = 2; q.x = 3; }",
        "fn f() { let p = make(); let q = (&*p); p.x = 2; let n = q.x; }",
        "fn f() { let p = make(); let q: &Point = p; q.x = 2; }",
        "fn f() { let p = make(); let q = (&*p); let r: &mut Point = q; }",
        "fn twice(a: &mut Point, b: &mut Point) {} fn f() { let p = make(); twice(p, p) }",
        "fn both(a: &mut Point, b: &Point) {} fn f() { let p = make(); both(p, p) }",
        "fn both(a: &Point, b: &mut Point) {} fn f() { let p = make(); both(p, p) }",
        "fn first(a: &Point, b: &Point) -> &Point { a } fn f() -> Int32 { let a = make(); let b = make(); let v = first(a, b); a.x = 2; v.x }",
        "fn pass(a: &mut Point) -> &mut Point { a } fn f() -> Int32 { let p = make(); let q = pass(p); p.x = 3; q.x }",
        "fn pass(a: &mut Point) -> &mut Point { a } fn f() -> Int32 { let p = make(); let q = pass(p); let n = p.x; q.x + n }",
        "fn f(flag: Bool) { let p = make(); if flag { let q = p; }; p.x = 2; }",
        "fn f() { let p = make(); var i = 0; while i < 2 { let q = p; q.x = i; i = i + 1; } }",
        "fn f() { let p = make(); let q = alloc(p); }",
        "struct Link { next: &mut Point }",
        "type R = &Point; type Nested = &R; fn f(p: Nested) {}",
        "fn f(p: &&Point) {}",
        "fn f() { let p = alloc(\"text\"); }",
        "fn f(p: &vm.Function) {}",
        "fn f() { let p = make(); let xs = (p,); }",
        "fn f() { let p = make(); let maybe = Some(p); }",
        "extern fn host(p: &Point) fn f() { let p = make(); host(p); }",
        "extern fn host() -> &mut Point",
        "fn alloc(x: Int32) -> Int32 { x }",
        "fn ; (&*(x: Int32)) -> Int32 { x }",
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
        fn read(p: Read) -> Int32 { (*p) }
        fn choose(flag: Bool, a: &mut Int32, b: &mut Int32) -> &mut Int32 {
            if flag { a } else { b }
        }
        fn work(flag: Bool) -> Int32 {
            let a = alloc(10)
            let b = alloc(20)
            let q = choose(flag, a, b);
            (*(q) = 30)
            let view = (&*a)
            let copy = view
            let n = read(view) + read(copy);
            (*(a) = 5)
            var current = a
            current = b
            var i = 0
            while i < 3 { (*(current) = (*current) + 1); i = i + 1; }
            n + (*current)
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
    let source = "fn f() -> Int32 { let first = alloc(42); var i = 0; while i < 100 { let next = alloc(i); i = i + 1; }; (*first) }";
    let _reset = Reset(LIMIT.with(|limit| limit.replace(128)));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let error = vm.call_int32_0("f").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        assert_eq!(error.span().unwrap().slice(source), Some("alloc(i)"));
        LIVE.with(|live| assert_eq!(live.get(), 0));
        vm.load_source("fn f() -> Int32 { (*(alloc(42))) }")
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
        "fn f() { let p = alloc(1); let (q) = p; (*(p) = 2); (*(q) = 3); }",
        "fn f() { let p = alloc(1); var (q) = p; (*(p) = 2); (*(q) = 3); }",
        "fn use(a: &Int32, b: Int32) {} fn f() { let p = alloc(1); use(p, { (*(p) = 2); 0 }); }",
        "fn use(a: &mut Int32, b: Int32) {} fn f() { let p = alloc(1); use(p, (*p)); }",
        "fn use(a: &mut Int32, b: Int32) {} fn f() { let p = alloc(1); use(p, { let q = p; (*q) }); }",
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

#[test]
fn arena_graphs_shared_fields_and_recursive_lists() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../../tests/fixtures/arena_graphs.jk"))
            .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
        for name in [
            "graph_list",
            "graph_shared",
            "graph_tuple",
            "graph_generic",
            "graph_loop",
            "graph_tree",
            "graph_mutual",
            "graph_generic_node",
            "graph_construct",
            "graph_aggregate",
            "graph_local_borrow",
            "graph_alias",
            "graph_tuple_target",
            "graph_outer_reference",
            "graph_option_method",
            "graph_read_copy",
            "graph_inferred_record",
            "graph_inferred_enum",
            "graph_read_tuple",
            "graph_read_recursive",
            "graph_read_enum",
            "graph_read_nested",
            "graph_forward_nested",
            "graph_forward_enum",
        ] {
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        assert!(vm.function_signature("graph_aggregate_return").is_none());
        assert!(vm.call_int32_1("graph_failure", 0).is_err());
        LIVE.with(|live| assert_eq!(live.get(), 0));
        assert_eq!(vm.call_int32_1("graph_failure", 1).unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn arena_graphs_reject_permission_loss_and_unsealed_publication() {
    let prefix = "struct Point { x: Int32 } struct Link { value: Int32, next: Option<&Point> } ";
    let rejected = [
        "fn f() { let p = alloc(Point { x: 1 }); let unused = alloc(Link { value: 0, next: Some((&*p)) }); p.x = 2; }",
        "fn f() { let p = alloc(Point { x: 1 }); let x = (Some((&*p)),); let copied = x; let unused = alloc(copied); p.x = 2; }",
        "fn f() { let p = alloc(Point { x: 1 }); let x = alloc(Link { value: 0, next: None }); x.next = Some((&*p)); p.x = 2; }",
        "fn f() { let p = alloc(Point { x: 1 }); let x = alloc(Link { value: 0, next: None }); (*(x) = Link { value: 1, next: Some((&*p)) }); p.x = 2; }",
        "fn store(p: &Point) { let unused = alloc(Link { value: 0, next: Some(p) }); } fn f() { let p = alloc(Point { x: 1 }); store(p); p.x = 2; }",
        "fn store<T: Copy>(x: T) { let unused = alloc((x,)); } fn forward(p: &Point) { store(Some(p)); } fn f() { let p = alloc(Point { x: 1 }); forward(p); p.x = 2; }",
        "fn f() { let x = alloc(Link { value: 0, next: None }); let view = (&*x); x.value = 2; }",
        "fn f() -> Int32 { let x = alloc(Link { value: 0, next: None }); let view = (&*x); x.value }",
        "fn f() { let x = alloc(Link { value: 0, next: None }); let view = (&*x); (*(x) = Link { value: 1, next: None }); }",
        "fn f(flag: Bool) { let x = alloc(Link { value: 0, next: None }); if flag { let view = (&*x); }; x.value = 2; }",
        "fn consume(x: &Link) {} fn f() { let x = alloc(Link { value: 0, next: None }); consume(x); x.value = 2; }",
        "fn build() -> &mut Link { alloc(Link { value: 0, next: None }) }",
        "fn mutate(x: &mut Link) { x.value = 1; }",
        "fn build<T>(value: T) -> &mut T { alloc(value) } fn f() { let x = build(Link { value: 0, next: None }); }",
        "struct Bad { p: &mut Point }",
        "type Exclusive = &mut Point; enum Bad { Value(Exclusive) }",
        "struct Generic<T> { value: T } fn f(x: Generic<&mut Point>) {}",
        "struct Recursive { next: Recursive }",
        "enum Recursive { Next(Recursive) }",
        "type Loop = &Loop; fn f(x: Loop) {}",
        "struct Bad { pointer: &Missing }",
        "struct Cycle { next: Option<&Cycle> } fn f() { let a = alloc(Cycle { next: None }); let b = alloc(Cycle { next: None }); a.next = Some((&*b)); b.next = Some((&*a)); }",
        "extern fn host(x: Link)",
        "extern fn host() -> Option<&Point>",
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
        let mut aot = crate::aot::AotCompiler::new("graph-rejection").unwrap();
        let ast = crate::parser::parse(&source).unwrap();
        assert!(aot.compile_module(&ast).is_err(), "AOT accepted {body}");
    }
}

#[test]
fn arena_graphs_cross_module_generic_aliases_preserve_storage_contracts() {
    let mut sources = crate::ModuleSources::new();
    sources
        .insert(
            "graph",
            r#"
        pub struct Node<T: Copy> { pub value: T, next: Option<&Node<T>> }
        pub type Head<T: Copy> = Option<&Node<T>>
        pub fn push<T: Copy>(head: Head<T>, value: T) -> &Node<T> {
            (&*(alloc(Node { value: value, next: head })))
        }
        pub fn sum(head: Head<Int32>) -> Int32 {
            match head { None => 0, Some(node) => node.value + sum(node.next) }
        }
        pub fn keep(value: &Int32) { let stored = alloc((value,)); }
        pub fn copy<T: Copy>(value: &T) -> T { (*value) }
    "#,
        )
        .unwrap();
    let source = r#"
        import graph as g.{push, sum}
        fn f() -> Int32 {
            let a = push(None, 21)
            let b = push(Some(a), 21)
            let head: g.Head<Int32> = Some(b)
            sum(head)
        }
        fn copied() -> Int32 { g.copy(push(None, 42)).value }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for _ in 0..2 {
            vm.load_source_with_modules(source, &sources)
                .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
            assert_eq!(vm.call_int32_0("f").unwrap(), 42);
            assert_eq!(vm.call_int32_0("copied").unwrap(), 42);
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        let bad = "import graph.{keep} fn bad() { let p = alloc(1); keep(p); (*(p) = 2); }";
        assert!(vm.load_source_with_modules(bad, &sources).is_err());
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
    }
    let mut compiler = crate::aot::AotCompiler::new("graph-imports").unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &sources)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
}
