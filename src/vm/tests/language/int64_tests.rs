use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Type};

#[test]
fn int64_contexts_aggregates_and_arithmetic_match_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../../../tests/fixtures/int64.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap();
            let check = if suspends {
                vm.call_async("check", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("check").unwrap()
            };
            assert_eq!(check, 42, "{mode:?} suspends={suspends}");
            for (name, expected) in [
                ("max", i64::MAX),
                ("min", i64::MIN),
                ("literals", 3000000042),
                ("contexts", 34000000000),
                ("aggregate_calls", 11000000000),
                ("operand_context", 25000000001),
                ("loops", 3000000008),
                ("snapshots", 9000000003),
            ] {
                let actual = if suspends {
                    vm.call_async_int64(name, &[])
                        .unwrap()
                        .cps_result()
                        .unwrap() as i64
                } else {
                    vm.call_int64_0(name).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            for (a, b) in [
                (i64::MIN, -1),
                (i64::MAX, 2),
                (-7, 3),
                (7, -3),
                (i64::MIN, 1),
                (i64::MIN, i64::MAX),
                (4000000000, -3000000000),
            ] {
                for (name, expected) in [
                    ("add", a.wrapping_add(b)),
                    ("sub", a.wrapping_sub(b)),
                    ("mul", a.wrapping_mul(b)),
                    ("divide", a.wrapping_div(b)),
                ] {
                    let actual = if suspends {
                        vm.call_async_int64(name, &[a, b])
                            .unwrap()
                            .cps_result()
                            .unwrap() as i64
                    } else {
                        vm.call_int64_2(name, a, b).unwrap()
                    };
                    assert_eq!(actual, expected, "{mode:?} {name}({a},{b})");
                }
            }
            for a in [i64::MIN, i64::MAX, 4000000000, -4000000000, 0] {
                for b in [i64::MIN, i64::MAX, 4000000000, -4000000000, 0] {
                    let expected = i64::from(a == b)
                        + 2 * i64::from(a != b)
                        + 4 * i64::from(a < b)
                        + 8 * i64::from(a <= b)
                        + 16 * i64::from(a > b)
                        + 32 * i64::from(a >= b);
                    let actual = if suspends {
                        vm.call_async_int64("ordered", &[a, b])
                            .unwrap()
                            .cps_result()
                            .unwrap() as i64
                    } else {
                        vm.call_int64_2("ordered", a, b).unwrap()
                    };
                    assert_eq!(actual, expected, "{mode:?} ordered({a},{b})");
                }
            }
            for n in [i64::MIN, i64::MAX, 4000000000, -4000000000, 0] {
                let actual = if suspends {
                    vm.call_async_int64("negate", &[n])
                        .unwrap()
                        .cps_result()
                        .unwrap() as i64
                } else {
                    vm.call_int64_1("negate", n).unwrap()
                };
                assert_eq!(actual, n.wrapping_neg());
            }
            assert!(vm.call_int32_0("max").is_err());
            assert!(vm.call_int64_0("check").is_err());
            assert!(vm.call_async("max", &[]).is_err());
            assert!(vm.call_async_int64("check", &[]).is_err());
        }
    }
}

#[test]
fn int64_rejects_overflow_and_implicit_conversion_before_publication() {
    let cases = [
        (
            "fn f() { let n = 2147483648 }",
            "outside Int32 range",
            "2147483648",
        ),
        (
            "fn f() -> Int32 { -2147483649 }",
            "outside Int32 range",
            "-2147483649",
        ),
        (
            "fn f() -> Int32 { -(2147483648) }",
            "outside Int32 range",
            "(2147483648)",
        ),
        (
            "fn f() -> Int64 { 9223372036854775808 }",
            "outside Int64 range",
            "9223372036854775808",
        ),
        (
            "fn f() -> Int64 { -(9223372036854775808) }",
            "outside Int64 range",
            "(9223372036854775808)",
        ),
        (
            "fn f() -> Int64 { let n = 42; n }",
            "Return type mismatch",
            "fn f() -> Int64 { let n = 42; n }",
        ),
        (
            "fn f() { let n = 42; let wide: Int64 = n }",
            "annotation mismatch",
            "let wide: Int64 = n",
        ),
        (
            "fn f() -> Int64 { let n = 42; n + 1 }",
            "Return type mismatch",
            "fn f() -> Int64 { let n = 42; n + 1 }",
        ),
        (
            "fn f(a: Int32, b: Int64) -> Int64 { a + b }",
            "same type",
            "a + b",
        ),
        (
            "fn f(a: Int32, b: Int64) -> Bool { a == b }",
            "same type",
            "a == b",
        ),
        (
            "fn take(a: Int64) {} fn f(n: Int32) { take(n) }",
            "Argument type mismatch",
            "n",
        ),
        (
            "fn f() { var n = 1; let wide: Int64 = 1; n = wide }",
            "Assignment type mismatch",
            "n = wide",
        ),
        (
            "fn f() -> Int64 { let n = 1; if true { 1 } else { n } }",
            "Return type mismatch",
            "fn f() -> Int64 { let n = 1; if true { 1 } else { n } }",
        ),
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn current() -> Int32 { 42 }").unwrap();
        let version = vm.module_version();
        for (source, message, marked) in cases {
            let source = format!("// 🙂\n{source}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{error}");
            assert_eq!(error.code(), ErrorCode::TypeMismatch, "{error}");
            assert!(error.message().contains(message), "{source}: {error}");
            assert_eq!(
                error.span().unwrap().slice(&source),
                Some(marked),
                "{error}"
            );
            let mut aot = crate::aot::AotCompiler::new("invalid_int64").unwrap();
            assert_eq!(
                aot.compile_module(&crate::parser::parse(&source).unwrap())
                    .unwrap_err()
                    .code(),
                error.code()
            );
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
}

#[test]
fn int64_host_adapters_preserve_bits_and_unit_parameter_positions() {
    extern "C" fn value() -> i64 {
        i64::MIN
    }
    extern "C" fn identity(n: i64) -> i64 {
        n
    }
    extern "C" fn add(a: i64, b: i64) -> i64 {
        a.wrapping_add(b)
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "value",
                FunctionSignature::new(vec![], Type::Int64),
                value as *const u8,
            )
            .unwrap();
            vm.register_fn(
                "identity",
                FunctionSignature::new(vec![Type::Unit, Type::Int64, Type::Unit], Type::Int64),
                identity as *const u8,
            )
            .unwrap();
            vm.register_fn(
                "add",
                FunctionSignature::new(vec![Type::Int64, Type::Int64], Type::Int64),
                add as *const u8,
            )
            .unwrap();
        }
        vm.load_source("extern fn value() -> Int64 extern fn identity(a: Unit, n: Int64, b: Unit) -> Int64 extern fn add(a: Int64, b: Int64) -> Int64 fn f() -> Int64 { add(identity({}, value(), {}), -1) }").unwrap();
        assert_eq!(vm.call_int64_0("f").unwrap(), i64::MAX);
    }
}

thread_local! {
    static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EVENTS: std::cell::RefCell<Vec<i64>> = const { std::cell::RefCell::new(Vec::new()) };
}

unsafe extern "C" fn delay64(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        EVENTS.with(|events| events.borrow_mut().push(frame.slots[0] as i64));
        frame.state = 1;
        return 1;
    }
    if !READY.with(std::cell::Cell::get) {
        return 1;
    }
    frame.result = (frame.slots[0] as i64).wrapping_add(1) as u64;
    0
}

extern "C" fn record64(value: i64) -> i64 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}

#[test]
fn int64_suspensions_keep_full_width_snapshots_across_reload_failure_and_cancel() {
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    let source = r#"
        extern fn delay(n: Int64) @suspends -> Int64
        extern fn record(n: Int64) -> Int64
        struct Wide { value: Int64 }
        enum E { Empty, Value(Wide) }
        fn direct(n: Int64) -> Int64 { n + 1 }
        fn child(n: Int64) @suspends -> (Int64, E) { (delay(n), E.Value(Wide { value: n })) }
        fn pending(n: Int64) @suspends -> Int64 {
            var value = n
            let (saved, (after, e)) = (value, child({ value = value + 2; record(n) }))
            let original = match e { Empty => 0, Value(wide) => wide.value }
            saved + direct(after) + value + original
        }
        fn failure() @suspends -> Int64 {
            let divisor = delay(-1)
            let pair = (3000000000 / divisor, record(99))
            42
        }
        fn sibling(n: Int64) @suspends { record(delay(n)); }
        fn scoped() @suspends -> Int64 { join { | sibling(3000000000) | sibling(-4000000000) }; 42 }
        fn unit(n: Int64) @suspends { delay(n); }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record",
                FunctionSignature::new(vec![Type::Int64], Type::Int64),
                record64 as *const u8,
            )
            .unwrap();
            vm.register_suspending_fn(
                "delay",
                FunctionSignature::new(vec![Type::Int64], Type::Int64),
                delay64,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        let mut task = vm.call_async_int64("pending", &[4000000000]).unwrap();
        let mut failure = vm.call_async_int64("failure", &[]).unwrap();
        let mut scoped = vm.call_async_int64("scoped", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async_int64("pending", &[-4000000000]).unwrap());
        let mut unit = vm.call_async_int64("unit", &[i64::MIN]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
            failure.resume().unwrap();
            scoped.resume().unwrap();
            unit.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| {
            assert_eq!(
                *events.borrow(),
                [
                    4000000000,
                    4000000000,
                    -1,
                    3000000000,
                    -4000000000,
                    -4000000000,
                    -4000000000,
                    i64::MIN
                ]
            )
        });
        scheduler.cancel(cancelled).unwrap();
        vm.load_source("fn pending(n: Int64) @suspends -> Int64 { n - 1 }")
            .unwrap();
        assert_eq!(
            vm.call_async_int64("pending", &[i64::MIN])
                .unwrap()
                .cps_result(),
            Some(i64::MAX as u64)
        );
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(16000000004));
        unit.resume().unwrap();
        assert_eq!(unit.cps_result(), Some(0));
        let ResumeError::Failed(error) = failure.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(
            error.span().unwrap().slice(source),
            Some("3000000000 / divisor")
        );
        assert_eq!(failure.cps_result(), None);
        assert_eq!(failure.resume(), Err(ResumeError::AlreadyFinished));
        scoped.resume().unwrap();
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| {
            assert_eq!(
                *events.borrow(),
                [
                    4000000000,
                    4000000000,
                    -1,
                    3000000000,
                    -4000000000,
                    -4000000000,
                    -4000000000,
                    i64::MIN,
                    3000000001,
                    -3999999999
                ]
            )
        });
    }
}

#[test]
fn int64_context_preserves_shadowing_sequence_order_and_default_boundaries() {
    let source = r#"
        enum E { Empty, Value(Int64) }
        fn f(n: Int64) -> Int64 {
            let a = 3000000000 + { let (x,): (Int64,) = (n,); x }
            let b = 3000000000 + { let n = 2; let wide: Int64 = 7; wide }
            let c = if true { { return a + b } } else { n }
            c
        }
        fn defaulted() -> Int32 { let n = (1 + 2); n }
        fn negative() -> Int64 { --9223372036854775808 }
        fn shadow(n: Int64) -> Int32 { let result = if true { 1 } else { let n = (2; 3); n }; result }
        fn arm(e: E) -> Int64 { match e { Empty => 3000000000, Value(n) => n } }
        fn sequencing(n: Int64) -> Int64 { (let x: Int64 = n; 3000000000) + x }
        fn nested_match(n: Int64) -> Int64 { 3000000000 + match E.Value(n) { Empty => 0, Value(value) => value } }
        fn grouped_shadow(n: Int64) -> Int32 { let result = (let n = 2; 1) + n; result }
        fn small() -> Int32 { -2147483648 }
        fn double_neg() -> Int32 { --2147483648 }
        fn widen_min() -> Int64 { -2147483648 }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int64_1("f", 4000000000).unwrap(), 10000000007);
        assert_eq!(vm.call_int32_0("defaulted").unwrap(), 3);
        assert_eq!(vm.call_int64_0("negative").unwrap(), i64::MIN);
        assert_eq!(
            vm.call_int64_1("sequencing", 4000000000).unwrap(),
            7000000000
        );
        assert_eq!(
            vm.call_int64_1("nested_match", 4000000000).unwrap(),
            7000000000
        );
        assert_eq!(vm.call_int32_0("small").unwrap(), i32::MIN);
        assert_eq!(vm.call_int32_0("double_neg").unwrap(), i32::MIN);
        assert_eq!(vm.call_int64_0("widen_min").unwrap(), -2147483648);
    }
}

#[test]
fn int64_interpreter_yields_and_bounds_recursion_without_truncation() {
    use super::InterpreterLimits;
    let mut vm = VmEngine::new_interpreter();
    vm.load_source("fn looped(n: Int64) @suspends -> Int64 { var i: Int64 = 0; var value = n; while i < 20 { value = value + 1000000000; i = i + 1 }; value } fn recurse(n: Int64) @suspends -> Int64 { recurse(n + 1) }").unwrap();
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 8,
        instruction_budget: Some(10000),
        poll_budget: Some(3),
        ..Default::default()
    })
    .unwrap();
    let mut task = vm.call_async_int64("looped", &[i64::MAX]).unwrap();
    let mut polls = 0;
    while !task.is_finished() {
        task.resume().unwrap();
        polls += 1;
        assert!(polls < 1000);
    }
    assert!(polls > 1);
    assert_eq!(
        task.cps_result().unwrap() as i64,
        i64::MAX.wrapping_add(20000000000)
    );
    let mut task = vm.call_async_int64("recurse", &[3000000000]).unwrap();
    let error = loop {
        if let Err(crate::runtime::suspension::ResumeError::Failed(error)) = task.resume() {
            break error;
        }
    };
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert_eq!(task.cps_result(), None);
}
