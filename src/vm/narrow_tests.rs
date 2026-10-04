use super::{VmEngine, VmMode};

/// N4 narrow-integer arithmetic, wrapping traps and extension rules agree
/// across the JIT and the interpreter.
#[test]
fn narrow_integers_match_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/narrow_integers.jk"))
            .unwrap();
        assert_eq!(vm.call_int32_0("main").unwrap(), 42, "{mode:?}");
    }
}

/// Every wrapping result matches Rust's own arithmetic bit for bit,
/// including MIN / -1, shifts at both bounds and the unsigned lanes.
#[test]
fn narrow_arithmetic_matches_rust_bits() {
    let source = r#"
        fn add8(a: Int8, b: Int8) -> Int8 { a + b }
        fn mul8(a: Int8, b: Int8) -> Int8 { a * b }
        fn div8(a: Int8, b: Int8) -> Int8 { a / b }
        fn shl8(a: Int8, b: Int8) -> Int8 { a << b }
        fn shr8(a: Int8, b: Int8) -> Int8 { a >> b }
        fn neg8(a: Int8) -> Int8 { -a }
        fn add16(a: Int16, b: Int16) -> Int16 { a + b }
        fn div16(a: Int16, b: Int16) -> Int16 { a / b }
        fn mul16(a: Int16, b: Int16) -> Int16 { a * b }
        fn addu16(a: UInt16, b: UInt16) -> UInt16 { a + b }
        fn mulu16(a: UInt16, b: UInt16) -> UInt16 { a * b }
        fn divu16(a: UInt16, b: UInt16) -> UInt16 { a / b }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        for (a, b) in [
            (i8::MIN, -1),
            (i8::MIN, 1),
            (i8::MAX, 1),
            (-128, 127),
            (50, -100),
        ] {
            assert_eq!(
                vm.call_i8_2("add8", a, b).unwrap(),
                a.wrapping_add(b),
                "{mode:?} add8"
            );
            assert_eq!(
                vm.call_i8_2("mul8", a, b).unwrap(),
                a.wrapping_mul(b),
                "{mode:?} mul8"
            );
            assert_eq!(
                vm.call_i8_2("div8", a, b).unwrap(),
                a.wrapping_div(b),
                "{mode:?} div8"
            );
        }
        for (a, count) in [(i8::MIN, 7i8), (i8::MIN, 1), (-1, 7), (1, 7), (i8::MAX, 1)] {
            assert_eq!(
                vm.call_i8_2("shl8", a, count).unwrap(),
                a.wrapping_shl(count as u32),
                "{mode:?} shl8"
            );
            assert_eq!(
                vm.call_i8_2("shr8", a, count).unwrap(),
                a.wrapping_shr(count as u32),
                "{mode:?} shr8"
            );
        }
        assert_eq!(
            vm.call_i8_1("neg8", i8::MIN).unwrap(),
            i8::MIN,
            "{mode:?} neg8"
        );
        for (a, b) in [
            (i16::MIN, -1),
            (i16::MIN, 1),
            (i16::MAX, i16::MAX),
            (-32768, 300),
        ] {
            assert_eq!(vm.call_i16_2("add16", a, b).unwrap(), a.wrapping_add(b));
            assert_eq!(vm.call_i16_2("div16", a, b).unwrap(), a.wrapping_div(b));
            assert_eq!(vm.call_i16_2("mul16", a, b).unwrap(), a.wrapping_mul(b));
        }
        for (a, b) in [(u16::MAX, 1), (u16::MAX, u16::MAX), (0, 65535)] {
            assert_eq!(vm.call_u16_2("addu16", a, b).unwrap(), a.wrapping_add(b));
            assert_eq!(vm.call_u16_2("mulu16", a, b).unwrap(), a.wrapping_mul(b));
            assert_eq!(vm.call_u16_2("divu16", a, b).unwrap(), a / b);
        }
    }
}

/// Division by zero and out-of-range shifts fail with the shared trap message
/// in both backends instead of wrapping silently.
#[test]
fn narrow_division_and_shift_traps_report_failures() {
    let source = r#"
        fn divide(a: Int8, b: Int8) -> Int8 { a / b }
        fn shift(a: Int16, b: Int16) -> Int16 { a << b }
        fn shift_u(a: UInt16, b: UInt16) -> UInt16 { a >> b }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let error = vm.call_i8_2("divide", 1, 0).unwrap_err();
        assert!(error.to_string().contains("Division by zero"), "{error}");
        let error = vm.call_i16_2("shift", 1, 16).unwrap_err();
        assert!(
            error.to_string().contains("less than the integer width"),
            "{error}"
        );
        let error = vm.call_i16_2("shift", 1, -1).unwrap_err();
        assert!(
            error.to_string().contains("less than the integer width"),
            "{error}"
        );
        let error = vm.call_u16_2("shift_u", 1, 16).unwrap_err();
        assert!(
            error.to_string().contains("less than the integer width"),
            "{error}"
        );
        // In-range shifts keep working after failures.
        assert_eq!(vm.call_i16_2("shift", 1, 15).unwrap(), -32768);
    }
}

/// Narrow suspending exports cross CPS polls as zero-extended bit patterns.
#[test]
fn narrow_integers_survive_suspension() {
    let source = r#"
        import joky/task
        fn echo8(v: Int8) @suspends -> Int8 { task.yield_now() v }
        fn echou16(v: UInt16) @suspends -> UInt16 { task.yield_now() v }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        for v in [i8::MIN, -1, 0, 127] {
            let mut task = vm.call_async_i8("echo8", &[v]).unwrap();
            while task.status() == crate::runtime::suspension::ContinuationStatus::Pending {
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result().unwrap(), u64::from(v as u8), "{mode:?}");
        }
        for v in [0u16, 65535, 4242] {
            let mut task = vm.call_async_u16("echou16", &[v]).unwrap();
            while task.status() == crate::runtime::suspension::ContinuationStatus::Pending {
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result().unwrap(), u64::from(v), "{mode:?}");
        }
    }
}

/// Call entries reject mismatched narrow signatures instead of reinterpreting
/// another width's bits.
#[test]
fn narrow_call_entries_validate_signatures() {
    let source = r#"
        fn only8(a: Int8) -> Int8 { a }
        fn only16(a: Int16) -> Int16 { a }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_i8_1("only8", -5).unwrap(), -5, "{mode:?}");
        assert!(vm.call_i16_1("only8", 5).is_err(), "{mode:?}");
        assert!(vm.call_i8_1("only16", 5).is_err(), "{mode:?}");
        assert!(vm.call_u16_1("only8", 5).is_err(), "{mode:?}");
    }
}

/// `&mut` C references to narrow scalar bindings write back through the
/// checked reference path, like the wider integer lanes.
#[test]
fn narrow_c_references_write_back() {
    use crate::{FunctionSignature, Type};
    unsafe extern "C" fn bump8(p: *mut i8) {
        *p = p.read().wrapping_add(1);
    }
    unsafe extern "C" fn bump32(p: *mut i32) {
        *p = p.read().wrapping_add(1);
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_c_fn(
                "c_bump8",
                FunctionSignature::new(vec![Type::reference(Type::Int8, true)], Type::Unit),
                bump8 as *const u8,
                None,
            )
            .unwrap();
            vm.register_c_fn(
                "c_bump32",
                FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit),
                bump32 as *const u8,
                None,
            )
            .unwrap();
        }
        vm.load_source(
            "unsafe extern \"C\" fn c_bump8(p: &mut Int8)\n\
             unsafe extern \"C\" fn c_bump32(p: &mut Int32)\n\
             fn narrow() -> Int32 { var v: Int8 = -128 unsafe { c_bump8(&mut v) } int32(v) }",
        )
        .unwrap();
        assert_eq!(vm.call_int32_0("narrow").unwrap(), -127, "{mode:?}");
        // Compare with the established Int32 lane on the same path.
        vm.load_source(
            "unsafe extern \"C\" fn c_bump32(p: &mut Int32)\n\
             fn wide() -> Int32 { var w: Int32 = 41 unsafe { c_bump32(&mut w) } w }",
        )
        .unwrap();
        assert_eq!(vm.call_int32_0("wide").unwrap(), 42, "{mode:?}");
    }
}
