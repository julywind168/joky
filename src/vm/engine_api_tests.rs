//! Engine API contracts: errors, signatures, and host registration.

use super::{VmEngine, VmMode};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Span, Type};

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
fn parse_errors_keep_their_source_line() {
    let mut vm = VmEngine::new();
    let error = vm.load_source("fn f() {\n#\n}").unwrap_err();
    assert_eq!(error.phase(), ErrorPhase::Parse);
    assert_eq!(error.line(), Some(2));
    assert_eq!(error.span(), Some(Span::new(9, 10)));
}

#[test]
fn runtime_lookup_errors_keep_their_runtime_phase() {
    let mut vm = VmEngine::new();
    vm.load_source("fn f() -> Int32 { 1 }").unwrap();
    let error = vm.call_int32_0("missing").unwrap_err();
    assert_eq!(error.phase(), ErrorPhase::Runtime);
    assert_eq!(error.code(), ErrorCode::NotFound);
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
            vm.call_int32_0("value").unwrap_err(),
            vm.call_int32_2("value", 1, 2).unwrap_err(),
            vm.call_int32_1("flag", 1).unwrap_err(),
            vm.call_int32_0("unit").unwrap_err(),
            vm.call_int32_1("unit", 1).unwrap_err(),
            vm.call_int32_0("text").unwrap_err(),
            vm.call_unit_0("text").unwrap_err(),
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
                "vm.error_code",
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
fn registered_host_opaque_types_are_accepted() {
    extern "C" fn host(_value: u64) -> i32 {
        7
    }
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_fn(
            "host",
            FunctionSignature::new(vec![Type::Named("Host.Context".into())], Type::Int32),
            host as *const u8,
        )
        .unwrap();
    }
    vm.load_source(
        "extern fn host(value: Host.Context) -> Int32 fn f(value: Host.Context) -> Int32 { host(value) }",
    )
    .unwrap();
}
