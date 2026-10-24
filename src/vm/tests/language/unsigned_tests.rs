use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Type};

#[test]
fn unsigned_contexts_arithmetic_comparisons_and_aggregates_match_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../../../tests/fixtures/unsigned.jk"),
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
            let call32 = |name, a, b| {
                if suspends {
                    vm.call_async_uint32(name, &[a, b])
                        .unwrap()
                        .cps_result()
                        .unwrap() as u32
                } else {
                    vm.call_uint32_2(name, a, b).unwrap()
                }
            };
            let call64 = |name, a, b| {
                if suspends {
                    vm.call_async_uint64(name, &[a, b])
                        .unwrap()
                        .cps_result()
                        .unwrap()
                } else {
                    vm.call_uint64_2(name, a, b).unwrap()
                }
            };
            for a in [
                0u64,
                1,
                7,
                i32::MAX as u64,
                1 << 31,
                u32::MAX as u64,
                1 << 32,
                i64::MAX as u64,
                1 << 63,
                u64::MAX,
            ] {
                for b in [0u64, 1, 3, 1 << 31, u32::MAX as u64, 1 << 63, u64::MAX] {
                    for (name, expected) in [
                        ("add64", a.wrapping_add(b)),
                        ("sub64", a.wrapping_sub(b)),
                        ("mul64", a.wrapping_mul(b)),
                        ("ordered64", score(a, b)),
                    ] {
                        assert_eq!(
                            call64(name, a, b),
                            expected,
                            "{mode:?} suspends={suspends} {name}({a},{b})"
                        );
                    }
                    if b != 0 {
                        assert_eq!(call64("div64", a, b), a / b);
                    }
                    let (a, b) = (a as u32, b as u32);
                    for (name, expected) in [
                        ("add32", a.wrapping_add(b)),
                        ("sub32", a.wrapping_sub(b)),
                        ("mul32", a.wrapping_mul(b)),
                        ("ordered32", score(u64::from(a), u64::from(b)) as u32),
                    ] {
                        assert_eq!(
                            call32(name, a, b),
                            expected,
                            "{mode:?} suspends={suspends} {name}({a},{b})"
                        );
                    }
                    if b != 0 {
                        assert_eq!(call32("div32", a, b), a / b);
                    }
                }
            }
            if suspends {
                assert_eq!(
                    vm.call_async_uint32("max32", &[]).unwrap().cps_result(),
                    Some(u64::from(u32::MAX))
                );
                assert_eq!(
                    vm.call_async_uint64("max64", &[]).unwrap().cps_result(),
                    Some(u64::MAX)
                );
                assert_eq!(
                    vm.call_async_uint32("id32", &[u32::MAX])
                        .unwrap()
                        .cps_result(),
                    Some(u64::from(u32::MAX))
                );
                assert_eq!(
                    vm.call_async_uint64("id64", &[u64::MAX])
                        .unwrap()
                        .cps_result(),
                    Some(u64::MAX)
                );
            } else {
                assert_eq!(vm.call_uint32_0("max32").unwrap(), u32::MAX);
                assert_eq!(vm.call_uint64_0("max64").unwrap(), u64::MAX);
                assert_eq!(vm.call_uint32_1("id32", u32::MAX).unwrap(), u32::MAX);
                assert_eq!(vm.call_uint64_1("id64", u64::MAX).unwrap(), u64::MAX);
            }
            assert!(vm.call_int32_0("max32").is_err());
            assert!(vm.call_int64_0("max64").is_err());
            assert!(vm.call_uint32_0("max64").is_err());
            assert!(vm.call_uint64_0("max32").is_err());
            assert!(vm.call_async("max32", &[]).is_err());
            assert!(vm.call_async_int64("max64", &[]).is_err());
            assert!(vm.call_async_uint32("max64", &[]).is_err());
            assert!(vm.call_async_uint64("max32", &[]).is_err());
            for (name, result) in [
                (
                    "div32",
                    if suspends {
                        vm.call_async_uint32("div32", &[u32::MAX, 0]).map(|_| ())
                    } else {
                        vm.call_uint32_2("div32", u32::MAX, 0).map(|_| ())
                    },
                ),
                (
                    "div64",
                    if suspends {
                        vm.call_async_uint64("div64", &[u64::MAX, 0]).map(|_| ())
                    } else {
                        vm.call_uint64_2("div64", u64::MAX, 0).map(|_| ())
                    },
                ),
            ] {
                let error = result.unwrap_err();
                assert_eq!(error.code(), ErrorCode::RuntimeError, "{name}");
                assert_eq!(error.span().unwrap().slice(&source), Some("a / b"));
            }
        }
    }
}

fn score(a: u64, b: u64) -> u64 {
    u64::from(a == b)
        + 2 * u64::from(a != b)
        + 4 * u64::from(a < b)
        + 8 * u64::from(a <= b)
        + 16 * u64::from(a > b)
        + 32 * u64::from(a >= b)
}

#[test]
fn unsigned_literals_reject_overflow_negation_and_implicit_conversions() {
    let cases = [
        (
            "fn f() -> UInt32 { 4294967296 }",
            "4294967296",
            "outside UInt32 range",
        ),
        ("fn f() -> UInt32 { -1 }", "-1", "not supported for UInt32"),
        ("fn f() -> UInt64 { -0 }", "-0", "not supported for UInt64"),
        (
            "fn f() -> UInt64 { -18446744073709551615 }",
            "-18446744073709551615",
            "not supported for UInt64",
        ),
        ("fn f() -> UInt64 { -(1) }", "-(1)", "Unary `-` operand"),
        (
            "fn f(n: UInt32) -> UInt32 { -n }",
            "-n",
            "Unary `-` operand",
        ),
        (
            "fn f() -> Int64 { 18446744073709551615 }",
            "18446744073709551615",
            "outside Int64 range",
        ),
        (
            "fn f() -> Int64 { -9223372036854775809 }",
            "-9223372036854775809",
            "outside Int64 range",
        ),
        (
            "fn f() { let n = 4294967295 }",
            "4294967295",
            "outside Int32 range",
        ),
        (
            "fn f() -> UInt64 { 18446744073709551615; 1 }",
            "18446744073709551615",
            "outside Int32 range",
        ),
        (
            "fn f() { let n = 1; let u: UInt32 = n }",
            "let u: UInt32 = n",
            "annotation mismatch",
        ),
        (
            "fn f(n: UInt32) { var u: UInt64 = 0; u = n }",
            "u = n",
            "Assignment type mismatch",
        ),
        (
            "fn f(n: UInt64) -> Int64 { n }",
            "fn f(n: UInt64) -> Int64 { n }",
            "Return type mismatch",
        ),
        (
            "fn f(a: Int32, b: UInt32) -> Bool { a < b }",
            "a < b",
            "same type",
        ),
        (
            "fn f(a: Int64, b: UInt64) -> Bool { a == b }",
            "a == b",
            "same type",
        ),
        (
            "fn f(a: UInt32, b: UInt64) -> UInt64 { a + b }",
            "a + b",
            "same type",
        ),
        (
            "fn f(a: UInt32, b: Bool) -> Bool { a == b }",
            "a == b",
            "same type",
        ),
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn current() -> Int32 { 42 }").unwrap();
        let version = vm.module_version();
        for (source, marked, message) in cases {
            let source = format!("// 🙂\n{source}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{source}: {error}");
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
            assert!(error.message().contains(message), "{source}: {error}");
            assert_eq!(error.span().unwrap().slice(&source), Some(marked));
            let mut aot = crate::aot::AotCompiler::new("invalid_unsigned").unwrap();
            let error = aot
                .compile_module(&crate::parser::parse(&source).unwrap())
                .unwrap_err();
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
            assert_eq!(error.span().unwrap().slice(&source), Some(marked));
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
    for spelling in ["18446744073709551616", "9999999999999999999999999999"] {
        let source = format!("// 🙂\nfn f() -> UInt64 {{ {spelling} }}");
        let error = crate::parser::parse(&source).unwrap_err();
        assert_eq!(error.span.slice(&source), Some(spelling));
        assert!(error.message.contains("outside UInt64 magnitude range"));
    }
    for source in [
        "type UInt32 = Int32",
        "fn UInt64() {}",
        "fn f(UInt32: Int32) {}",
    ] {
        assert!(crate::parser::parse(source).is_err());
    }
}

thread_local! {
    static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EVENTS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn record(value: u64) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
extern "C" fn record32(value: u32) -> u32 {
    record(u64::from(value));
    value
}
extern "C" fn record64(value: u64) -> u64 {
    record(value);
    value
}

#[test]
fn unsigned_host_adapters_preserve_bits_and_erase_unit_arguments() {
    macro_rules! check_width {
        ($rust:ty, $ty:ident, $call:ident) => {{
            extern "C" fn value() -> $rust {
                <$rust>::MAX
            }
            extern "C" fn identity(n: $rust) -> $rust {
                n
            }
            extern "C" fn add(a: $rust, b: $rust) -> $rust {
                a.wrapping_add(b)
            }
            extern "C" fn sink(n: $rust) {
                record(n as u64);
            }
            extern "C" fn pair(a: $rust, b: $rust) {
                record(a as u64);
                record(b as u64);
            }
            for mode in [VmMode::Jit, VmMode::Interpreter] {
                EVENTS.with(|events| events.borrow_mut().clear());
                let mut vm = VmEngine::with_mode(mode);
                let ty = Type::$ty;
                for (name, params, ret, entry) in [
                    ("value", vec![Type::Unit], ty.clone(), value as *const u8),
                    (
                        "identity",
                        vec![Type::Unit, ty.clone(), Type::Unit],
                        ty.clone(),
                        identity as *const u8,
                    ),
                    (
                        "add",
                        vec![ty.clone(), Type::Unit, ty.clone()],
                        ty.clone(),
                        add as *const u8,
                    ),
                    (
                        "sink",
                        vec![Type::Unit, ty.clone()],
                        Type::Unit,
                        sink as *const u8,
                    ),
                    (
                        "pair",
                        vec![ty.clone(), ty.clone(), Type::Unit],
                        Type::Unit,
                        pair as *const u8,
                    ),
                ] {
                    unsafe {
                        vm.register_fn(name, FunctionSignature::new(params, ret), entry)
                            .unwrap();
                    }
                }
                let source = format!(
                    r#"
                    extern fn value(unit: Unit) -> {ty}
                    extern fn identity(unit: Unit, n: {ty}, end: Unit) -> {ty}
                    extern fn add(a: {ty}, unit: Unit, b: {ty}) -> {ty}
                    extern fn sink(unit: Unit, n: {ty})
                    extern fn pair(a: {ty}, b: {ty}, unit: Unit)
                    fn f() -> {ty} {{
                        let n = identity({{}}, value({{}}), {{}})
                        sink({{}}, n); pair(n, 1, {{}})
                        add(n, {{}}, 1)
                    }}
                "#
                );
                vm.load_source(&source).unwrap();
                assert_eq!(vm.$call("f").unwrap(), 0);
                EVENTS.with(|events| {
                    assert_eq!(
                        *events.borrow(),
                        [<$rust>::MAX as u64, <$rust>::MAX as u64, 1]
                    )
                });
            }
        }};
    }
    check_width!(u8, UInt8, call_uint8_0);
    check_width!(u32, UInt32, call_uint32_0);
    check_width!(u64, UInt64, call_uint64_0);
}

unsafe extern "C" fn delay32(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    delayed(&mut *frame, true)
}
unsafe extern "C" fn delay64(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    delayed(&mut *frame, false)
}
fn delayed(frame: &mut crate::runtime::cps::CpsFrame, narrow: bool) -> i32 {
    assert_eq!(frame.slots[0], 0);
    assert_eq!(frame.slots[2], 0);
    if frame.state == 0 {
        record(frame.slots[1]);
        frame.state = 1;
        return 1;
    }
    if !READY.with(std::cell::Cell::get) {
        return 1;
    }
    frame.result = if narrow {
        u64::from((frame.slots[1] as u32).wrapping_add(1))
    } else {
        frame.slots[1].wrapping_add(1)
    };
    0
}

fn call_async(
    vm: &VmEngine,
    narrow: bool,
    name: &str,
    args: &[u64],
) -> Result<crate::runtime::suspension::Continuation, crate::Error> {
    if narrow {
        vm.call_async_uint32(name, &args.iter().map(|n| *n as u32).collect::<Vec<_>>())
    } else {
        vm.call_async_uint64(name, args)
    }
}

#[test]
fn unsigned_pending_snapshots_survive_reload_drop_failure_and_cancellation() {
    use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for narrow in [false, true] {
            let (ty, max, high) = if narrow {
                (Type::UInt32, u64::from(u32::MAX), (1u64 << 31) + 17)
            } else {
                (Type::UInt64, u64::MAX, (1u64 << 63) + 17)
            };
            let source = format!(
                r#"
                extern fn delay(unit: Unit, n: {ty}, end: Unit) @suspends -> {ty}
                extern fn record(n: {ty}) -> {ty}
                struct Number {{ value: {ty} }}
                enum E {{ Empty, Value(Number) }}
                fn direct(n: {ty}) -> {ty} {{ n + 1 }}
                fn child(n: {ty}) @suspends -> ({ty}, E) {{ (delay({{}}, n, {{}}), E.Value(Number {{ value: n }})) }}
                fn pending(n: {ty}) @suspends -> {ty} {{
                    var value = n
                    let (saved, (after, e)) = (value, child({{ value = value + 2; record(n) }}))
                    let original = match e {{ Empty => 0, Value(number) => number.value }}
                    saved + direct(after) + value + original
                }}
                fn failure() @suspends -> {ty} {{
                    let divisor = delay({{}}, {max}, {{}})
                    let pair = ({max} / divisor, record(99))
                    42
                }}
                fn sibling(n: {ty}) @suspends {{ record(delay({{}}, n, {{}})); }}
                fn scoped() @suspends -> {ty} {{ join {{ | sibling({high}) | sibling({max}) }}; 42 }}
                fn unit(n: {ty}) @suspends {{ delay({{}}, n, {{}}); }}
            "#
            );
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_fn(
                    "record",
                    FunctionSignature::new(vec![ty.clone()], ty.clone()),
                    if narrow {
                        record32 as *const u8
                    } else {
                        record64 as *const u8
                    },
                )
                .unwrap();
                vm.register_suspending_fn(
                    "delay",
                    FunctionSignature::new(vec![Type::Unit, ty.clone(), Type::Unit], ty),
                    if narrow { delay32 } else { delay64 },
                )
                .unwrap();
            }
            vm.load_source(&source).unwrap();
            let mut pending = call_async(&vm, narrow, "pending", &[high]).unwrap();
            let mut failure = call_async(&vm, narrow, "failure", &[]).unwrap();
            let mut scoped = call_async(&vm, narrow, "scoped", &[]).unwrap();
            let mut scheduler = Scheduler::new();
            let cancelled = scheduler.suspend(call_async(&vm, narrow, "pending", &[max]).unwrap());
            let mut unit = call_async(&vm, narrow, "unit", &[max]).unwrap();
            for _ in 0..3 {
                pending.resume().unwrap();
                failure.resume().unwrap();
                scoped.resume().unwrap();
                unit.resume().unwrap();
                scheduler.resume(cancelled).unwrap();
            }
            let initial = [high, high, max, high, max, max, max, max];
            EVENTS.with(|events| assert_eq!(*events.borrow(), initial));
            scheduler.cancel(cancelled).unwrap();
            let ty = if narrow { "UInt32" } else { "UInt64" };
            vm.load_source(&format!(
                "fn pending(n: {ty}) @suspends -> {ty} {{ n + 1 }}"
            ))
            .unwrap();
            assert_eq!(
                call_async(&vm, narrow, "pending", &[max])
                    .unwrap()
                    .cps_result(),
                Some(0)
            );
            drop(vm);
            READY.with(|ready| ready.set(true));
            pending.resume().unwrap();
            assert_eq!(
                pending.cps_result(),
                Some(high.wrapping_mul(4).wrapping_add(4) & max)
            );
            unit.resume().unwrap();
            assert_eq!(unit.cps_result(), Some(0));
            let ResumeError::Failed(error) = failure.resume().unwrap_err() else {
                panic!()
            };
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert_eq!(
                error.span().unwrap().slice(&source),
                Some(format!("{max} / divisor").as_str())
            );
            assert_eq!(failure.cps_result(), None);
            assert_eq!(failure.resume(), Err(ResumeError::AlreadyFinished));
            scoped.resume().unwrap();
            assert_eq!(scoped.cps_result(), Some(42));
            assert_eq!(scheduler.resume_all().unwrap(), 0);
            let mut expected = initial.to_vec();
            expected.extend([high + 1, 0]);
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
        }
    }
}

#[test]
fn unsigned_interpreter_poll_budgets_preserve_width_and_order() {
    for narrow in [false, true] {
        let ty = if narrow { "UInt32" } else { "UInt64" };
        let max = if narrow {
            u64::from(u32::MAX)
        } else {
            u64::MAX
        };
        let mut vm = VmEngine::new_interpreter();
        vm.load_source(&format!("fn looped(n: {ty}) @suspends -> {ty} {{ var value = n; var i: {ty} = 0; while i < 20 {{ value = value + 1; i = i + 1 }}; value }}")).unwrap();
        vm.set_interpreter_limits(super::InterpreterLimits {
            max_frames: 8,
            instruction_budget: Some(10000),
            poll_budget: Some(3),
        })
        .unwrap();
        let mut task = call_async(&vm, narrow, "looped", &[max]).unwrap();
        let mut polls = 0;
        while !task.is_finished() {
            task.resume().unwrap();
            polls += 1;
            assert!(polls < 1000);
        }
        assert!(polls > 1);
        assert_eq!(task.cps_result(), Some(19));
    }
}
