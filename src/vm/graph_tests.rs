use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;

#[test]
fn arena_cycles_plan_and_execute_on_a_small_native_stack() {
    // Exercise the new IR planner on the embedding stack independently of the
    // recursive parser, whose required stack depends on syntactic nesting.
    let ast = crate::parser::parse(
        r#"
        struct Node { value: Int32, next: Option<&Node> }
        fn f() -> Int32 {
            let root = graph {
                let a = graph.alloc(Node { value: 20, next: None })
                let b = graph.alloc(Node { value: 22, next: None })
                graph.link(a.next, b)
                graph.link(b.next, a)
                a
            }
            match root.next { | None => 0 | Some(b) => root.value + b.value }
        }
    "#,
    )
    .unwrap();
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut vm = VmEngine::new_interpreter();
            vm.load_module(&ast).unwrap();
            assert_eq!(vm.call_int32_0("f").unwrap(), 42);
            LIVE.with(|live| assert_eq!(live.get(), 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn arena_cycles_support_imports_aliases_and_preresolved_modules() {
    let mut sources = crate::ModuleSources::new();
    sources
        .insert(
            "graph",
            r#"
        import joky/option as optional
        pub type Link<T> = optional.Option<&Node<T>>
        pub struct Node<T> { value: T, next: Link<T> }
        pub fn make<T>(value: T) -> &Node<T> {
            graph {
                let a = graph.alloc(Node { value: value, next: optional.None })
                graph.link(a.next, a)
                a
            }
        }
        pub fn read<T>(node: &Node<T>) -> T {
            match node.next { | optional.None => node.value | optional.Some(next) => next.value }
        }
        pub fn link(value: Int32) -> Int32 { value }
    "#,
        )
        .unwrap();
    let source = r#"
        import graph
        fn f() -> Int32 { graph.read(graph.make(graph.link(42))) }
    "#;
    let ast = crate::TypeRegistry::default()
        .with_prelude(false)
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &sources)
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_module(&ast)
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
        vm.load_source_with_modules(source, &sources).unwrap();
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    crate::aot::AotCompiler::new("cycle-imports")
        .unwrap()
        .compile_module(&ast)
        .unwrap();
}

#[test]
fn arena_cycles_allocation_failure_discards_the_unpublished_group() {
    use crate::runtime::arena::LIMIT;
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let source = "struct Node { value: Int32, next: Option<&Node> } fn f() -> Int32 { let root = graph { let a = graph.alloc(Node { value: 1, next: None }); graph.link(a.next, a); let b = graph.alloc(Node { value: 2, next: None }); graph.link(b.next, a); b }; root.value }";
    let _reset = Reset(LIMIT.with(|limit| limit.replace(64)));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let error = vm.call_int32_0("f").unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
        assert_eq!(
            error.span().unwrap().slice(source),
            Some("graph.alloc(Node { value: 2, next: None })")
        );
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn arena_cycles_execute_and_release_in_both_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/arena_cycles.jk"))
            .unwrap_or_else(|error| panic!("{mode:?}: {error} {:?}", error.span()));
        for name in [
            "cycle_self",
            "cycle_pair",
            "cycle_shared",
            "cycle_mutual",
            "cycle_generic",
            "cycle_loop",
            "cycle_data_control",
            "cycle_external",
            "cycle_relink",
            "cycle_unlinked",
            "cycle_hint",
            "cycle_evaluation_order",
            "cycle_contextual_name",
        ] {
            for _ in 0..3 {
                assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
                LIVE.with(|live| assert_eq!(live.get(), 0));
            }
        }
        let error = vm.call_int32_1("cycle_failure", 0).unwrap_err();
        assert_eq!(
            error
                .span()
                .unwrap()
                .slice(include_str!("../../tests/fixtures/arena_cycles.jk")),
            Some("22 / divisor")
        );
        LIVE.with(|live| assert_eq!(live.get(), 0));
        assert_eq!(vm.call_int32_1("cycle_failure", 1).unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
        assert!(vm.function_signature("make_cycle").is_none());
    }
}

#[test]
fn arena_cycles_reject_unchecked_construction_in_all_backends() {
    let prefix = "struct Node { value: Int32, next: Option<&Node> } ";
    let invalid = [
        ("fn f() { graph {} }", "must end with a construction handle"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); a; } }", "bare construction handle"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); Some(a) } }", "bare construction handle"),
        ("fn f() { graph.alloc(Node { value: 1, next: None }); }", "direct let binding"),
        ("fn f() { let a = alloc(Node { value: 1, next: None }); graph.link(a.next, a); }", "direct let binding"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let copy = a; a } }", "Construction handles"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let copy = Some(a); a } }", "Copy type"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let copy: &Node = a; a } }", "Construction handles"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let copy = (a,); a } }", "Construction handles"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let copy = (*a); a } }", "Construction handles"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let copy = a.value; a } }", "Construction handles"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); a.value = 2; a } }", "Construction handles"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); (&*a); a } }", "Construction handles"),
        ("fn helper(a: &Node) {} fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); helper(a); a } }", "Construction handles"),
        ("impl Node { fn read(self) -> Int32 { self.value } } fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); a.read(); a } }", "Construction handles"),
        ("fn f() { graph { var a = graph.alloc(Node { value: 1, next: None }); a } }", "direct let binding"),
        ("fn f() { graph { let a: &Node = graph.alloc(Node { value: 1, next: None }); a } }", "unannotated let binding"),
        ("fn f() { graph { let a = graph.alloc::<Node>(Node { value: 1, next: None }); a } }", "unannotated let binding"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1 }); a } }", "Missing field"),
        ("fn f() { graph { let a = graph.alloc(42); a } }", "Copy record"),
        ("fn f() { let a = alloc(Node { value: 1, next: None }); graph { a } }", "allocated in this graph block"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); graph.link(a.value, a); a } }", "standard Option<&T>"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); graph.link(a.missing, a); a } }", "no field named"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); graph.link(a.next, Some(a)); a } }", "allocated in this graph block"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); graph.link(a.next, a, a); a } }", "destination field"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); graph.link::<Node>(a.next, a); a } }", "destination field"),
        ("struct Other { value: Int32 } fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let b = graph.alloc(Other { value: 2 }); graph.link(a.next, b); a } }", "standard Option<&T>"),
        ("fn f() { let a = (&*(alloc(Node { value: 1, next: None }))); graph { let b = graph.alloc(Node { value: 2, next: None }); graph.link(b.next, a); b } }", "allocated in this graph block"),
        ("fn f() { let a = alloc(Node { value: 1, next: None }); graph { let b = graph.alloc(Node { value: 2, next: None }); graph.link(a.next, b); b } }", "allocated in this graph block"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); if true { graph.link(a.next, a); }; a } }", "direct let binding"),
        ("fn f() { graph { let a = { graph.alloc(Node { value: 1, next: None }) }; a } }", "direct let binding"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); let b = graph { let c = graph.alloc(Node { value: 2, next: None }); c }; a } }", "cannot be nested"),
        ("fn f() { graph { let a = graph.alloc(Node { value: 1, next: None }); return; a } }", "cannot leave a graph block"),
        ("fn f() { while true { graph { let a = graph.alloc(Node { value: 1, next: None }); break; a }; } }", "cannot leave a graph block"),
        ("fn f() { while true { graph { let a = graph.alloc(Node { value: 1, next: None }); continue; a }; } }", "cannot leave a graph block"),
        ("fn f() { let r = graph { let a = graph.alloc(Node { value: 1, next: None }); a }; r.value = 2; }", "shared reference"),
        ("fn f() { let r = graph { let a = graph.alloc(Node { value: 1, next: None }); a }; let exclusive: &mut Node = r; }", "Type annotation mismatch"),
        ("fn f() { let r = graph { let a = graph.alloc(Node { value: 1, next: None }); a }; graph.link(r.next, r); }", "direct let binding"),
        ("fn f() { let r = graph { let a = graph.alloc(Node { value: 1, next: None }); a }; let escaped = a; }", "Undefined variable"),
        ("fn wait() @suspends {} fn f() @suspends { graph { let a = graph.alloc(Node { value: 1, next: None }); wait(); a }; }", "must be marked @suspends"),
        ("enum Fake<T> { None, Some(T) } struct FakeNode { next: Fake<&FakeNode> } fn f() { graph { let a = graph.alloc(FakeNode { next: Fake.None }); graph.link(a.next, a); a }; }", "standard Option<&T>"),
        ("enum Option<T> { None, Some(T) } fn f() { graph { let a = graph.alloc(Node { value: 1, next: Option.None }); graph.link(a.next, a); a }; }", "standard Option<&T>"),
        ("struct Plain { value: Int32 } struct Stored { value: &Plain } fn f() { let p = alloc(Plain { value: 1 }); let g = graph { let a = graph.alloc(Stored { value: p }); a }; p.value = 2; }", "retained by the task arena"),
    ];
    for (body, message) in invalid {
        let source = format!("{prefix}{body}");
        let prelude = !body.starts_with("enum Option<");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            let error = vm
                .load_source(&source)
                .expect_err(&format!("accepted {mode:?}: {body}"));
            assert!(error.span().is_some(), "{body}: {error}");
            assert!(
                error.message().contains(message),
                "{mode:?}: {body}: {error}"
            );
        }
        let mut aot = crate::aot::AotCompiler::new("cycle-rejection").unwrap();
        aot.set_prelude_enabled(prelude).unwrap();
        let ast = crate::parser::parse(&source).unwrap();
        let error = aot
            .compile_module(&ast)
            .expect_err(&format!("AOT accepted {body}"));
        assert!(error.message().contains(message), "AOT: {body}: {error}");
    }
}
