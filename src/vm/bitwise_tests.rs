use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase};

#[test]
fn bitwise_widths_signed_shifts_and_invalid_counts_match_backends() {
    macro_rules! width {
        ($ty:ident, $rust:ty, $sync:ident, $async:ident) => {{
            for mode in [VmMode::Jit, VmMode::Interpreter] {
                for suspends in [false, true] {
                    let ty = stringify!($ty);
                    let attr = if suspends { "@suspends" } else { "" };
                    let source = format!(
                        r#"
                        fn and(a: {ty}, b: {ty}) {attr} -> {ty} {{ a & b }}
                        fn or(a: {ty}, b: {ty}) {attr} -> {ty} {{ a | b }}
                        fn xor(a: {ty}, b: {ty}) {attr} -> {ty} {{ a ^ b }}
                        fn not(a: {ty}, b: {ty}) {attr} -> {ty} {{ ~a }}
                        fn left(a: {ty}, b: {ty}) {attr} -> {ty} {{ a << b }}
                        fn right(a: {ty}, b: {ty}) {attr} -> {ty} {{ a >> b }}
                        fn immediate(a: {ty}, b: {ty}) {attr} -> {ty} {{ a << 1 }}
                        fn fail(a: {ty}, b: {ty}) {attr} -> {ty} {{ a >> {width} }}
                    "#,
                        width = <$rust>::BITS
                    );
                    let mut vm = VmEngine::with_mode(mode);
                    vm.load_source(&source).unwrap();
                    let call = |name, a, b| {
                        if suspends {
                            vm.$async(name, &[a, b])
                                .map(|task| task.cps_result().unwrap() as $rust)
                        } else {
                            vm.$sync(name, a, b)
                        }
                    };
                    for a in [
                        0,
                        1,
                        7,
                        <$rust>::MIN,
                        <$rust>::MAX,
                        (1u64 << (<$rust>::BITS - 1)) as $rust,
                        0xaaaa_aaaa_aaaa_aaaa_u64 as $rust,
                    ] {
                        for b in [
                            0,
                            1,
                            3,
                            <$rust>::MIN,
                            <$rust>::MAX,
                            0x5555_5555_5555_5555_u64 as $rust,
                        ] {
                            for (name, expected) in
                                [("and", a & b), ("or", a | b), ("xor", a ^ b), ("not", !a)]
                            {
                                assert_eq!(
                                    call(name, a, b).unwrap(),
                                    expected,
                                    "{ty} {mode:?} suspends={suspends} {name}({a}, {b})"
                                );
                            }
                        }
                        for n in [0, 1, <$rust>::BITS / 2, <$rust>::BITS - 1] {
                            assert_eq!(call("left", a, n as $rust).unwrap(), a.wrapping_shl(n));
                            assert_eq!(call("right", a, n as $rust).unwrap(), a.wrapping_shr(n));
                        }
                        assert_eq!(call("immediate", a, 0).unwrap(), a.wrapping_shl(1));
                    }
                    for count in [
                        <$rust>::BITS as u64,
                        <$rust>::BITS as u64 + 1,
                        u64::MAX,
                        1u64 << 32,
                        1u64 << 63,
                    ] {
                        let count = count as $rust;
                        if (count as u64) < u64::from(<$rust>::BITS) {
                            continue;
                        }
                        for name in ["left", "right"] {
                            let error = call(name, 1, count).unwrap_err();
                            assert_eq!(error.code(), ErrorCode::RuntimeError);
                            assert!(error.message().contains("Shift count"));
                            assert_eq!(
                                error.span().unwrap().slice(&source),
                                Some(if name == "left" { "a << b" } else { "a >> b" })
                            );
                        }
                    }
                    let error = call("fail", 1, 0).unwrap_err();
                    assert_eq!(error.code(), ErrorCode::RuntimeError);
                    assert_eq!(
                        error.span().unwrap().slice(&source),
                        Some(format!("a >> {}", <$rust>::BITS).as_str())
                    );
                }
            }
        }};
    }
    width!(Int32, i32, call_int32_2, call_async);
    width!(Int64, i64, call_int64_2, call_async_int64);
    width!(UInt32, u32, call_uint32_2, call_async_uint32);
    width!(UInt64, u64, call_uint64_2, call_async_uint64);
}

#[test]
fn bitwise_literals_precedence_aggregates_and_branch_pipes_match_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/bitwise.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap();
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
fn bitwise_type_and_literal_errors_preserve_previous_modules() {
    for (source, marked) in [
        ("fn f() -> Bool { true & false }", "true & false"),
        ("fn f() -> Bool { true | false }", "true | false"),
        ("fn f() -> Bool { true ^ false }", "true ^ false"),
        ("fn f() -> Bool { ~true }", "~true"),
        ("fn f() { ~{} }", "~{}"),
        ("fn f(a: UInt32, b: Int32) -> UInt32 { a << b }", "a << b"),
        ("fn f(a: UInt32, b: UInt64) -> UInt32 { a | b }", "a | b"),
        ("fn f(a: UInt64, b: Int64) -> Bool { a & b == 0 }", "a & b"),
        ("fn f() -> Int32 { 0xffff_ffff }", "0xffff_ffff"),
        (
            "fn f() -> Int64 { 0xffff_ffff_ffff_ffff }",
            "0xffff_ffff_ffff_ffff",
        ),
        ("fn f() -> UInt32 { -0xff }", "-0xff"),
        ("fn f() -> Int32 { -(0x8000_0000) }", "(0x8000_0000)"),
        ("fn f() -> UInt32 { 0x1_0000_0000 }", "0x1_0000_0000"),
        ("fn f() { let n = 0x8000_0000 }", "0x8000_0000"),
    ] {
        let source = format!("// 🙂\n{source}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn current() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), ErrorCode::TypeMismatch, "{source}: {error}");
            assert_eq!(error.phase(), ErrorPhase::TypeCheck);
            assert_eq!(error.span().unwrap().slice(&source), Some(marked));
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
        let mut aot = crate::aot::AotCompiler::new("invalid_bits").unwrap();
        let error = aot
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert_eq!(error.span().unwrap().slice(&source), Some(marked));
    }
}

thread_local! {
    static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EVENTS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(value: u64) -> u64 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
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
fn bitwise_pending_operands_keep_snapshots_after_reload_drop_and_cancel() {
    use crate::{
        runtime::{scheduler::Scheduler, suspension::ResumeError},
        FunctionSignature, Type,
    };
    let source = r#"
        extern fn record(n: UInt64) -> UInt64
        extern fn delay(n: UInt64) @suspends -> UInt64
        fn pending(n: UInt64) @suspends -> UInt64 {
            var value = n
            let saved = value & delay({ value = 0 record(n) })
            let pair = (~saved, value | delay(3))
            let (inverted, low) = pair
            inverted ^ (low << delay(1))
        }
        fn failure(n: UInt64) @suspends -> UInt64 {
            let count = delay(n)
            let pair = (1 << count, record(99))
            42
        }
        fn direct(n: UInt64) -> UInt64 { 1 >> n }
        fn through_call(n: UInt64) @suspends -> UInt64 { direct(delay(n)) | record(99) }
        fn sibling(n: UInt64) @suspends { record(delay(n)); }
        fn scoped() @suspends -> UInt64 { scope { | sibling(1 | 2) | sibling(4 | 8) } 42 }
        fn failed_branch(n: UInt64) @suspends { direct(delay(n)); }
        fn failed_scope() @suspends -> UInt64 { scope { | failed_branch(64 | 0) | sibling(7 | 8) } record(99) }
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
        let high = 0x8000_0000_0000_005a;
        let mut task = vm.call_async_uint64("pending", &[high]).unwrap();
        let mut failure = vm.call_async_uint64("failure", &[64]).unwrap();
        let mut through_call = vm.call_async_uint64("through_call", &[1 << 32]).unwrap();
        let mut scoped = vm.call_async_uint64("scoped", &[]).unwrap();
        let mut failed_scope = vm.call_async_uint64("failed_scope", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async_uint64("pending", &[7]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failure.resume().unwrap();
            through_call.resume().unwrap();
            scoped.resume().unwrap();
            failed_scope.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        let initial = [high, high, 64, 1 << 32, 3, 12, 64, 15, 7, 7];
        EVENTS.with(|events| assert_eq!(*events.borrow(), initial));
        scheduler.cancel(cancelled).unwrap();
        vm.load_source("fn pending(n: UInt64) @suspends -> UInt64 { ~n }")
            .unwrap();
        assert_eq!(
            vm.call_async_uint64("pending", &[high])
                .unwrap()
                .cps_result(),
            Some(!high)
        );
        drop(vm);
        READY.with(|ready| ready.set(true));
        for _ in 0..3 {
            if !task.is_finished() {
                task.resume().unwrap();
            }
        }
        assert_eq!(task.cps_result(), Some(!high ^ 6));
        for (task, marked) in [
            (&mut failure, "1 << count"),
            (&mut through_call, "1 >> n"),
            (&mut failed_scope, "1 >> n"),
        ] {
            let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                panic!()
            };
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert!(error.message().contains("Shift count"));
            assert_eq!(error.span().unwrap().slice(source), Some(marked));
            assert_eq!(task.cps_result(), None);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        }
        scoped.resume().unwrap();
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        let mut expected = initial.to_vec();
        expected.extend([3, 1, 3, 12]);
        EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
    }
}

#[test]
fn bitwise_failures_stop_subsequent_operands_and_short_circuit_paths_stay_lazy() {
    use crate::{FunctionSignature, Type};
    let source = r#"
        extern fn record(n: UInt64) -> UInt64
        fn bad(n: UInt64) -> UInt64 { 1 << n }
        fn through(n: UInt64) -> UInt64 { bad(n) | record(99) }
        fn before(n: UInt64) -> UInt64 { record(7) << record(n) }
        fn take(a: UInt64, b: UInt64) -> UInt64 { a | b }
        fn args(n: UInt64) -> UInt64 { take(bad(n), record(99)) }
        fn discarded(n: UInt64) -> UInt64 { bad(n); record(99) }
        fn lazy(n: UInt64) -> UInt64 { if false && bad(n) == 0 || true || bad(n) == 1 { 42 } else { 0 } }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record",
                FunctionSignature::new(vec![Type::UInt64], Type::UInt64),
                record as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        for name in ["through", "args", "discarded"] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let error = vm.call_uint64_1(name, 64).unwrap_err();
            assert_eq!(error.span().unwrap().slice(source), Some("1 << n"));
            EVENTS.with(|events| assert!(events.borrow().is_empty()));
        }
        assert_eq!(vm.call_uint64_1("lazy", 64).unwrap(), 42);
        let error = vm.call_uint64_1("before", 64).unwrap_err();
        assert_eq!(
            error.span().unwrap().slice(source),
            Some("record(7) << record(n)")
        );
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 64]));
    }
}
