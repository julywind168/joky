//! Method-local Copy parameters, inference, and specialization parity.
use super::{VmEngine, VmMode};
use crate::ErrorCode;

#[test]
fn generic_library_method_arguments_remain_eager_when_pending_or_failing() {
    use super::support::{parity_vm, EVENTS, READY};
    use crate::runtime::suspension::ResumeError;
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn optional() @suspends -> Int32 {
            let value = Option.None::<Bool>.zip(Option.Some(delayed_value(record(7))))
            match value { None => 42, Some(pair) => 0 }
        }
        fn failing() @suspends -> Int32 {
            let err: Result<Bool, Int32> = Err(5)
            let value = err.and::<Int32>(Ok(42 / delayed_value(record(-1))))
            record(99)
            match value { Ok(n) => n, Err(code) => code }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        vm.load_source_with_modules(source, &crate::ModuleSources::new())
            .unwrap();
        let mut optional = vm.call_async("optional", &[]).unwrap();
        let mut failing = vm.call_async("failing", &[]).unwrap();
        for _ in 0..3 {
            optional.resume().unwrap();
            failing.resume().unwrap();
            assert_eq!(optional.cps_result(), None);
            assert_eq!(failing.cps_result(), None);
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1]));
        READY.with(|ready| ready.set(true));
        optional.resume().unwrap();
        assert_eq!(optional.cps_result(), Some(42));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        let span = error.span().unwrap();
        assert_eq!(
            span.slice(error.sources().get(span.source_id()).unwrap().text()),
            Some("42 / delayed_value(record(-1))")
        );
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1]));
    }
}

#[test]
fn generic_method_instances_reuse_complete_arguments_and_bound_growth() {
    let mut reused = "struct Marker<T> {} impl<T> Marker<T> { fn count<U>(self, value: U) -> Int32 { 1 } } fn check() -> Int32 { let marker = Marker::<Bool> {};".to_owned();
    for _ in 0..4097 {
        reused.push_str(" marker.count(true);");
    }
    reused.push_str(" 42 }");
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&reused).unwrap();
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    let mut many =
        "struct Marker<T> {} impl<T> Marker<T> { fn keep<U>(self, value: U) -> U { value } }"
            .to_owned();
    for n in 0..4097 {
        many.push_str(&format!(" struct Item{n} {{}}"));
    }
    many.push_str(" fn check() { let marker = Marker::<Bool> {};");
    for n in 0..4097 {
        many.push_str(&format!(" marker.keep(Item{n} {{}});"));
    }
    many.push_str(" }");
    let growth = "struct Marker<T> {} impl<T> Marker<T> { fn grow<U>(self, value: U) { self.grow((value,)) } } fn start() { Marker::<Bool> {}.grow(1) }";
    for (source, diagnostic) in [
        (many.as_str(), "Generated function limit 4096 exceeded"),
        (growth, "32 dependency levels (requested depth 33)"),
    ] {
        let error = vm.load_source(source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit, "{error}");
        assert!(error.message().contains(diagnostic));
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
}

#[test]
fn generic_method_errors_keep_the_previous_module() {
    let declarations = "struct Pair<T> { value: T } struct Marker<T> {} ";
    let methods = "impl<T> Pair<T> { fn pair<U>(self, value: U) -> (T, U) { (self.value, value) } fn same<U>(self, a: U, b: U) -> U { a } fn phantom<U>(self) -> Marker<U> { Marker {} } fn make<U>(a: T, b: U) -> Self { Self { value: a } } } ";
    for (body, code) in [
        ("impl<T> Pair<T> { fn bad<U>(self, value: U) -> T { value } }", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { fn bad<U>(self, value: U) -> U { value + value } }", ErrorCode::TypeMismatch),
        ("impl<T> Pair<T> { fn bad<U>(self, value: U) { value.no_method() } }", ErrorCode::UnknownName),
        ("impl<T> Pair<T> { fn one<U>(self, value: U) {} fn two(self, value: U) {} }", ErrorCode::UnknownType),
        ("impl<T> Pair<T> { fn one<U>(self, value: U) {} const BAD: U = 1 }", ErrorCode::TypeMismatch),
        ("fn f(value: U) {}", ErrorCode::UnknownType),
        ("fn f() { Pair { value: 1 }.pair::<Int32, Bool>(true) }", ErrorCode::InvalidCall),
        ("fn f() { Pair { value: 1 }.pair::<Bool>(42) }", ErrorCode::TypeMismatch),
        ("fn f() { Pair { value: 1 }.pair::<String>(\"bad\") }", ErrorCode::TypeMismatch),
        ("fn f() { Pair { value: 1 }.phantom::<String>() }", ErrorCode::TypeMismatch),
        ("fn f() { let result = Pair { value: 1 }.phantom() }", ErrorCode::TypeMismatch),
        ("fn f() { Pair { value: 1 }.same(42, true) }", ErrorCode::TypeMismatch),
        ("fn f() { Pair::<Int32>.make::<Int32, Bool>(42, true) }", ErrorCode::InvalidCall),
        ("fn f() { Pair.make::<Int32>(42, true) }", ErrorCode::InvalidCall),
        ("fn f() { Pair.make::<Int32, Bool, Unit>(42, true) }", ErrorCode::InvalidCall),
        ("fn f() { Pair::<Int32>.pair::<Bool>(Pair { value: false }, true) }", ErrorCode::TypeMismatch),
        ("struct P {} impl P { fn suspended<U>(self, value: U) @suspends -> U { value } } fn f() { (P {}).suspended(42) }", ErrorCode::InvalidCall),
        ("fn free<T, U>(a: T, b: U) {} fn f() { free::<Int32>(42, true) }", ErrorCode::InvalidCall),
    ] {
        let source = format!("// 🙂\n{declarations}{methods}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn current() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{body}: {error}");
            assert!(error.span().is_some_and(|span| span.slice(&source).is_some()), "{error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("bad_generic_method").unwrap();
        let error = aot.compile_module(&crate::parser::parse(&source).unwrap()).unwrap_err();
        assert_eq!(error.code(), code, "{body}: {error}");
    }
}
