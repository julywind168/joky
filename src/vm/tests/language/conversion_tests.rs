use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Type};

#[test]
fn integer_conversion_matrix_preserves_values_and_wrapping_across_backends() {
    let sources: [(&str, &[i128]); 5] = [
        (
            "Int32",
            &[i32::MIN as i128, -12345, -1, 0, 1, i32::MAX as i128],
        ),
        (
            "Int64",
            &[
                i64::MIN as i128,
                -4294967297,
                -2147483649,
                -1,
                0,
                1,
                2147483648,
                4294967296,
                i64::MAX as i128,
            ],
        ),
        ("UInt32", &[0, 1, 2147483647, 2147483648, u32::MAX as i128]),
        (
            "UInt64",
            &[
                0,
                1,
                2147483648,
                4294967295,
                4294967296,
                i64::MAX as i128,
                9223372036854775808,
                u64::MAX as i128,
            ],
        ),
        ("UInt8", &[0, 1, 127, 128, 254, 255]),
    ];
    let targets = ["Int32", "Int64", "UInt32", "UInt64", "UInt8"];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let attr = if suspends { "@suspends" } else { "" };
            let mut source = String::new();
            for (i, (src, values)) in sources.iter().enumerate() {
                for (j, dst) in targets.iter().enumerate() {
                    source.push_str(&format!(
                        "fn convert_{i}_{j}(n: {src}) {attr} -> {dst} {{ {}(n) }}\n",
                        dst.to_lowercase()
                    ));
                    for (k, value) in values.iter().enumerate() {
                        source.push_str(&format!("fn value_{i}_{j}_{k}() {attr} -> {dst} {{ convert_{i}_{j}({value}) }}\n"));
                    }
                }
            }
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap();
            for (i, (_, values)) in sources.iter().enumerate() {
                for (j, _) in targets.iter().enumerate() {
                    for (k, &value) in values.iter().enumerate() {
                        let name = format!("value_{i}_{j}_{k}");
                        macro_rules! check {
                            ($sync:ident, $async:ident, $ty:ty) => {{
                                let actual = if suspends {
                                    vm.$async(&name, &[]).unwrap().cps_result().unwrap() as $ty
                                } else {
                                    vm.$sync(&name).unwrap()
                                };
                                assert_eq!(
                                    actual, value as $ty,
                                    "{mode:?} suspends={suspends} {name}({value})"
                                );
                            }};
                        }
                        match j {
                            0 => check!(call_int32_0, call_async, i32),
                            1 => check!(call_int64_0, call_async_int64, i64),
                            2 => check!(call_uint32_0, call_async_uint32, u32),
                            3 => check!(call_uint64_0, call_async_uint64, u64),
                            4 => check!(call_uint8_0, call_async_uint8, u8),
                            _ => unreachable!(),
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn integer_conversion_contexts_aggregates_and_control_flow_match_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../../../tests/fixtures/conversions.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            let exports = vm.load_source(&source).unwrap();
            for name in ["int32", "int64", "uint32", "uint64"] {
                assert!(!exports.iter().any(|export| export == name));
            }
            let result = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(result, 42, "{mode:?} suspends={suspends}");
        }
    }
}

#[test]
fn integer_conversion_diagnostics_preserve_spans_and_failed_reload() {
    for (body, code, marked) in [
        ("fn f() { int32() }", ErrorCode::InvalidCall, "int32()"),
        (
            "fn f() { uint64(1, 2) }",
            ErrorCode::InvalidCall,
            "uint64(1, 2)",
        ),
        ("fn f() { int64(true) }", ErrorCode::TypeMismatch, "true"),
        (
            "fn f() { uint32(\"text\") }",
            ErrorCode::TypeMismatch,
            "\"text\"",
        ),
        ("fn f() { int32({}) }", ErrorCode::TypeMismatch, "{}"),
        (
            "fn f() { int32((1, 2)) }",
            ErrorCode::TypeMismatch,
            "(1, 2)",
        ),
        (
            "struct R { n: Int32 } fn f() { int32(R { n: 1 }) }",
            ErrorCode::TypeMismatch,
            "R { n: 1 }",
        ),
        (
            "enum E { A } fn f() { int32(E.A) }",
            ErrorCode::TypeMismatch,
            "E.A",
        ),
        (
            "fn f() -> Int64 { int32(1) }",
            ErrorCode::TypeMismatch,
            "fn f() -> Int64 { int32(1) }",
        ),
        (
            "fn f() { let n: Int64 = 1; int32(n) + n }",
            ErrorCode::TypeMismatch,
            "int32(n) + n",
        ),
        (
            "fn f() -> UInt64 { uint64(0xffff_ffff) }",
            ErrorCode::TypeMismatch,
            "0xffff_ffff",
        ),
        (
            "fn f() -> Int64 { int64(2147483648) }",
            ErrorCode::TypeMismatch,
            "2147483648",
        ),
        (
            "fn f() -> Int64 { int64(-9223372036854775808) }",
            ErrorCode::TypeMismatch,
            "-9223372036854775808",
        ),
        (
            "extern fn delay(n: Int32) @suspends -> Int32 fn f() { int64(delay(1)) }",
            ErrorCode::InvalidCall,
            "delay(1)",
        ),
        (
            "fn f() @suspends { parallel { | int32(1) } }",
            ErrorCode::InvalidCall,
            "int32(1)",
        ),
    ] {
        let source = format!("// 🙂\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            if source.contains("extern fn delay") {
                unsafe {
                    vm.register_suspending_fn(
                        "delay",
                        FunctionSignature::new(vec![Type::Int32], Type::Int32),
                        delay,
                    )
                    .unwrap();
                }
            }
            vm.load_source("fn current() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert_eq!(error.phase(), ErrorPhase::TypeCheck);
            assert_eq!(
                error.span().unwrap().slice(&source),
                Some(marked),
                "{source}: {error}"
            );
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("invalid_conversion").unwrap();
        let error = aot
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
        assert_eq!(error.span().unwrap().slice(&source), Some(marked));
    }
}

#[test]
fn integer_conversion_names_cannot_be_redefined_or_registered() {
    extern "C" fn host(n: i32) -> i32 {
        n
    }
    for name in ["int32", "int64", "uint32", "uint64"] {
        for prefix in ["fn", "extern fn"] {
            let body = if prefix == "fn" { " { n }" } else { "" };
            let source = format!("{prefix} {name}(n: Int32) -> Int32{body}");
            for mode in [VmMode::Jit, VmMode::Interpreter] {
                let mut vm = VmEngine::with_mode(mode);
                let error = vm.load_source(&source).unwrap_err();
                assert_eq!(error.code(), ErrorCode::ReservedName);
                assert_eq!(error.span().unwrap().slice(&source), Some(source.as_str()));
            }
            let mut aot = crate::aot::AotCompiler::new("reserved_conversion").unwrap();
            assert_eq!(
                aot.compile_module(&crate::parser::parse(&source).unwrap())
                    .unwrap_err()
                    .code(),
                ErrorCode::ReservedName
            );
        }
        let mut vm = VmEngine::new();
        let error = unsafe {
            vm.register_fn(
                name,
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                host as *const u8,
            )
        }
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::DuplicateDefinition);
    }
}

thread_local! {
    static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EVENTS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(n: u64) -> u64 {
    EVENTS.with(|events| events.borrow_mut().push(n));
    n
}
unsafe extern "C" fn delay(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        record(frame.slots[0]);
        frame.state = 1;
        return 1;
    }
    if !READY.with(std::cell::Cell::get) {
        return 1;
    }
    frame.result = frame.slots[0];
    0
}

#[test]
fn integer_conversions_preserve_pending_snapshots_failures_and_cancellation() {
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = r#"
        extern fn record(n: UInt64) -> UInt64
        extern fn delay(n: UInt64) @suspends -> UInt64
        fn pending(n: UInt64) @suspends -> UInt64 {
            var value = n
            let pair = (int32(value), uint32(delay({ value = 0; record(n) })))
            let (signed, unsigned) = pair
            let wide = int64(signed) + int64(unsigned)
            uint64(wide) ^ uint64(int32(delay(3))) ^ value
        }
        fn bad(n: UInt64) -> UInt64 { 1 << n }
        fn failure(n: UInt64) @suspends -> UInt64 {
            let value = int32(bad(delay(n)))
            record(uint64(value))
        }
        fn before(n: UInt64) -> UInt64 { uint64(int32(bad(n))) + record(99) }
        fn discarded(n: UInt64) -> UInt64 { int32(bad(n)); record(99) }
        fn lazy(n: UInt64) -> UInt64 { if true || int32(bad(n)) == 0 { 42 } else { 0 } }
        fn branch_task(n: UInt64) @suspends { record(uint64(uint32(delay(n)))); }
        fn scoped() @suspends -> UInt64 { parallel { | branch_task(uint64(-1)) | branch_task(uint64(42)) }; 42 }
        fn failed_branch(n: UInt64) @suspends { int32(bad(delay(n))); }
        fn failed_scope() @suspends -> UInt64 { parallel { | failed_branch(uint64(64)) | branch_task(uint64(7)) }; record(99) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record",
                FunctionSignature::new(vec![Type::UInt64], Type::UInt64),
                record as *const u8,
            )
            .unwrap();
            vm.register_suspending_fn(
                "delay",
                FunctionSignature::new(vec![Type::UInt64], Type::UInt64),
                delay,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        for name in ["before", "discarded"] {
            let error = vm.call_uint64_1(name, 64).unwrap_err();
            assert_eq!(error.span().unwrap().slice(source), Some("1 << n"));
        }
        assert_eq!(vm.call_uint64_1("lazy", 64).unwrap(), 42);
        EVENTS.with(|events| assert!(events.borrow().is_empty()));
        let high = 0x8000_0000_ffff_ffff;
        let mut task = vm.call_async_uint64("pending", &[high]).unwrap();
        let mut failed = vm.call_async_uint64("failure", &[64]).unwrap();
        let mut scoped = vm.call_async_uint64("scoped", &[]).unwrap();
        let mut failed_scope = vm.call_async_uint64("failed_scope", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async_uint64("pending", &[7]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failed.resume().unwrap();
            scoped.resume().unwrap();
            failed_scope.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        let initial = [high, high, 64, u64::MAX, 42, 64, 7, 7, 7];
        EVENTS.with(|events| assert_eq!(*events.borrow(), initial));
        scheduler.cancel(cancelled).unwrap();
        vm.load_source("fn pending(n: UInt64) @suspends -> UInt64 { uint64(int32(n)) }")
            .unwrap();
        assert_eq!(
            vm.call_async_uint64("pending", &[high])
                .unwrap()
                .cps_result(),
            Some(u64::MAX)
        );
        drop(vm);
        READY.with(|ready| ready.set(true));
        for _ in 0..3 {
            if !task.is_finished() {
                task.resume().unwrap();
            }
        }
        assert_eq!(task.cps_result(), Some(4294967294 ^ 3));
        for task in [&mut failed, &mut failed_scope] {
            let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                panic!()
            };
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert_eq!(error.span().unwrap().slice(source), Some("1 << n"));
            assert_eq!(task.cps_result(), None);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        }
        scoped.resume().unwrap();
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        let mut expected = initial.to_vec();
        expected.extend([3, u32::MAX as u64, 42]);
        EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
    }
}

#[test]
fn integer_conversion_rejects_opaque_values_even_with_integer_storage() {
    let source = "fn f(n: Host.Handle) -> Int32 { int32(n) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.register_opaque_type("Host.Handle").unwrap();
        let error = vm.load_source(source).unwrap_err();
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert_eq!(error.span().unwrap().slice(source), Some("n"));
    }
    let mut aot = crate::aot::AotCompiler::new("opaque_conversion").unwrap();
    aot.register_opaque_type("Host.Handle").unwrap();
    assert_eq!(
        aot.compile_module(&crate::parser::parse(source).unwrap())
            .unwrap_err()
            .code(),
        ErrorCode::TypeMismatch
    );
}
