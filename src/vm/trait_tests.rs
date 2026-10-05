use super::{VmEngine, VmMode};

#[test]
fn static_traits_execute_borrowed_requirements_and_conditional_impls() {
    let source = r#"
        trait Eq { fn eq(&self, other: &Self) -> Bool }
        struct Id { value: Int32 }
        impl Eq for Id { fn eq(&self, other: &Self) -> Bool { self.value == other.value } }
        struct Pair<T> { left: T right: T }
        impl<T: Eq> Eq for Pair<T> {
            fn eq(&self, other: &Self) -> Bool {
                let left = self.left let right = other.left
                let a = alloc(left) let b = alloc(right)
                Eq.eq(a, b)
            }
        }
        fn same<T: Eq>(left: &T, right: &T) -> Bool { left.eq(right) }
        fn main() -> Int32 {
            let a = alloc(Pair { left: Id { value: 7 } right: Id { value: 8 } })
            let b = alloc(Pair { left: Id { value: 7 } right: Id { value: 9 } })
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
        "trait T { fn f(self) {} }",
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
        ("base", "pub trait Read { fn read(self) -> Int32 } pub struct Id { value: Int32 } fn helper(x: Int32) -> Int32 { x } impl Read for Id { fn read(self) -> Int32 { helper(self.value) } } pub fn read<T: Read>(x: T) -> Int32 { x.read() } trait Hidden { fn hidden(self) -> Int32 } impl Hidden for Id { fn hidden(self) -> Int32 { self.value } } pub fn private_bound<T: Hidden>(x: T) -> Int32 { x.hidden() }"),
        ("left", "import base pub fn make() -> base.Id { base.Id { value: 20 } }"),
        ("right", "import base pub fn make() -> base.Id { base.Id { value: 22 } }"),
        ("foreign", "import base pub struct Value { n: Int32 } impl base.Read for Value { fn read(self) -> Int32 { self.n } }"),
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
            let a = left.make() let z = right.make()
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
        fn view<T: View>(x: &T) -> &T { x.view() }
    "#;
    for (extra, message) in [
        ("fn bad() { let x = Id { value: 1 } x.set(42) }", "var binding"),
        ("fn bad() { var x = Id { value: 1 } let r = x.view() x.set(42) r.read(); }", "borrow"),
        ("fn bad() { var x = Id { value: 1 } x.set(x.read()) }", "borrow"),
        ("fn bad() -> &Id { let x = Id { value: 1 } view(&x) }", "local"),
        ("fn bad<T: View>(x: T) -> &T { view(&x) }", "local"),
        ("fn bad<T: Read>(x: T) -> Int32 { x.read() }", "retained"),
        ("fn bad() { let x = share(alloc(Id { value: 1 })) x.set(42) }", "Argument type mismatch"),
        ("fn pause() @suspends {} fn bad() @suspends { let x = Id { value: 1 } let r = &x pause() r.read(); }", "suspension"),
        ("trait Save { fn save(&self) } struct Holder { p: &Id } impl Save for Id { fn save(&self) { alloc(Holder { p: self }); } } fn save<T: Save>(x: &T) { x.save() } fn bad() { let x = Id { value: 1 } save(&x) }", "retained"),
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
        fn concrete() @suspends -> Int32 { let x = Id { value: 42 } x.read() }
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
            "pub struct Id { value: Int32 } impl Id { fn read(self) -> Int32 { 999 } }",
        )
        .unwrap();
    modules
        .insert(
            "extension",
            r#"
        import base
        pub trait Read { fn read(&self) -> Int32 }
        impl Read for base.Id { fn read(&self) -> Int32 { self.value } }
        pub fn check() -> Int32 { let x = base.Id { value: 42 } x.read() }
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
    assert_eq!(implementation.type_params[0].bounds, ["Eq", "Mark"]);
    assert_eq!(implementation.type_params[0].span.slice(source), Some("T"));
    let Item::FnDef(function) = &module.items[0] else {
        panic!()
    };
    assert_eq!(function.type_params[0].bounds, ["Eq"]);
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
