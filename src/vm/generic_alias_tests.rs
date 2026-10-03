//! Generic alias erasure and diagnostics across execution modes.
use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::ErrorCode;

#[test]
fn generic_alias_pending_snapshots_survive_reload_failure_and_cancellation() {
    use super::support::{parity_vm, EVENTS, READY};
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = r#"
        type PairOf<T> = (T, T)
        type Snapshot<T> = (PairOf<T>, Int32)
        extern fn delayed_value(n: Int32) @suspends -> Int32
        extern fn record(n: Int32) -> Int32
        fn original() -> PairOf<Int64> { (4294967295, -7) }
        fn save<T>(p: PairOf<T>, n: Int32) @suspends -> Snapshot<T> { (p, delayed_value(record(n))) }
        fn pending() @suspends -> Int32 {
            var p = original()
            let (saved, stamp) = save(p, { p = (9, 8) 7 })
            let (a, b) = saved
            let (c, d) = p
            if a == 4294967295 && b == -7 && c == 9 && d == 8 && stamp == 8 { 42 } else { 0 }
        }
        fn failing() @suspends -> Int32 { let (saved, stamp) = save(original(), -1) 42 / stamp + record(99) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        vm.load_source_with_modules(source, &crate::ModuleSources::new())
            .unwrap();
        let original_type = vm.function_signature("original").unwrap().ret;
        let mut pending = vm.call_async("pending", &[]).unwrap();
        let mut failed = vm.call_async("failing", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            pending.resume().unwrap();
            failed.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 7, 7]));
        vm.load_source("type Different<T> = (T, T) fn original() -> Different<Int64> { (1, 2) }")
            .unwrap();
        assert_eq!(
            original_type,
            vm.function_signature("original").unwrap().ret
        );
        scheduler.cancel(cancelled).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        pending.resume().unwrap();
        assert_eq!(pending.cps_result(), Some(42));
        let ResumeError::Failed(error) = failed.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        let span = error.span().unwrap();
        assert_eq!(
            span.slice(error.sources().get(span.source_id()).unwrap().text()),
            Some("42 / stamp")
        );
        assert_eq!(failed.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 7, 7]));
    }
}

#[test]
fn generic_aliases_preserve_transparent_inference_and_layouts_across_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/generic_aliases.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            vm.load_source(&source).unwrap();
            let answer = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(answer, 42, "{mode:?} suspends={suspends}");
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
fn generic_alias_errors_are_checked_when_unused_and_preserve_the_loaded_module() {
    for (body, code) in [
        ("type A<T> = Missing", ErrorCode::UnknownType),
        ("type A<T> = A<T>", ErrorCode::CyclicTypeAlias),
        ("type A<T> = A<(T,)>", ErrorCode::CyclicTypeAlias),
        ("type A<T> = B<T> type B<U> = A<U>", ErrorCode::CyclicTypeAlias),
        ("type A<T> = Drop<A<T>>", ErrorCode::CyclicTypeAlias),
        ("type A<T> = R<T> struct R<T> { value: A<T> }", ErrorCode::RecursiveRecord),
        ("type A<T> = E<T> enum E<T> { Value(A<T>) }", ErrorCode::RecursiveType),
        ("type A<T> = Drop<vm.Handle>", ErrorCode::TypeMismatch),
        ("type A<T> = T<Int32>", ErrorCode::TypeMismatch),
        ("type PairOf = Bool", ErrorCode::DuplicateDefinition),
        ("fn f(x: PairOf) {}", ErrorCode::TypeMismatch),
        ("fn f(x: PairOf<Int32, Bool>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Drop<vm.Handle>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Drop<vm.Handle>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Drop<Missing>) {}", ErrorCode::UnknownType),
        ("fn f(x: PairOf<vm.Handle>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Drop<Option<vm.Handle>>) {}", ErrorCode::TypeMismatch),
        ("type Plain = Int32 fn f(x: Plain<Bool>) {}", ErrorCode::UnknownType),
        ("fn f(x: PairOf<Int32>) -> PairOf<Bool> { x }", ErrorCode::TypeMismatch),
        ("fn bad<T>(x: PairOf<T>) -> Int32 { let (a, b) = x a + b }", ErrorCode::TypeMismatch),
        ("struct R<T> { value: T } type A<T> = R<T> fn f() { let a = A::<Int32> { value: 1 } }", ErrorCode::UnknownType),
        ("type A<T> = Option<T> fn f() { A.Some::<Int32>(1) }", ErrorCode::UnknownName),
        ("type A<T> = Option<T> fn f(x: A<Int32>) -> Int32 { match x { | A.None => 0 | A.Some(n) => n } }", ErrorCode::InvalidPattern),
        ("impl PairOf {}", ErrorCode::TypeMismatch),
        ("extern fn host(x: PairOf<Int32>)", ErrorCode::AbiError),
    ] {
        let source = format!("// 🙂\ntype PairOf<T> = (T, T) type Drop<T> = Int32 {body}");
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
        let error = crate::aot::AotCompiler::new("invalid_alias").unwrap().compile_module(&crate::parser::parse(&source).unwrap()).unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}
