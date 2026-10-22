use super::{VmEngine, VmMode};

#[test]
fn static_traits_execute_borrowed_requirements_and_conditional_impls() {
    let source = r#"
        trait Eq { fn eq(&self, other: &Self) -> Bool }
        struct Id { value: Int32 }
        impl Eq for Id { fn eq(&self, other: &Self) -> Bool { self.value == other.value } }
        struct Pair<T> { left: T, right: T }
        impl<T: Copy + Eq> Eq for Pair<T> {
            fn eq(&self, other: &Self) -> Bool {
                let left = self.left; let right = other.left
                let a = alloc(left); let b = alloc(right)
                Eq.eq(a, b)
            }
        }
        fn same<T: Eq>(left: &T, right: &T) -> Bool { left.eq(right) }
        fn main() -> Int32 {
            let a = alloc(Pair { left: Id { value: 7 }, right: Id { value: 8 } })
            let b = alloc(Pair { left: Id { value: 7 }, right: Id { value: 9 } })
            if same(a, b) { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("main").unwrap(), 42);
    }
}

const FIXTURE: &str = include_str!("../../tests/fixtures/static_traits.jk");

#[test]
fn static_traits_preserve_layouts_bounds_and_template_dispatch() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            unsafe {
                vm.register_suspending_fn(
                    "pause",
                    crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
                    pause,
                )
                .unwrap();
            }
            let exports = vm.load_source(FIXTURE).unwrap();
            assert!(!exports
                .iter()
                .any(|n| n.contains("joky/") || n.contains('.')));
            for entry in [
                "trait_locals",
                "trait_generics",
                "trait_layouts",
                "trait_factory",
                "trait_dispatch",
            ] {
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
            }
        }
    }
}

thread_local! { static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
unsafe extern "C" fn pause(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    if !READY.with(std::cell::Cell::get) {
        return 1;
    }
    let frame = &mut *frame;
    frame.result = frame.slots[0];
    0
}

#[test]
fn static_traits_suspend_resume_and_release_cancelled_arenas() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_suspending_fn(
                "pause",
                crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
                pause,
            )
            .unwrap();
        }
        vm.load_source(FIXTURE).unwrap();
        READY.with(|ready| ready.set(false));
        let mut task = vm.call_async("trait_task", &[]).unwrap();
        assert!(task.cps_result().is_none());
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        READY.with(|ready| ready.set(false));
        let task = vm.call_async("trait_task", &[]).unwrap();
        assert!(task.cps_result().is_none());
        drop(task);
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn static_traits_reject_invalid_contracts_and_unused_templates_in_all_backends() {
    let base = "trait Read { fn read(self) -> Int32 } struct Id { value: Int32 } impl Read for Id { fn read(self) -> Int32 { self.value } }";
    for (extra, message) in [
        ("fn bad<T: Missing>(x: T) {}", "Unknown trait bound"),
        ("fn need<T: Read>(x: T) {} fn bad() { need(true) }", "does not satisfy"),
        ("fn need<T: Read>(x: T) {} fn bad<T>(x: T) { need(x) }", "does not satisfy"),
        ("fn need<T: Read>() {} fn bad() { need::<Bool>() }", "does not satisfy"),
        ("fn bad<T>(x: T) -> Int32 { x.read() }", "no method"),
        ("fn bad<T>(x: T) -> Int32 { Read.read(x) }", "does not satisfy"),
        ("impl Read for Bool {}", "Missing trait method"),
        ("impl Read for Bool { fn read(&self) -> Int32 { 1 } }", "Signature does not match"),
        ("impl Read for Bool { fn read(self) -> Bool { self } }", "Signature does not match"),
        ("impl Read for Bool { fn read(self) @suspends -> Int32 { 1 } }", "Signature does not match"),
        ("impl Read for Bool { pub fn read(self) -> Int32 { 1 } }", "Signature does not match"),
        ("impl Read for Bool { fn read<T>(self) -> Int32 { 1 } }", "Signature does not match"),
        ("impl Read for Bool { fn extra(self) {} }", "has no method"),
        ("impl Read for Bool { const X: Int32 = 1 }", "associated constants"),
        ("impl Read for Id { fn read(self) -> Int32 { 2 } }", "Overlapping"),
        ("trait Other { fn read(self) -> Int32 } impl Other for Id { fn read(self) -> Int32 { 1 } } fn bad(x: Id) -> Int32 { x.read() }", "Ambiguous"),
        ("trait Other { fn read(self) -> Int32 } fn bad<T: Read + Other>(x: T) -> Int32 { x.read() }", "Ambiguous"),
        ("trait Repeated { fn a(self) fn a(self) }", "Duplicate trait method"),
        ("trait Bad { fn a(x: Self, x: Self) }", "Duplicate trait method parameter"),
        ("struct Pair<T> { x: T } impl<T: Read> Read for Pair<T> { fn read(self) -> Int32 { self.x.read() } } fn bad(x: Pair<Bool>) -> Int32 { x.read() }", "no method"),
        ("struct Pair<T> { x: T } impl<T: Read> Read for Pair<T> { fn read(self) -> Int32 { self.x.read() } } fn bad<T>(x: Pair<T>) -> Int32 { Read.read(x) }", "does not satisfy"),
        ("impl Missing for Bool {}", "Unknown trait"),
        ("fn bad(x: Id) { Read::<Bool>.read(x); }", "Traits do not accept"),
        ("fn bad(x: Id) { (Read::<Id>).read(x); }", "Traits do not accept"),
        ("fn bad(x: Id) { Read.read::<Bool>(x); }", "Conflicting types"),
        ("fn bad(x: Id) { Read.read(x, x); }", "Argument count"),
        ("fn bad(x: Id) { x.read::<Bool>(); }", "Type argument count"),
    ] {
        let source = format!("{base} {extra}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn previous() -> Int32 { 42 }").unwrap();
            let error = vm.load_source(&source).expect_err(extra);
            assert!(error.message().contains(message), "{mode:?}: {error}\n{extra}");
            assert!(error.span().is_some());
            assert_eq!(vm.call_int32_0("previous").unwrap(), 42);
        }
        let error = crate::aot::AotCompiler::new("bad-trait").unwrap()
            .compile_module(&crate::parser::parse(&source).unwrap()).expect_err(extra);
        assert!(error.message().contains(message), "{error}\n{extra}");
    }
}

#[test]
fn static_traits_reject_unsupported_syntax() {
    for source in [
        "struct P<T: Read> { x: T }",
        "type P<T: Read> = T",
        "enum P<T: Read> { Some(T) }",
        "trait T<X> {}",
        "trait T { fn f<X>(self) }",
        "trait T { type X = Int32 }",
        "trait T { const X: Int32 = 1 }",
        "trait T { pub fn f(self) }",
        "trait T { fn f(x: Self, &self) }",
        "trait T {} fn f<X: T + T>() {}",
        "trait T {} import x",
    ] {
        assert!(crate::parser::parse(source).is_err(), "{source}");
    }
}

#[test]
fn static_traits_keep_identity_and_definition_scope_across_diamond_imports() {
    let mut modules = crate::ModuleSources::new();
    for (path, source) in [
        ("base", "pub trait Read { fn read(self) -> Int32 } pub struct Id { pub value: Int32 } fn helper(x: Int32) -> Int32 { x } impl Read for Id { fn read(self) -> Int32 { helper(self.value) } } pub fn read<T: Read>(x: T) -> Int32 { x.read() } trait Hidden { fn hidden(self) -> Int32 } impl Hidden for Id { fn hidden(self) -> Int32 { self.value } } pub fn private_bound<T: Hidden>(x: T) -> Int32 { x.hidden() }"),
        ("left", "import base pub fn make() -> base.Id { base.Id { value: 20 } }"),
        ("right", "import base pub fn make() -> base.Id { base.Id { value: 22 } }"),
        ("foreign", "import base pub struct Value { pub n: Int32 } impl base.Read for Value { fn read(self) -> Int32 { self.n } }"),
    ] { modules.insert(path, source).unwrap(); }
    let source = r#"
        import base as b.{type Read as Saved, type Id}
        import left
        import right
        import foreign
        trait Read { fn read(self) -> Bool }
        struct Local { n: Int32 }
        impl Saved for Local { fn read(self) -> Int32 { self.n } }
        fn check() -> Int32 {
            let a = left.make(); let z = right.make()
            let x = foreign.Value { n: 42 }
            if Saved.read(a) + z.read() == 42 && b.read(x) == 42 && b.private_bound(Id { value: 42 }) == 42 {
                b.read(Local { n: 42 })
            } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(source, &modules)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        for (bad, message) in [
            ("import base.{type Hidden} fn bad() {}", "does not export"),
            ("import base fn bad(x: base.Id) { x.hidden(); }", "no method"),
            ("import base fn bad(x: base.Id) { base.Hidden.hidden(x); }", "Undeclared"),
            ("import base trait Read {} fn need<T: Read>(x: T) {} fn bad(x: base.Id) { need(x) }", "does not satisfy"),
            ("import base impl base.Read for Int32 { fn read(self) -> Int32 { self } }", "defined in this module"),
            ("import base impl base.Read for base.Id { fn read(self) -> Int32 { self.value } }", "defined in this module"),
            ("import base.{type Read as A, type Read as B} fn bad<T: A + B>() {}", "Duplicate trait bound"),
            ("import base.{type Read} struct Read {}", "already defined"),
            ("import base.{type Id as Read} trait Read {}", "already defined"),
        ] {
            let error = vm.load_source_with_modules(bad, &modules).expect_err(bad);
            assert!(error.message().contains(message), "{error}\n{bad}");
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        }
    }
    let registry = crate::TypeRegistry::default();
    let ast = registry
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    let again = registry.resolve_aliases(&ast).unwrap();
    assert_eq!(ast.traits.len(), again.traits.len());
    crate::aot::AotCompiler::new("module-traits")
        .unwrap()
        .compile_module(&again)
        .unwrap();
}

#[test]
fn static_traits_preserve_borrowing_and_conservative_template_effects() {
    let base = r#"
        struct Id { value: Int32 }
        trait Read { fn read(&self) -> Int32 }
        trait Set { fn set(&mut self, n: Int32) }
        trait View { fn view(&self) -> &Self }
        impl Read for Id { fn read(&self) -> Int32 { self.value } }
        impl Set for Id { fn set(&mut self, n: Int32) { self.value = n } }
        impl View for Id { fn view(&self) -> &Self { self } }
        fn view<T: Copy + View>(x: &T) -> &T { x.view() }
    "#;
    for (extra, message) in [
        ("fn bad() { let x = Id { value: 1 }; x.set(42) }", "var binding"),
        ("fn bad() { var x = Id { value: 1 }; let r = x.view(); x.set(42); r.read(); }", "borrow"),
        ("fn bad() { var x = Id { value: 1 }; x.set(x.read()) }", "borrow"),
        ("fn bad() -> &Id { let x = Id { value: 1 }; view(&x) }", "local"),
        ("fn bad<T: Copy + View>(x: T) -> &T { view(&x) }", "local"),
        ("fn bad<T: Copy + Read>(x: T) -> Int32 { x.read() }", "retained"),
        ("fn bad() { let x = (&*(alloc(Id { value: 1 }))); x.set(42) }", "Argument type mismatch"),
        ("fn pause() @suspends {} fn bad() @suspends { let x = Id { value: 1 }; let r = &x; pause(); r.read(); }", "suspension"),
        ("trait Save { fn save(&self) } struct Holder { p: &Id } impl Save for Id { fn save(&self) { alloc(Holder { p: self }); } } fn save<T: Save>(x: &T) { x.save() } fn bad() { let x = Id { value: 1 }; save(&x) }", "retained"),
        ("trait Wait { fn wait(&self) @suspends } fn bad<T: Wait>(x: &T) { x.wait() }", "suspend"),
    ] {
        let source = format!("{base} {extra}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source(&source).expect_err(extra);
            assert!(error.message().contains(message), "{error}\n{extra}");
        }
        let error = crate::aot::AotCompiler::new("unsafe-traits").unwrap()
            .compile_module(&crate::parser::parse(&source).unwrap()).expect_err(extra);
        assert!(error.message().contains(message), "{error}\n{extra}");
    }
}

#[test]
fn static_traits_keep_by_value_receivers_free_of_spurious_local_borrows() {
    let source = r#"
        trait Value { fn read(self) -> Int32 }
        trait Borrowed { fn read(&self) -> Int32 }
        struct Id { value: Int32 }
        impl Value for Id { fn read(self) -> Int32 { self.value } }
        impl Borrowed for Id { fn read(&self) -> Int32 { self.value } }
        impl Id { fn read(self) -> Int32 { self.value } }
        fn generic<T: Value>(x: T) @suspends -> Int32 { x.read() }
        fn concrete() @suspends -> Int32 { let x = Id { value: 42 }; x.read() }
        fn bound() @suspends -> Int32 { generic(Id { value: 42 }) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        for entry in ["concrete", "bound"] {
            assert_eq!(vm.call_async(entry, &[]).unwrap().cps_result(), Some(42));
        }
    }
}

#[test]
fn static_traits_enforce_bounds_on_inherent_methods_and_constants() {
    let base = "trait Marker {} impl Marker for Int32 {} struct P<T> { value: T } impl<T: Marker> P<T> { const VALUE: Int32 = 42 fn value(self) -> T { self.value } }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&format!("{base} fn main() -> Int32 {{ if P::<Int32>.VALUE == 42 {{ P {{ value: 42 }}.value() }} else {{ 0 }} }}")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("main").unwrap(), 42);
        for bad in [
            "fn bad() { P::<Bool>.VALUE; }",
            "fn bad() { P { value: true }.value(); }",
            "fn bad<T>(p: P<T>) { p.value(); }",
        ] {
            let error = vm.load_source(&format!("{base} {bad}")).expect_err(bad);
            assert!(error.message().contains("does not satisfy"), "{error}");
        }
    }
}

#[test]
fn static_traits_do_not_expose_private_inherent_methods_through_trait_impls() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "base",
            "pub struct Id { pub value: Int32 } impl Id { fn read(self) -> Int32 { 999 } }",
        )
        .unwrap();
    modules
        .insert(
            "extension",
            r#"
        import base
        pub trait Read { fn read(&self) -> Int32 }
        impl Read for base.Id { fn read(&self) -> Int32 { self.value } }
        pub fn check() -> Int32 { let x = base.Id { value: 42 }; x.read() }
    "#,
        )
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(
            "import extension fn check() -> Int32 { extension.check() }",
            &modules,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
}

#[test]
fn static_trait_proofs_share_repeated_phantom_obligations() {
    let mut source = String::from("trait Mark {} impl Mark for Int32 {} struct P<T, U> {} impl<T: Mark, U: Mark> Mark for P<T, U> {} type N0 = Int32");
    for n in 1..=20 {
        source.push_str(&format!(" type N{n} = P<N{}, N{}>", n - 1, n - 1));
    }
    source.push_str(" fn need<T: Mark>() -> Int32 { 42 } fn main() -> Int32 { need::<N20>() }");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("main").unwrap(), 42);
    }
}

#[test]
fn static_trait_syntax_preserves_bounds_receivers_and_declaration_identity() {
    use crate::parser::ast::{Item, Type};
    let source = "pub trait Eq { fn eq(&self, other: &Self) -> Bool } trait Mark {} struct P<T> { value: T } impl<T: Eq + Mark> Eq for P<T> { fn eq(&self, other: &Self) -> Bool { true } } fn same<T: Eq>(left: &T, right: &T) -> Bool { left.eq(right) }";
    let module = crate::parser::parse(source).unwrap();
    let declaration = &module.traits[0];
    assert!(declaration.public);
    let receiver = declaration.methods[0].params[0]
        .ty
        .reference_type()
        .unwrap();
    assert!(!receiver.mutable);
    assert!(matches!(&receiver.target, Type::Parameter(p) if p == &declaration.self_parameter));
    let implementation = &module.implementations[0];
    assert_eq!(implementation.trait_name.as_deref(), Some("Eq"));
    assert_eq!(
        implementation.type_params[0]
            .bounds
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["Eq", "Mark"]
    );
    assert_eq!(implementation.type_params[0].span.slice(source), Some("T"));
    let Item::FnDef(function) = &module.items[0] else {
        panic!()
    };
    assert_eq!(
        function.type_params[0]
            .bounds
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["Eq"]
    );
    assert_ne!(function.type_params[0], implementation.type_params[0]);
    assert_eq!(function.type_params[0].span.slice(source), Some("T"));
}

#[test]
fn static_traits_reject_stale_identities_in_normalized_ast() {
    let registry = crate::TypeRegistry::default();
    let mut module = registry
        .resolve_aliases(
            &crate::parser::parse(
                "trait Mark {} fn need<T: Mark>() {} fn main() { need::<Int32>() }",
            )
            .unwrap(),
        )
        .unwrap();
    module.traits[0].self_parameter = std::sync::Arc::new(crate::parser::ast::TypeParameter::new(
        "Self".into(),
        module.traits[0].span,
    ));
    let error = crate::aot::AotCompiler::new("stale-trait")
        .unwrap()
        .compile_module(&module)
        .unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::UnknownType);
    assert!(error.message().contains("Unknown trait identity"));
}

#[test]
fn trait_defaults_execute_final_static_methods_in_all_backends() {
    let source = r#"
        trait Measure {
            fn value(&self) @noescape -> Int32
            fn doubled(&self) @noescape -> Int32 { self.value() * 2 }
            fn noop(&self) @noescape {}
            fn recurse(&self, n: Int32) @noescape -> Int32 {
                if n == 0 { self.doubled() } else { self.recurse(n - 1) }
            }
        }
        struct A {}
        struct B {}
        impl Measure for A { fn value(&self) -> Int32 { 21 } }
        impl A { fn value(&self) -> Int32 { 999 } }
        impl Measure for B {
            fn value(&self) -> Int32 { 1 }
            fn doubled(&self) -> Int32 { 42 }
        }
        fn read<T: Measure>(value: &T) @noescape -> Int32 { value.recurse(3) }
        fn test() -> Int32 {
            let a = A {}; let b = B {}
            a.noop()
            read(&a) + read(&b)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 84);
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "test",
        84,
    );
}

#[test]
fn trait_defaults_reject_unused_and_overridden_invalid_bodies() {
    for source in [
        "trait Bad { fn x(&self) -> Int32 { unknown() } }",
        "trait Bad { fn x(&self) -> Int32 { self.field } }",
        "trait Bad { fn x(&self) -> Int32 { true } } struct A {} impl Bad for A { fn x(&self) -> Int32 { 42 } }",
        "trait Bad { fn x(&self) -> Int32 {} }",
        "trait Bad { fn need(&self) fn x(&self) {} } struct A {} impl Bad for A {}",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(source).is_err(), "{source}");
        }
        assert!(crate::aot::AotCompiler::new("bad-default").unwrap()
            .compile_module(&crate::parser::parse(source).unwrap()).is_err(), "{source}");
    }
}

#[test]
fn trait_defaults_support_all_receivers_and_mutual_recursion() {
    let source = r#"
        trait Counter {
            fn create() -> Self
            fn advance(&mut self) @noescape
            fn finish(self) -> Int32
            fn twice(&mut self) @noescape { self.advance(); self.advance() }
            fn consume(self) -> Int32 { self.finish() }
            fn even(&self, n: Int32) @noescape -> Bool {
                if n == 0 { true } else { self.odd(n - 1) }
            }
            fn odd(&self, n: Int32) @noescape -> Bool {
                if n == 0 { false } else { self.even(n - 1) }
            }
            fn run() -> Int32 {
                var value = Counter.create::<Self>()
                value.twice()
                if value.even(8) { value.consume() } else { 0 }
            }
        }
        struct C { value: Int32 }
        impl Counter for C {
            fn create() -> Self { C { value: 40 } }
            fn advance(&mut self) { self.value = self.value + 1 }
            fn finish(self) -> Int32 { self.value }
        }
        fn test() -> Int32 { C.run() }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "test",
        42,
    );
}

#[test]
fn associated_types_bind_nested_suffixes_to_each_bases_capability() {
    let source = r#"
        trait Inner { type Output }
        trait Outer {
            type Item: Inner
            fn pass(value: Self.Item.Output) -> Self.Item.Output { value }
        }
        struct I {}
        struct O {}
        impl Inner for I { type Output = Int32 }
        impl Outer for O { type Item = I }
        fn exact<T: Inner<Output = U.Item.Output>, U: Outer>(value: T.Output) -> U.Item.Output { value }
        fn test() -> Int32 { exact::<I, O>(O.pass(42)) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "test",
        42,
    );
}

#[test]
fn associated_types_execute_concrete_and_generic_projections() {
    let source = r#"
        trait Source {
            type Item
            fn next(&self) @noescape -> Self.Item
        }
        struct Counter {}
        impl Source for Counter {
            type Item = Int32
            fn next(&self) -> Int32 { 42 }
        }
        fn take<S: Source>(s: &S) @noescape -> S.Item { s.next() }
        fn test() -> Int32 { let c = Counter {}; take(&c) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "test",
        42,
    );
}

#[test]
fn associated_types_compose_equalities_owners_and_defaults() {
    let cases = [
        r#"
        trait Source { type Item
            fn next(&self) @noescape -> Self.Item
            fn skip(&self) @noescape { let ignored = self.next() }
        }
        struct C {}
        impl Source for C { type Item = String
            fn next(&self) -> String { "forty-two" }
        }
        fn take<S: Source>(s: &S) @noescape -> S.Item { s.next() }
        fn test() -> Int32 { let c = C {}; c.skip(); let text = take(&c); if text.byte_length() == 9 { 42 } else { 0 } }
        "#,
        r#"
        import joky/option.{type Option, Some}
        trait Source { type Item
            fn next(&self) @noescape -> Option<Self.Item>
        }
        struct C {}
        impl Source for C { type Item = Int32
            fn next(&self) -> Option<Int32> { Some(42) }
        }
        fn take<S: Source<Item = U>, U>(s: &S) @noescape -> Option<U> { s.next() }
        fn first<S: Source<Item = Int32>>(s: &S) @noescape -> Int32 { take::<S,Int32>(s).unwrap_or(0) }
        fn test() -> Int32 { let c = C {}; first(&c) }
        "#,
        r#"
        trait Value { fn value(&self) @noescape -> Int32 }
        impl Value for Int32 { fn value(&self) -> Int32 { (*self) } }
        trait Source { type Item: Value
            fn next(&self) @noescape -> Self.Item
            fn read(&self) @noescape -> Int32 { let value = self.next(); value.value() }
        }
        struct C {}
        impl Source for C { type Item = Int32
            fn next(&self) -> Int32 { 42 }
        }
        fn test() -> Int32 { let c = C {}; c.read() }
        "#,
        r#"
        trait Left { type Item }
        trait Right { type Item }
        struct C {}
        impl Left for C { type Item = Int32 }
        impl Right for C { type Item = Bool }
        fn choose<T: Left + Right>(a: <T as Left>::Item, b: <T as Right>::Item) -> <T as Left>::Item { a }
        fn test() -> Int32 { choose::<C>(42, true) }
        "#,
        r#"
        trait Source { type Item
            fn next(&self) @noescape -> Self.Item
        }
        struct Box<T> { value: T }
        impl<T: Copy> Source for Box<T> { type Item = T
            fn next(&self) -> T { self.value }
        }
        type Output = <Box<Int32> as Source>::Item
        fn test() -> Output { let b = Box { value: 42 }; b.next() }
        "#,
    ];
    for (index, source) in cases.iter().enumerate() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source)
                .unwrap_or_else(|e| panic!("case {index}: {e}"));
            assert_eq!(vm.call_int32_0("test").unwrap(), 42, "case {index}");
        }
        #[cfg(unix)]
        crate::backends::aot::tests::assert_sync_int32(
            &crate::parser::parse(source).unwrap(),
            "test",
            42,
        );
    }
}

#[test]
fn associated_types_reject_invalid_definitions_and_uses_in_all_backends() {
    let cases = [
        "trait S { type Item } struct C {} impl S for C {}",
        "trait S { type Item } struct C {} impl S for C { type Item = Int32; type Item = Int32 }",
        "trait S { type Item } struct C {} impl S for C { type Other = Int32 }",
        "trait S { type Item; type Item }",
        "trait S { type Item } struct C {} impl S for C { type Item = Self.Item }",
        "trait S { type Item; type Other } struct C {} impl S for C { type Item = Self.Other; type Other = Self.Item }",
        "trait S { type Item; fn get(&self) -> Self.Item } struct C {} impl S for C { type Item = String; fn get(&self) -> Int32 { 1 } }",
        "fn bad<T>(x: T.Item) {}",
        "trait S { type Item } fn bad<T>(x: <T as S>::Item) {}",
        "trait L { type Item } trait R { type Item } fn bad<T: L + R>(x: T.Item) {}",
        "trait S { type Item } fn bad<T: S<Item = Int32, Item = Int32>>() {}",
        "trait S { type Item } fn bad<T: S<Other = Int32>>() {}",
        "trait S { type Item } fn bad<T: S<Item = _>>() {}",
        "trait S { type Item: M } trait M {} struct C {} impl S for C { type Item = Int32 }",
        "trait S { type Item } struct C {} impl S for C { type Item = String } fn need<T: S<Item = Int32>>() {} fn bad() { need::<C>() }",
        "trait S { type Item; fn next(&self) @noescape -> Self.Item } struct C {} impl S for C { type Item = String; fn next(&self) -> String { \"x\" } } fn bad(c: &C) { let x = c.next(); let a = x; let b = x }",
        "trait S { type Item } fn bad<T: S>(x: T.Item) -> Int32 { x + 1 }",
    ];
    for source in cases {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn previous() -> Int32 { 42 }").unwrap();
            assert!(vm.load_source(source).is_err(), "accepted: {source}");
            assert_eq!(vm.call_int32_0("previous").unwrap(), 42);
        }
        assert!(
            crate::aot::AotCompiler::new("bad-associated")
                .unwrap()
                .compile_module(&crate::parser::parse(source).unwrap())
                .is_err(),
            "accepted: {source}"
        );
    }
}

#[test]
fn associated_types_preserve_definition_scopes_and_diamond_identities() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "protocol",
            r#"
        pub trait Source {
            type Item
            fn next(&self) @noescape -> Self.Item
            fn forwarded(&self) @noescape -> Self.Item { helper(self.next()) }
        }
        fn helper<T>(x: T) -> T { x }
        pub fn take<T: Source>(x: &T) @noescape -> T.Item { x.forwarded() }
    "#,
        )
        .unwrap();
    modules
        .insert("left", "import protocol pub fn ignore() {}")
        .unwrap();
    modules
        .insert("right", "import protocol pub fn ignore() {}")
        .unwrap();
    modules
        .insert(
            "implementation",
            r#"
        import protocol.{type Source as Capability}
        pub struct C {}
        type Number = Int32
        impl Capability for C { type Item = Number
            fn next(&self) -> Int32 { 42 }
        }
        impl C { pub fn next(&self) -> Bool { false } }
    "#,
        )
        .unwrap();
    modules
        .insert(
            "other",
            r#"
        import protocol.{type Source as Capability}
        pub struct C {}
        type Number = Bool
        impl Capability for C { type Item = Number
            fn next(&self) -> Bool { true }
        }
    "#,
        )
        .unwrap();
    let source = r#"
        import protocol as p import left import right import implementation as i import other
        type Number = String
        fn read_int(value: &dyn p.Source<Item = Int32>) @noescape -> Int32 { value.forwarded() }
        fn read_bool(value: &dyn p.Source<Item = Bool>) @noescape -> Bool { value.forwarded() }
        fn test() -> Int32 {
            let c = i.C {}; let d = other.C {}
            let integer: &dyn p.Source<Item = Int32> = &c
            let boolean: &dyn p.Source<Item = Bool> = &d
            if p.take(&d) && read_bool(boolean) { p.take(&c) + read_int(integer) - 42 } else { 0 }
        }
    "#;
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
}

#[test]
fn associated_types_resolve_forward_layouts_and_identity_equalities() {
    let cases = [
        "trait S { type Item } struct Outer { value: C.Item } struct C {} struct Value { n: Int32 } impl S for C { type Item = Value } fn test() -> Int32 { let x = Outer { value: Value { n: 42 } }; x.value.n }",
        "trait S { type Item } struct Outer { first: A.Item, second: B.Item } struct A {} struct B {} struct V1 { n: Int32 } struct V2 { n: Int32 } impl S for A { type Item = (V1,) } impl S for B { type Item = (V2,) } fn test() -> Int32 { let x = Outer { first: (V1 { n: 20 },), second: (V2 { n: 22 },) }; let (a,) = x.first; let (b,) = x.second; a.n + b.n }",
        "trait S { type Item } struct C {} impl S for C { type Item = Int32 } fn id<T: S<Item = T.Item>>(x: T.Item) -> T.Item { x } fn test() -> Int32 { id::<C>(42) }",
        "trait S { type Item } type A = C.Item struct C {} impl S for C { type Item = Int32 } fn test() -> A { 42 }",
        "trait S { type Item } struct C {} impl S for C { type Item = Self } fn test() -> Int32 { let c: C.Item = C {}; 42 }",
    ];
    for source in cases {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source)
                .unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(vm.call_int32_0("test").unwrap(), 42);
        }
        #[cfg(unix)]
        crate::backends::aot::tests::assert_sync_int32(
            &crate::parser::parse(source).unwrap(),
            "test",
            42,
        );
    }
    for source in [
        "trait S { type Item } type A = C.Item struct C {} impl S for C { type Item = A }",
        "trait S { type Item } fn bad<T: S<Item = T.Other, Other = T.Item>>() {}",
    ] {
        assert!(VmEngine::new().load_source(source).is_err(), "{source}");
    }
}

#[test]
fn associated_type_syntax_rejects_out_of_scope_extensions() {
    for source in [
        "trait S { type Item = Int32 }",
        "trait S { type Item<T> }",
        "trait S { type Item fn get(&self) }",
        "trait S { type Item, fn get(&self) }",
        "trait S { pub type Item }",
        "struct C {} impl C { type Item = Int32 }",
        "trait S { type Item } struct C {} impl S for C { pub type Item = Int32 }",
        "trait S { type Item: Fn() -> Int32 }",
        "trait S { type Item } fn f<T: S<Int32>>() {}",
        "trait S { type Item } struct C {} impl S for C { type Item = Int32 fn get() {} }",
    ] {
        assert!(crate::parser::parse(source).is_err(), "{source}");
    }
}

#[test]
fn associated_types_report_binding_locations_and_cycle_paths() {
    for (source, code, location, path) in [
        ("trait S { type Item } struct C {} impl S for C { type Item = Missing }",
            crate::ErrorCode::UnknownType, "Missing", None),
        ("trait S { type Item } struct C {} impl S for C { type Other = Int32 }",
            crate::ErrorCode::UnknownName, "Other", None),
        ("trait S { type Item; type Other } struct C {} impl S for C { type Item = Self.Other; type Other = Self.Item }",
            crate::ErrorCode::RecursiveType, "Self", Some("->")),
    ] {
        let assert_error = |error: crate::Error| {
            assert_eq!(error.code(), code, "{error}");
            let span = error.span().expect("associated diagnostics retain source locations");
            assert_eq!(&source[span.start()..span.end()], location, "{error}");
            if let Some(path) = path {
                assert!(error.message().contains(path), "{error}");
                assert!(error.message().contains("Item"), "{error}");
                assert!(error.message().contains("Other"), "{error}");
            }
        };
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert_error(VmEngine::with_mode(mode).load_source(source).unwrap_err());
        }
        assert_error(crate::aot::AotCompiler::new("associated-diagnostic").unwrap()
            .compile_module(&crate::parser::parse(source).unwrap()).unwrap_err());
    }
}

const ASSOCIATED_LIFECYCLE: &str =
    include_str!("../../tests/fixtures/trait_associated_lifecycle.jk");
thread_local! { static DEFAULT_EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
unsafe extern "C" fn default_record(value: i32) -> i32 {
    DEFAULT_EVENTS.with(|events| events.borrow_mut().push(value));
    value
}
fn default_lifecycle_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
            default_record as *const u8,
        )
        .unwrap();
        vm.register_suspending_fn(
            "pause",
            crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Int32),
            pause,
        )
        .unwrap();
    }
    vm.load_source(ASSOCIATED_LIFECYCLE)
        .unwrap_or_else(|e| panic!("{e}"));
    vm
}
#[test]
fn trait_defaults_associated_owners_cleanup_resume_cancel_failure_and_reload() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = default_lifecycle_vm(mode);
        DEFAULT_EVENTS.with(|events| events.borrow_mut().clear());
        assert_eq!(vm.call_int32_0("default_discard").unwrap(), 42);
        DEFAULT_EVENTS.with(|events| assert_eq!(*events.borrow(), [7]));
        DEFAULT_EVENTS.with(|events| events.borrow_mut().clear());
        assert!(vm.call_int32_0("default_failure").is_err());
        DEFAULT_EVENTS.with(|events| assert_eq!(*events.borrow(), [7]));
        for cancel in [false, true] {
            DEFAULT_EVENTS.with(|events| events.borrow_mut().clear());
            READY.with(|r| r.set(false));
            let mut task = vm.call_async("default_task", &[]).unwrap();
            for _ in 0..4 {
                task.resume().unwrap();
                assert!(task.cps_result().is_none());
            }
            DEFAULT_EVENTS.with(|events| assert_eq!(*events.borrow(), [10]));
            if !cancel {
                assert!(vm
                    .load_source("trait Invalid { type Item } struct C {} impl Invalid for C {}")
                    .is_err());
                let replacement = ASSOCIATED_LIFECYCLE.replace("pause(42)","pause(99)")
                    .replace("type Item = String\n    fn make(id: Int32) -> String { \"owned across suspension\" }", "type Item = Bool\n    fn make(id: Int32) -> Bool { true }");
                vm.load_source(&replacement)
                    .unwrap_or_else(|e| panic!("{e}"));
                READY.with(|r| r.set(true));
                task.resume().unwrap();
                assert_eq!(task.cps_result(), Some(42));
                DEFAULT_EVENTS.with(|events| assert_eq!(*events.borrow(), [10, 20]));
                DEFAULT_EVENTS.with(|events| events.borrow_mut().clear());
                let current = vm.call_async("default_task", &[]).unwrap();
                assert_eq!(current.cps_result(), Some(99));
                drop(current);
            }
            drop(task);
            if cancel {
                DEFAULT_EVENTS.with(|events| assert_eq!(*events.borrow(), [10]));
            }
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
            vm.load_source(ASSOCIATED_LIFECYCLE)
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }
}

#[test]
#[ignore = "exhaustive: cancel associated-owner defaults at every instruction boundary"]
fn exhaustive_trait_defaults_associated_owners_instruction_cancellation() {
    let mut vm = default_lifecycle_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(crate::vm::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for boundary in 0..512 {
        READY.with(|r| r.set(true));
        DEFAULT_EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("default_task", &[]).unwrap();
        for _ in 0..boundary {
            if task.cps_result().is_some() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.cps_result().is_some();
        drop(task);
        DEFAULT_EVENTS.with(|events| {
            let events = events.borrow();
            assert!(
                events.iter().filter(|&&value| value == 7).count() <= 1,
                "boundary {boundary}: {events:?}"
            );
            assert!(events.iter().filter(|&&value| value == 10).count() <= 1);
            assert!(events.iter().filter(|&&value| value == 20).count() <= 1);
            if completed {
                assert_eq!(*events, [10, 20]);
            }
        });
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0, "boundary {boundary}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn associated_types_bound_normalization_depth_and_cache_reuse() {
    // Nested bases are dependencies too, even though each impl binding is flat.
    for depth in [32, 33] {
        let mut source = String::from("trait S { type Item }\n");
        let mut projection = "C0".to_owned();
        for i in 0..=depth {
            source.push_str(&format!(
                "struct C{i} {{}} impl S for C{i} {{ type Item = {} }}\n",
                if i == depth {
                    "Int32".into()
                } else {
                    format!("C{}", i + 1)
                }
            ));
            projection = format!("<{projection} as S>::Item");
        }
        source.push_str(&format!("fn test() -> {projection} {{ 42 }}"));
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            if depth == 32 {
                vm.load_source(&source)
                    .unwrap_or_else(|e| panic!("nested depth {depth}: {e}"));
                assert_eq!(vm.call_int32_0("test").unwrap(), 42);
            } else {
                assert_eq!(
                    vm.load_source(&source).unwrap_err().code(),
                    crate::ErrorCode::ResourceLimit
                );
            }
        }
        let ast = crate::parser::parse(&source).unwrap();
        if depth == 32 {
            #[cfg(unix)]
            crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
        } else {
            assert_eq!(
                crate::aot::AotCompiler::new("nested-projection")
                    .unwrap()
                    .compile_module(&ast)
                    .unwrap_err()
                    .code(),
                crate::ErrorCode::ResourceLimit
            );
        }
    }
    for depth in [32, 33] {
        let mut source = String::from("trait S { type Item }\n");
        for i in 0..=depth {
            source.push_str(&format!(
                "struct C{i} {{}} impl S for C{i} {{ type Item = {} }}\n",
                if i == depth {
                    "Int32".into()
                } else {
                    format!("<C{} as S>::Item", i + 1)
                }
            ));
        }
        source.push_str("fn test() -> <C0 as S>::Item { 42 }");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            if depth == 32 {
                vm.load_source(&source)
                    .unwrap_or_else(|e| panic!("depth {depth}: {e}"));
                assert_eq!(vm.call_int32_0("test").unwrap(), 42);
            } else {
                let error = vm.load_source(&source).unwrap_err();
                assert_eq!(error.code(), crate::ErrorCode::ResourceLimit, "{error}");
            }
        }
        let ast = crate::parser::parse(&source).unwrap();
        if depth == 32 {
            #[cfg(unix)]
            crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
        } else {
            assert_eq!(
                crate::aot::AotCompiler::new("deep-projection")
                    .unwrap()
                    .compile_module(&ast)
                    .unwrap_err()
                    .code(),
                crate::ErrorCode::ResourceLimit
            );
        }
    }
    let mut source = String::from("trait M {} impl M for Int32 {} struct P<T,U> {} impl<T:M,U:M> M for P<T,U> {} trait S { type Item } struct C<T> {} impl<T:M> S for C<T> { type Item = Int32 } type N0 = Int32\n");
    for i in 1..=20 {
        source.push_str(&format!("type N{i} = P<N{},N{}>\n", i - 1, i - 1));
    }
    source.push_str("fn test() -> <C<N20> as S>::Item { 42 }");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
}

#[test]
fn associated_types_share_the_256_obligation_budget_with_trait_proofs() {
    let mut source = String::from("trait M {} struct P<T,U> {} impl<T:M,U:M> M for P<T,U> {} struct Q<T> {} impl<T:M> M for Q<T> {} trait S { type Item } struct C<T> {} impl<T:M> S for C<T> { type Item = Int32 }\n");
    let mut level = Vec::new();
    for i in 0..128 {
        source.push_str(&format!("struct L{i} {{}} impl M for L{i} {{}}\n"));
        level.push(format!("L{i}"));
    }
    let mut index = 0;
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| {
                let name = format!("N{index}");
                index += 1;
                source.push_str(&format!("type {name} = P<{},{}>\n", pair[0], pair[1]));
                name
            })
            .collect();
    }
    for overflow in [false, true] {
        let root = if overflow {
            format!("Q<{}>", level[0])
        } else {
            level[0].clone()
        };
        let source = format!("{source} fn test() -> <C<{root}> as S>::Item {{ 42 }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            if overflow {
                let error = vm.load_source(&source).unwrap_err();
                assert_eq!(error.code(), crate::ErrorCode::ResourceLimit, "{error}");
            } else {
                vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(vm.call_int32_0("test").unwrap(), 42);
            }
        }
        // Keep the exact obligation budget independent of standard-library templates.
        let mut types = crate::TypeRegistry::default();
        types.set_prelude_enabled(false);
        let ast = types.resolve_with_modules(
            &crate::parser::parse(&source).unwrap(),
            &crate::ModuleSources::new(),
        );
        if overflow {
            assert_eq!(ast.unwrap_err().code(), crate::ErrorCode::ResourceLimit);
        } else {
            #[cfg(unix)]
            crate::backends::aot::tests::assert_sync_int32(&ast.unwrap(), "test", 42);
        }
    }
}

#[test]
fn associated_types_support_blanket_impls_without_equality_specialization() {
    let source = r#"
        trait Marker {}
        trait Source { type Item
            fn get(&self) @noescape -> Self.Item
            fn doubled(&self) @noescape -> Int32 { 21 * 2 }
        }
        impl<T: Marker> Source for T { type Item = Int32
            fn get(&self) -> Int32 { 42 }
        }
        struct C {}
        impl Marker for C {}
        fn read<T: Marker>(x: &T) @noescape -> <T as Source>::Item { Source.get(x) }
        fn test() -> Int32 { let c = C {}; read(&c) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "test",
        42,
    );
    for source in [
        "trait S { type Item } struct C {} impl S for C { type Item = String } struct W<T> {} trait Use {} impl<T:S<Item=Int32>> Use for W<T> {} fn need<T:Use>() {} fn bad() { need::<W<C>>() }",
        "trait S { type Item } struct C {} impl S for C { type Item = Int32 } impl S for C { type Item = String }",
        "trait M {} trait S { type Item: M } fn bad<T:S<Item=Bool>>() {}",
        "trait S { type Item } fn bad<T:S<Item=Int32>,U:S<Item=Bool>>(x:T.Item,y:U.Item) { let z:Bool = x }",
        "trait S { type Item } fn only<T:S>(x:T.Item) {} fn bad() { only(42) }",
    ] {
        for mode in [VmMode::Jit,VmMode::Interpreter] { assert!(VmEngine::with_mode(mode).load_source(source).is_err(),"{source}"); }
        assert!(crate::aot::AotCompiler::new("bad-conditional-associated").unwrap().compile_module(&crate::parser::parse(source).unwrap()).is_err(),"{source}");
    }
}

#[test]
fn trait_defaults_keep_effect_contracts_and_implementation_privacy() {
    for source in [
        "trait S { fn view(x: &Int32) @noescape(x) -> &Int32 { x } }",
        "trait S { fn view(x: &Int32) @noescape(x) -> &Int32 { x } } struct C {} impl S for C { fn view(y:&Int32) -> &Int32 { y } }",
        "trait S { fn next(&self) -> Int32; fn read(&self) @noescape -> Int32 { self.next() } }",
        "trait S { fn run(&self) @suspends {} } struct C {} impl S for C { fn run(&self) {} }",
        "trait S { fn run(&self) {} } struct C {} impl S for C { fn run(&self) @suspends {} }",
        "trait S { type Item; fn next(&self) @noescape -> Self.Item } struct C { value:Int32 } impl S for C { type Item = &Int32; fn next(&self) -> &Int32 { &self.value } }",
        "trait S { type Item; fn next(&self) -> Self.Item } struct C { value:Int32 } impl S for C { type Item = &Int32; fn next(&self) -> &Int32 { &self.value } } fn bad() -> C.Item { let c = C { value:42 }; c.next() }",
    ] {
        for mode in [VmMode::Jit,VmMode::Interpreter] { assert!(VmEngine::with_mode(mode).load_source(source).is_err(),"{source}"); }
        assert!(crate::aot::AotCompiler::new("bad-default-effect").unwrap().compile_module(&crate::parser::parse(source).unwrap()).is_err(),"{source}");
    }
    let mut modules = crate::ModuleSources::new();
    modules.insert("protocol","fn helper() -> Int32 { 42 } pub trait S { fn read(&self) @noescape -> Int32 { helper() } }").unwrap();
    let bad = "import protocol struct C {} impl protocol.S for C { fn read(&self) -> Int32 { helper() } }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert!(VmEngine::with_mode(mode)
            .load_source_with_modules(bad, &modules)
            .is_err());
    }
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(bad).unwrap(), &modules)
        .unwrap();
    assert!(crate::aot::AotCompiler::new("private-default")
        .unwrap()
        .compile_module(&ast)
        .is_err());
}
