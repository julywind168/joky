//! Generic inherent member specialization and execution parity.
use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::ErrorCode;

#[test]
fn generic_impls_preserve_self_and_inference_across_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/generic_impls.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            let exports = vm.load_source(&source).unwrap();
            assert!(!exports.iter().any(|name| name.contains('.')));
            let result = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(result, 42, "{mode:?}, suspends={suspends}");
            let wide = if suspends {
                vm.call_async_uint64("wide", &[])
                    .unwrap()
                    .cps_result()
                    .unwrap()
            } else {
                vm.call_uint64_0("wide").unwrap()
            };
            assert_eq!(wide, u64::MAX);
            assert_eq!(vm.function_signature("Pair.first"), None);
        }
    }
}

#[test]
fn generic_impl_errors_preserve_the_previous_module() {
    let declarations = "struct Pair<T> { left: T right: T } struct Split<A, B> { first: A second: B } struct Marker<T> {} enum Choice<T> { None Some(T) } ";
    for (body, code) in [
        ("impl<T> Int32 {}", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { fn f(value: T) -> T { value } } fn bad<Pair>() { Pair::<Int32>.f(42) }", ErrorCode::TypeMismatch),
        ("fn bad<Choice>() { Choice::<Int32>.Some(42) }", ErrorCode::TypeMismatch),
        ("fn bad<Choice>() { Choice.None::<Int32> }", ErrorCode::TypeMismatch),
        ("impl<T> T {}", ErrorCode::TypeMismatch),
        ("impl<T> Marker<T> { const SIZE: Int32 = 0 } fn bad() { Marker::<String>.SIZE }", ErrorCode::TypeMismatch),
        ("impl<T> Marker<T> { const SIZE: Int32 = 0 } fn bad() { Marker::<Int32, Bool>.SIZE }", ErrorCode::TypeMismatch),
        ("impl<T> Marker<T> { fn make() -> Self { Self {} } } fn bad() { Marker::<String>.make() }", ErrorCode::TypeMismatch),
        ("impl<T> Marker<T> { fn make() -> Self { Self {} } } fn bad() { Marker::<Missing>.make() }", ErrorCode::UnknownType),
        ("impl<T> Pair<Int32> {}", ErrorCode::TypeMismatch),
        ("impl<T, U> Split<U, T> {}", ErrorCode::TypeMismatch),
        ("impl<T, U> Split<T, T> {}", ErrorCode::TypeMismatch),
        ("impl<T> Split<T, Int32> {}", ErrorCode::TypeMismatch),
        ("impl<T, U> Pair<T> {}", ErrorCode::TypeMismatch),
        ("type Alias<T> = Pair<T> impl<T> Alias<T> {}", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { fn bad(self) -> T { self.left + self.right } }", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { fn bad(self) -> Int32 { self.left } }", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { fn bad(self) -> T { self.left.no_method() } }", ErrorCode::UnknownName),
        ("impl<T> Pair<T> { fn first(self) -> T { self.left } } fn outside(value: T) {}", ErrorCode::UnknownType),
        ("impl<T> Pair<T> { fn f(self) {} } impl<U> Pair<U> { fn f(self) {} }", ErrorCode::DuplicateDefinition),
        ("impl<T> Pair<T> { fn f(self) {} const f: Int32 = 1 }", ErrorCode::DuplicateDefinition),
        ("impl<T> Choice<T> { fn Some(value: T) -> Self { Self.Some(value) } }", ErrorCode::DuplicateDefinition),
        ("impl<T> Pair<T> { const BAD: T = 1 }", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { const BAD: Int32 = true }", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { const A: Int32 = Self.B const B: Int32 = Pair.A }", ErrorCode::InvalidState),
        ("impl<T> Pair<T> { const A: Int32 = Pair::<Int32>.A }", ErrorCode::InvalidState),
        ("impl<T> Pair<T> { const A: Int32 = Self.MISSING }", ErrorCode::UnknownName),
        ("impl<T> Marker<T> { fn make() -> Self { Self {} } } fn f() { Marker.make() }", ErrorCode::TypeMismatch),
        ("impl<T> Marker<T> { fn make() -> Self { Self {} } } fn f() { Marker.make::<String>() }", ErrorCode::TypeMismatch),
        ("impl<T> Marker<T> { fn make() -> Self { Self {} } } fn f() { Marker.make::<Int32, Bool>() }", ErrorCode::InvalidCall),
        ("impl<T> Pair<T> { fn f(self, value: T) {} } fn f() { Pair { left: 1 right: 2 }.f(true) }", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { fn f(self) {} } fn f() { Pair { left: 1 right: 2 }.f::<Int32>() }", ErrorCode::InvalidCall),
        ("impl<T> Pair<T> { fn f(self) {} } fn f() { Pair.f::<Int32>() }", ErrorCode::InvalidCall),
        ("impl<T> Pair<T> { fn f(self) {} } fn f() { Pair::<Int32>.f::<Int32>(Pair { left: 1 right: 2 }) }", ErrorCode::InvalidCall),
        ("impl<T> Pair<T> { fn f(value: T) -> T { value } } fn f() { Pair { left: 1 right: 2 }.f(1) }", ErrorCode::InvalidCall),
        ("impl<T> Pair<T> { fn f(self) @suspends {} } fn f() { Pair { left: 1 right: 2 }.f() }", ErrorCode::InvalidCall),
    ] {
        let source = format!("// 🙂\n{declarations}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert!(error.span().is_some_and(|span| span.slice(&source).is_some()), "{error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("bad_generic_impl").unwrap();
        let error = aot.compile_module(&crate::parser::parse(&source).unwrap()).unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn generic_impl_parser_keeps_parameter_scope_and_self_arguments() {
    use crate::parser::ast::{Expr, Item, Type};
    let source = "// 🙂\nimpl<T,> Pair<T> { fn make(value: T) -> Self { Self { left: value right: value } } fn get(self) -> T { Self.first(self) } } fn outside(value: T) {}";
    let ast = crate::parser::parse(source).unwrap();
    let implementation = &ast.implementations[0];
    assert_eq!(implementation.target_name, "Pair");
    let parameter = &implementation.type_params[0];
    assert_eq!(parameter.span.slice(source), Some("T"));
    for method in &implementation.methods {
        assert!(method.type_params.is_empty());
    }
    let make = &implementation.methods[0];
    assert!(matches!(&make.params[0].ty, Type::Parameter(p) if p == parameter));
    assert_eq!(make.ret_ty, implementation.target);
    assert_eq!(
        implementation.methods[1].params[0].ty,
        implementation.target
    );
    let Expr::Block { exprs, .. } = &make.body else {
        panic!()
    };
    let Expr::Record {
        name,
        type_args,
        span,
        ..
    } = &exprs[0]
    else {
        panic!()
    };
    assert_eq!(name, "Pair");
    assert!(matches!(&type_args[0], Type::Parameter(p) if p == parameter));
    assert_eq!(
        span.slice(source),
        Some("Self { left: value right: value }")
    );
    let Item::FnDef(outside) = &ast.items[0] else {
        panic!()
    };
    assert!(matches!(&outside.params[0].ty, Type::Named(name) if name == "T"));
    for invalid in [
        "impl<> Pair<Int32> {}",
        "impl<T, T> Pair<T> {}",
        "impl<_> Pair<_> {}",
        "impl<Self> Pair<Self> {}",
        "impl<T: Copy> Pair<T> {}",
        "impl<T> Pair<T> { fn f<U>(self, value: U) {} }",
        "impl<T> Pair<T> { fn f(self) { Self::<T>.f(self) } }",
        "impl<T> Pair<T> { fn f(Self: T) {} }",
        "impl Pair<Int32> {}",
        "impl<T> (T, T) {}",
    ] {
        assert!(crate::parser::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn generic_impl_pending_receivers_survive_reload_failure_and_cancellation() {
    use super::support::{parity_vm, EVENTS, READY};
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = r#"
        struct Pair<T> { left: T right: T }
        struct Frame<T> { value: T stamp: Int32 }
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn original() -> Pair<Int64> { Pair { left: 4294967295 right: -7 } }
        impl<T> Pair<T> {
            fn build(self, n: Int32) @suspends -> Frame<Self> {
                Frame { stamp: delayed_value(record(n)) value: self }
            }
            fn first(self) -> T { self.left }
        }
        impl<T> Frame<T> {
            fn fail(self) -> Int32 { 42 / self.stamp }
        }
        fn pending() @suspends -> Int32 {
            var p = original()
            let frame = p.build({ p.left = 9 7 })
            if frame.value.first() == 4294967295 && frame.value.right == -7
                && p.first() == 9 && frame.stamp == 8 { 42 } else { 0 }
        }
        fn failing() @suspends -> Int32 { original().build(-1).fail() + record(99) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        vm.load_source_with_modules(source, &crate::ModuleSources::new())
            .unwrap();
        let old_type = vm.function_signature("original").unwrap().ret;
        let mut pending = vm.call_async("pending", &[]).unwrap();
        let mut failure = vm.call_async("failing", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            pending.resume().unwrap();
            failure.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 7, 7]));
        vm.load_source("struct Pair<T> { left: T right: T } impl<T> Pair<T> { fn first(self) -> T { self.right } } fn original() -> Pair<Int64> { Pair { left: 1 right: 2 } } fn fresh() -> Int32 { int32(original().first()) }").unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert_eq!(vm.call_int32_0("fresh").unwrap(), 2);
        scheduler.cancel(cancelled).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        pending.resume().unwrap();
        assert_eq!(pending.cps_result(), Some(42));
        let ResumeError::Failed(error) = failure.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        let span = error.span().unwrap();
        assert_eq!(
            span.slice(error.sources().get(span.source_id()).unwrap().text()),
            Some("42 / self.stamp")
        );
        assert_eq!(failure.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 7, 7]));
    }
}

#[test]
fn generic_impl_specialization_reuses_instances_and_bounds_growth() {
    let mut reused = "struct Marker<T> {} impl<T> Marker<T> { fn count(self) -> Int32 { 1 } } fn check() -> Int32 { let marker = Marker::<Bool> {}".to_owned();
    for _ in 0..260 {
        reused.push_str(" marker.count();");
    }
    reused.push_str(" 42 }");
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&reused).unwrap();
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);

    let mut many = "struct Marker<T> {} impl<T> Marker<T> {".to_owned();
    for index in 0..257 {
        many.push_str(&format!(" fn f{index}(self) {{}}"));
    }
    many.push_str(" } fn check() { let marker = Marker::<Bool> {}");
    for index in 0..257 {
        many.push_str(&format!(" marker.f{index}();"));
    }
    many.push_str(" }");
    let growth = "struct Marker<T> {} impl<T> Marker<T> { fn grow(self) { Marker::<(T,)> {}.grow() } } fn start() { Marker::<Bool> {}.grow() }";
    for source in [&many, growth] {
        let error = vm.load_source(source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit, "{error}");
        assert!(
            error
                .message()
                .contains("256 instances or 32 dependency levels"),
            "{error}"
        );
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
}

#[test]
fn generic_impl_normalization_preserves_parameters_and_nominal_identity() {
    let registry = crate::TypeRegistry::new().with_prelude(false);
    let parsed =
        crate::parser::parse(include_str!("../../tests/fixtures/generic_impls.jk")).unwrap();
    let once = registry.resolve_aliases(&parsed).unwrap();
    let twice = registry.resolve_aliases(&once).unwrap();
    for (a, b) in once.implementations.iter().zip(&twice.implementations) {
        assert_eq!(a.target, b.target);
        assert_eq!(a.type_params, b.type_params);
        for (a, b) in a.methods.iter().zip(&b.methods) {
            assert!(b.type_params.is_empty());
            assert_eq!(a.ret_ty, b.ret_ty);
            for (a, b) in a.params.iter().zip(&b.params) {
                assert_eq!(a.ty, b.ty);
            }
        }
    }
    let mut vm = VmEngine::new_interpreter();
    vm.set_prelude_enabled(false);
    vm.load_module(&twice).unwrap();
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    // A public AST caller cannot use an uninstantiated generic template as a
    // concrete impl target, bypassing the source syntax's arity check.
    let mut invalid = twice.clone();
    let implementation = &mut invalid.implementations[0];
    let crate::Type::Record(record) = &implementation.target else {
        panic!()
    };
    implementation.target = crate::Type::Record(record.template.clone().unwrap());
    implementation.type_params.clear();
    assert_eq!(
        registry.resolve_aliases(&invalid).unwrap_err().code(),
        ErrorCode::TypeMismatch
    );
}
