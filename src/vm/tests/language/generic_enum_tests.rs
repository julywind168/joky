use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase};

#[test]
fn generic_enums_preserve_inference_layouts_and_matching_across_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../../../tests/fixtures/generic_enums.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            vm.load_source(&source).unwrap();
            let result = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(result, 42, "{mode:?} suspends={suspends}");
            let wide = if suspends {
                vm.call_async_uint64("wide", &[])
                    .unwrap()
                    .cps_result()
                    .unwrap()
            } else {
                vm.call_uint64_0("wide").unwrap()
            };
            assert_eq!(wide, u64::MAX);
        }
    }
}

#[test]
fn generic_enum_errors_reject_bad_templates_applications_and_constructors() {
    let prelude = "enum Option<T> { None, Some(T) } enum Result<T, E> { Ok(T), Err(E) } ";
    for (body, code) in [
        ("enum Bad<T> { Value(vm.Function) }", ErrorCode::TypeMismatch),
        ("enum Bad<T> { Value(Unknown) }", ErrorCode::UnknownType),
        ("enum Bad<T> { A, A(T) }", ErrorCode::DuplicateDefinition),
        ("enum Bad<T> {}", ErrorCode::InvalidPattern),
        ("enum Bad<T> { Next(Bad<T>) }", ErrorCode::RecursiveType),
        ("enum Bad<T> { Next(Bad<(T,)>) }", ErrorCode::RecursiveType),
        ("enum Bad<T> { Next(Other<T>) } enum Other<T> { Next(Bad<T>) }", ErrorCode::RecursiveType),
        ("enum Bad<T> { Next(Option<Bad<T>>) }", ErrorCode::RecursiveType),
        ("enum Bad<T> { Next(Alias) } type Alias = Bad<Int32>", ErrorCode::RecursiveType),
        ("fn f(x: Option) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Option<Int32, Bool>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Option<vm.Function>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Option<vm.Function>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Option<Unknown>) {}", ErrorCode::UnknownType),
        ("struct Pair {} fn f(x: Pair<Int32>) {}", ErrorCode::UnknownType),
        ("type Alias = Option<Int32> fn f(x: Alias<Bool>) {}", ErrorCode::UnknownType),
        ("fn f<T>(x: T<Int32>) {}", ErrorCode::TypeMismatch),
        ("fn f() { Option.None }", ErrorCode::TypeMismatch),
        ("fn f() { Result.Ok(42) }", ErrorCode::TypeMismatch),
        ("fn f() { Option.Some(\"text\") }", ErrorCode::TypeMismatch),
        ("fn f() { Option.None::<vm.Function> }", ErrorCode::TypeMismatch),
        ("fn f() { Option.Some::<Int32, Bool>(1) }", ErrorCode::InvalidCall),
        ("fn f() { let x: Option<Bool> = Option.Some(42) }", ErrorCode::TypeMismatch),
        ("fn f() { let x: Option<Int32> = Option.None::<Bool> }", ErrorCode::TypeMismatch),
        ("fn f() { Option.None::<Int32>() }", ErrorCode::InvalidCall),
        ("fn f() { Option.Some::<Int32> }", ErrorCode::InvalidCall),
        ("fn f() { Option.Some::<Int32>(1,2) }", ErrorCode::InvalidCall),
        ("fn f() { Option.Missing::<Int32> }", ErrorCode::UnknownName),
        ("fn f() { let a: Option<Int32> = Option.None; let b: Option<Bool> = a }", ErrorCode::TypeMismatch),
        ("fn bad<T>(x: Option<T>) -> T { match x { Some(n) => n + n, None => 0 } }", ErrorCode::TypeMismatch),
        ("fn bad<T>(x: Option<T>) -> Int32 { match x { None => 0 } }", ErrorCode::NonExhaustiveMatch),
        ("enum Other<T> { None, Some(T) } fn f(x: Option<Int32>) { match x { Other.None => {}, Some(n) => {} } }", ErrorCode::InvalidPattern),
        ("extern fn host(x: Option<Int32>)", ErrorCode::AbiError),
    ] {
        let source = format!("// 🙂\n{prelude}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
            vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert!(error.span().is_some(), "{source}: {error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("bad_generic_enum").unwrap();
        aot.set_prelude_enabled(false).unwrap();
        let error = aot.compile_module(&crate::parser::parse(&source).unwrap()).unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(matches!(error.phase(), ErrorPhase::TypeCheck | ErrorPhase::Abi));
    }
}

#[test]
fn generic_enum_instance_limits_count_nominal_and_phantom_arguments() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut source = "enum Marker<T> { Value }".to_owned();
        for n in 0..257 {
            source.push_str(&format!(" struct R{n} {{}} fn f{n}(x: Marker<R{n}>) {{}}"));
        }
        let error = VmEngine::with_mode(mode).load_source(&source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("256 instances"));
    }
}

#[test]
fn generic_enum_recursion_obeys_interpreter_frame_and_poll_limits() {
    use super::InterpreterLimits;
    use crate::runtime::suspension::ResumeError;
    let source = "enum Option<T> { None, Some(T) }
        fn descend<T>(x: Option<T>, n: Int32) @suspends -> Option<T> { if n == 0 { x } else { descend(x, n - 1) } }
        fn run(n: Int32) @suspends -> Int32 { match descend(Option.Some(42), n) { None => 0, Some(x) => x } }";
    let mut vm = VmEngine::new_interpreter();
    vm.set_prelude_enabled(false);
    vm.load_source(source).unwrap();
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 64,
        instruction_budget: Some(5000),
        poll_budget: Some(1),
    })
    .unwrap();
    let mut task = vm.call_async("run", &[20]).unwrap();
    let mut polls = 0;
    while !task.is_finished() {
        task.resume().unwrap();
        polls += 1;
        assert!(polls < 5000);
    }
    assert!(polls > 20);
    assert_eq!(task.cps_result(), Some(42));
    for limits in [
        InterpreterLimits {
            max_frames: 4,
            instruction_budget: None,
            poll_budget: Some(1),
        },
        InterpreterLimits {
            max_frames: 64,
            instruction_budget: Some(20),
            poll_budget: Some(1),
        },
    ] {
        vm.set_interpreter_limits(limits).unwrap();
        let mut task = vm.call_async("run", &[20]).unwrap();
        for _ in 0..5000 {
            if let Err(ResumeError::Failed(error)) = task.resume() {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                break;
            }
            assert!(!task.is_finished());
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}

#[test]
fn generic_enum_layout_limits_bound_inline_width_and_phantom_depth() {
    for width in [false, true] {
        let mut source = if width {
            "enum Double<T> { Value((T, T)) } type T0 = Int32".to_owned()
        } else {
            "enum Phantom<T> { Value } type T0 = Unit".to_owned()
        };
        for n in 1..=if width { 16 } else { 257 } {
            source.push_str(&format!(
                " type T{n} = {}<T{}>",
                if width { "Double" } else { "Phantom" },
                n - 1
            ));
        }
        let error = VmEngine::new_interpreter()
            .load_source(&source)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error
            .message()
            .contains(if width { "65536" } else { "256" }));
        assert!(error.span().is_some());
    }
}

#[test]
fn generic_enum_constructors_preserve_context_scope_and_evaluation_order() {
    let source = r#"
        enum Option<T> { None, Some(T) }
        fn unwrap<T>(x: Option<T>, fallback: T) -> T { match x { None => fallback, Some(n) => n } }
        fn run() -> Int32 {
            let typed: Option<Int64> = Option.Some(4294967295)
            let matched: Option<Int64> = match Option.Some(1) { None => Option.None, Some(n) => Option.Some(4294967295) }
            let branch_task: Option<Int64> = if true { Option.None } else { Option.Some(4294967295) }
            let defaulted = if true { Option.None } else { Option.Some(42) }
            let matched_default = match Option.Some(1) { None => Option.None, Some(n) => Option.Some(42) }
            let a = unwrap(Option.Some((let n: Int64 = 7; n)), n)
            let b = unwrap::<Option<Int64>>(Option.Some(Option.None), typed)
            if unwrap(typed, 0) == 4294967295 && unwrap(matched, 0) == 4294967295 &&
                unwrap(branch_task, 9) == 9 && a == 7 && unwrap(b, 8) == 8 &&
                unwrap(defaulted, 42) == 42 && unwrap(matched_default, 0) == 42 { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
        vm.load_source("import joky/vm as runtime enum vm<T> { call_int32(T), register_int32(T) } fn run() -> Int32 { match vm.call_int32::<Int32>(42) { call_int32(n) => n, register_int32(n) => n } }").unwrap();
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
        let shadowed =
            "enum Option<T> { None, Some(T) } fn f<Option>(x: Option) { Option.Some(x); }";
        assert_eq!(
            vm.load_source(shadowed).unwrap_err().code(),
            ErrorCode::TypeMismatch
        );
        let field = "struct R { n: Int32 } fn f() { var r = R { n: 1 }; r.n::<Int32> = 2 }";
        assert_eq!(
            vm.load_source(field).unwrap_err().code(),
            ErrorCode::InvalidAssignment
        );
    }
}
