//! Generic record inference, layout, and execution parity.
use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::ErrorCode;

#[test]
fn generic_records_preserve_inference_and_snapshots_across_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/generic_records.jk"),
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
fn generic_record_errors_leave_the_previous_module_published() {
    let declarations =
        "struct Pair<T> { left: T, right: T } struct Cell<T> { value: T } struct Marker<T> {} ";
    for (body, code) in [
        ("struct Bad<T> { text: vm.Handle }", ErrorCode::TypeMismatch),
        ("struct Bad<T> { value: Unknown }", ErrorCode::UnknownType),
        (
            "struct Bad<T> { value: T, value: T }",
            ErrorCode::DuplicateDefinition,
        ),
        ("struct Bad<T> { next: Bad<T> }", ErrorCode::RecursiveRecord),
        (
            "struct Bad<T> { next: Bad<(T,)> }",
            ErrorCode::RecursiveRecord,
        ),
        (
            "struct Bad<T> { next: Cell<Bad<T>> }",
            ErrorCode::RecursiveRecord,
        ),
        (
            "struct Bad<T> { next: Other<T> } struct Other<T> { next: Bad<T> }",
            ErrorCode::RecursiveRecord,
        ),
        (
            "struct Bad<T> { next: Alias } type Alias = Bad<Int32>",
            ErrorCode::RecursiveRecord,
        ),
        (
            "struct Bad<T> { next: Option<Bad<T>> }",
            ErrorCode::RecursiveRecord,
        ),
        ("fn f(x: Pair) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Pair<Int32, Bool>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Marker<vm.Handle>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Pair<vm.Handle>) {}", ErrorCode::TypeMismatch),
        ("fn f(x: Pair<Missing>) {}", ErrorCode::UnknownType),
        (
            "type Alias = Pair<Int32> fn f(x: Alias<Bool>) {}",
            ErrorCode::UnknownType,
        ),
        (
            "type Alias = Pair<Int32> fn f() { let p = Alias { left: 1, right: 2 } }",
            ErrorCode::UnknownType,
        ),
        ("fn f() { let p = Marker {} }", ErrorCode::TypeMismatch),
        (
            "fn f() { let p = Pair { left: 1, right: true } }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let p: Pair<Bool> = Pair { left: 1, right: 2 } }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let p = Pair::<Int32, Bool> { left: 1, right: 2 } }",
            ErrorCode::InvalidCall,
        ),
        (
            "fn f() { let p = Marker::<vm.Handle> {} }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let p: Marker<Int32> = Marker::<Bool> {} }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let p = Pair { left: 1 } }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let p = Pair { left: 1, left: 2, right: 3 } }",
            ErrorCode::DuplicateDefinition,
        ),
        (
            "fn f() { let p = Pair { bad: 1, right: 2 } }",
            ErrorCode::UnknownName,
        ),
        (
            "fn f() { let p: Pair<Int32> = Pair { left: 1, right: 2 } let q: Pair<Int64> = p }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let p = Pair { left: 1, right: 2 } p.left = 3 }",
            ErrorCode::InvalidAssignment,
        ),
        (
            "fn f() { var p = Pair { left: 1, right: 2 } p.left = true }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn bad<T>(p: Pair<T>) -> T { p.left + p.right }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn bad<T>(p: Pair<T>) { let q: Pair<Int32> = p }",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn bad<T>(p: T) { let q = T { value: p } }",
            ErrorCode::TypeMismatch,
        ),
        (
            "struct Plain {} fn f() { let p = Plain::<Int32> {} }",
            ErrorCode::InvalidCall,
        ),
        ("impl Pair {}", ErrorCode::TypeMismatch),
        ("extern fn host(p: Pair<Int32>)", ErrorCode::AbiError),
        (
            "extern fn host() @suspends -> Pair<Int32>",
            ErrorCode::AbiError,
        ),
    ] {
        let source = format!("// 🙂\n{declarations}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert!(
                error
                    .span()
                    .is_some_and(|span| span.slice(&source).is_some()),
                "{error}"
            );
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("bad_generic_record").unwrap();
        let error = aot
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn generic_record_limits_bound_instances_width_and_phantom_depth() {
    let mut instances = "struct Marker<T> {}".to_owned();
    for n in 0..257 {
        instances.push_str(&format!(" struct R{n} {{}} fn f{n}(x: Marker<R{n}>) {{}}"));
    }
    let mut wide = "struct Double<T> { a: T, b: T } type T0 = Int32".to_owned();
    for n in 1..=17 {
        wide.push_str(&format!(" type T{n} = Double<T{}>", n - 1));
    }
    let mut deep = "struct Marker<T> {} type T0 = Unit".to_owned();
    for n in 1..=257 {
        deep.push_str(&format!(" type T{n} = Marker<T{}>", n - 1));
    }
    for (source, message) in [(instances, "256 instances"), (wide, "65536"), (deep, "256")] {
        let error = VmEngine::new_interpreter()
            .load_source(&source)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains(message), "{error}");
    }
}

#[test]
fn generic_record_pending_values_survive_reload_failure_and_cancellation() {
    use super::support::{parity_vm, EVENTS, READY};
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = r#"
        struct Pair<T> { left: T, right: T }
        struct Frame<T> { value: T, stamp: Int32 }
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn original() -> Pair<Int64> { Pair { left: 4294967295, right: -7 } }
        fn build<T>(value: T, n: Int32) @suspends -> Frame<T> {
            Frame { stamp: delayed_value(record(n)), value: value }
        }
        fn pending() @suspends -> Int32 {
            var p = original()
            let frame = build(p, { p.left = 9 7 })
            if frame.value.left == 4294967295 && frame.value.right == -7
                && p.left == 9 && frame.stamp == 8 { 42 } else { 0 }
        }
        fn failing() @suspends -> Int32 {
            let frame = build(original(), -1)
            let p = Pair { left: 42 / frame.stamp, right: record(99) }
            p.left
        }
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
        vm.load_source("struct Pair<T> { left: T, right: T } fn original() -> Pair<Int64> { Pair { left: 1, right: 2 } }").unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert!(vm.call_int32_0("original").is_err());
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
            Some("42 / frame.stamp")
        );
        assert_eq!(failure.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 7, 7]));
    }
}
