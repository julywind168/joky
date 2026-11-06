//! Engine API contracts: errors, signatures, and host registration.

use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Type};

#[test]
fn unknown_opaque_types_are_rejected_with_structured_errors() {
    let mut vm = VmEngine::new();
    let error = vm
        .load_source("fn f(value: Host.Context) -> Int32 { 1 }")
        .unwrap_err();
    assert_eq!(error.phase(), ErrorPhase::TypeCheck);
    assert_eq!(error.code(), ErrorCode::UnknownType);
    assert!(error.span().is_some());
    assert!(error.message().contains("Unknown type `Host.Context`"));
}

#[test]
fn unloaded_vm_calls_report_the_same_error_in_both_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = VmEngine::with_mode(mode);
        let errors = [
            vm.call_int32_0("missing").unwrap_err(),
            vm.call_int32_1("missing", 1).unwrap_err(),
            vm.call_int32_2("missing", 1, 2).unwrap_err(),
            vm.call_unit_0("missing").unwrap_err(),
            vm.call_unit_1("missing", 1).unwrap_err(),
            vm.call_unit_2("missing", 1, 2).unwrap_err(),
            vm.call_async("missing", &[]).unwrap_err(),
        ];
        for error in errors {
            assert_eq!(error.phase(), ErrorPhase::Runtime);
            assert_eq!(error.code(), ErrorCode::InvalidState);
            assert_eq!(error.message(), "No module loaded");
        }
    }
}

#[test]
fn checked_calls_preserve_signatures_and_reject_wrong_types_in_both_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn value(x: Int32) -> Int32 { x } fn flag(x: Bool) -> Int32 { 42 } fn unit(x: Unit) -> Int32 { 42 } fn unit_one(x: Int32) { } fn text() -> String { \"value\" }").unwrap();
        let mut signature = vm.function_signature("value").unwrap();
        signature.params.clear();
        signature.ret = Type::Unit;
        assert_eq!(vm.call_int32_1("value", 42).unwrap(), 42);
        assert_eq!(
            vm.function_signature("value").unwrap(),
            FunctionSignature::new(vec![Type::Int32], Type::Int32)
        );
        vm.call_unit_1("unit_one", 1).unwrap();
        for error in [
            vm.call_int32_0("text").unwrap_err(),
            vm.call_unit_0("text").unwrap_err(),
        ] {
            assert_eq!(error.phase(), ErrorPhase::Abi);
            assert_eq!(error.code(), ErrorCode::AbiError);
        }
        for error in [
            vm.call_int32_0("value").unwrap_err(),
            vm.call_int32_2("value", 1, 2).unwrap_err(),
            vm.call_int32_1("flag", 1).unwrap_err(),
            vm.call_int32_0("unit").unwrap_err(),
            vm.call_int32_1("unit", 1).unwrap_err(),
        ] {
            assert_eq!(error.phase(), ErrorPhase::Runtime);
            assert_eq!(error.code(), ErrorCode::InvalidCall);
        }
    }
}

#[test]
fn registration_rejects_invalid_entries_and_builtin_collisions() {
    extern "C" fn host() -> i32 {
        1
    }
    let mut vm = VmEngine::new();
    unsafe {
        assert_eq!(
            vm.register_fn(
                "__vm_error_code",
                FunctionSignature::new(vec![], Type::Int32),
                host as *const u8
            )
            .unwrap_err()
            .code(),
            ErrorCode::DuplicateDefinition
        );
        assert_eq!(
            vm.register_fn(
                "bad\0name",
                FunctionSignature::new(vec![], Type::Int32),
                host as *const u8
            )
            .unwrap_err()
            .code(),
            ErrorCode::AbiError
        );
        assert_eq!(
            vm.register_fn(
                "null",
                FunctionSignature::new(vec![], Type::Int32),
                std::ptr::null()
            )
            .unwrap_err()
            .code(),
            ErrorCode::AbiError
        );
        assert_eq!(
            vm.register_fn(
                "invalid_type",
                FunctionSignature::new(vec![Type::Named("not valid".into())], Type::Int32),
                host as *const u8,
            )
            .unwrap_err()
            .code(),
            ErrorCode::AbiError
        );
    }
}

#[test]
fn synchronous_call_errors_have_the_same_precedence_in_both_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn pending(x: Int32) @suspends -> Int32 { x }")
            .unwrap();
        let error = vm.call_int32_0("pending").unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidCall);
        assert!(error.message().contains("call_async"));
    }
}

#[test]
fn scalar_call_adapters_preserve_width_arity_and_failure_checks() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"
            fn signed(x: Int32) -> Int32 { x }
            fn wide(x: Int64) -> Int64 { x }
            fn floating(x: Float64) -> Float64 { x }
            fn narrow(x: Int8) -> Int8 { x }
            fn unsigned(x: UInt64) -> UInt64 { x }
            fn mixed(x: Float64, y: Int32) -> Float64 { x - float64(y) }
            fn reversed(x: Int32, y: Float64) -> Float64 { float64(x) - y }
            fn triple(x: Float64, y: Float64, z: Float64) -> Float64 { x - y - z }
            fn failure(x: Int64) -> Int64 { x / 0 }
            fn pending(x: UInt64) @suspends -> UInt64 { x }
            fn negative_zero(x: Float64) @suspends -> Float64 { x }
        "#,
        )
        .unwrap();
        assert_eq!(vm.call_int32_1("signed", i32::MIN).unwrap(), i32::MIN);
        assert_eq!(vm.call_int64_1("wide", i64::MIN).unwrap(), i64::MIN);
        assert_eq!(
            vm.call_float64_1("floating", -0.0).unwrap().to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(vm.call_i8_1("narrow", i8::MIN).unwrap(), i8::MIN);
        assert_eq!(vm.call_uint64_1("unsigned", u64::MAX).unwrap(), u64::MAX);
        assert_eq!(vm.call_float64_f64_i32("mixed", 4.5, 2).unwrap(), 2.5);
        assert_eq!(vm.call_float64_i32_f64("reversed", 4, 2.5).unwrap(), 1.5);
        assert_eq!(vm.call_float64_3("triple", 9.5, 2.0, 3.0).unwrap(), 4.5);
        assert_eq!(
            vm.call_int64_1("failure", 7).unwrap_err().code(),
            ErrorCode::RuntimeError,
        );
        assert_eq!(vm.call_int64_1("wide", i64::MAX).unwrap(), i64::MAX);
        assert_eq!(
            vm.call_async_uint64("pending", &[u64::MAX])
                .unwrap()
                .cps_result(),
            Some(u64::MAX),
        );
        assert_eq!(
            vm.call_async_float64("negative_zero", &[-0.0])
                .unwrap()
                .cps_result(),
            Some((-0.0_f64).to_bits()),
        );
        for error in [
            vm.call_int32_0("signed").unwrap_err(),
            vm.call_int64_1("signed", 1).unwrap_err(),
            vm.call_float32_1("floating", 1.0).unwrap_err(),
            vm.call_float64_1("signed", 1.0).unwrap_err(),
            vm.call_i8_1("signed", 1).unwrap_err(),
            vm.call_i16_1("signed", 1).unwrap_err(),
            vm.call_uint8_1("signed", 1).unwrap_err(),
            vm.call_u16_1("signed", 1).unwrap_err(),
            vm.call_uint32_1("signed", 1).unwrap_err(),
            vm.call_uint64_1("wide", 1).unwrap_err(),
        ] {
            assert_eq!(error.phase(), ErrorPhase::Runtime, "{mode:?}: {error}");
            assert_eq!(error.code(), ErrorCode::InvalidCall, "{mode:?}: {error}");
        }
    }
}
