//! Module reload, host declarations, and ABI validation tests.

use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::{cps::CpsFrame, suspension::ResumeError};
use crate::{ErrorPhase, FunctionSignature, Type};

#[test]
fn continuations_keep_their_module_alive_after_reload_and_vm_drop() {
    let mut vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn f(x: Int32) @suspends -> Int32 { delayed_value(x) + 10 }
        fn removed() -> Int32 { 99 }
    "#);
    let mut old = vm.call_async("f", &[1]).unwrap();
    vm.load_source(
        r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn f(x: Int32) @suspends -> Int32 { delayed_value(x) + 100 }
    "#,
    )
    .unwrap();
    assert!(vm.call_int32_0("removed").is_err());
    let mut new = vm.call_async("f", &[1]).unwrap();
    assert_ne!(old.module_version(), new.module_version());
    drop(vm);
    READY.with(|r| r.set(true));
    old.resume().unwrap();
    new.resume().unwrap();
    assert_eq!(old.cps_result(), Some(12));
    assert_eq!(new.cps_result(), Some(102));
}

#[test]
fn invalid_syntax_preserves_the_loaded_module_and_pending_tasks() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(
            "extern fn delayed_value(x: Int32,) @suspends -> Int32 fn value() -> Int32 { 42 } fn pending() @suspends -> Int32 { delayed_value(7,) }",
            mode,
        );
        let version = vm.module_version();
        let mut task = vm.call_async("pending", &[]).unwrap();
        for (invalid, marked) in [
            ("fn f(x: Int32 y: Int32) {}", "y"),
            ("fn f() { g(1 2) }", "2"),
            ("fn f() {} import joky/vm", "import"),
            (r#"fn f() { "hé\q" }"#, "\\q"),
            ("fn f() { \"hé\nnext\" }", "\n"),
        ] {
            let source = format!("// é🙂\n{invalid}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.phase(), ErrorPhase::Parse);
            assert_eq!(error.line(), Some(2));
            assert_eq!(error.span().unwrap().slice(&source), Some(marked));
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("value").unwrap(), 42);
            task.resume().unwrap();
            assert!(!task.is_finished());
        }
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(8));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7]));
    }
}

#[test]
fn failed_load_preserves_previous_code_and_signatures() {
    let mut vm = vm("fn f(x: Int32) -> Int32 { x + 1 }");
    let version = vm.module_version();
    for invalid in [
        "fn f() -> Bool { 1 }",
        "fn f() { # }",
        "fn f() { 2147483648 }",
        "fn f() { let x: Bool = 1 }",
        "fn f() { let text: Int32 = \"wrong type\" }",
        "extern fn missing()",
    ] {
        assert!(vm.load_source(invalid).is_err());
        assert_eq!(vm.module_version(), version);
        assert_eq!(vm.call_int32_1("f", 41).unwrap(), 42);
    }
    let mut snapshot = vm.function_signature("f").unwrap();
    snapshot.params.clear();
    snapshot.ret = Type::Unit;
    assert_eq!(
        vm.function_signature("f").unwrap(),
        FunctionSignature::new(vec![Type::Int32], Type::Int32)
    );
    assert_eq!(vm.call_int32_1("f", 41).unwrap(), 42);
    assert!(vm.call_int32_0("f").is_err());
    assert!(vm.call_unit_0("f").is_err());
    assert!(vm.call_unit_2("f", 1, 2).is_err());
    assert!(vm.call_async("f", &[1]).is_err());
}

#[test]
fn host_declarations_require_the_complete_registered_signature() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn f() -> Int32 { 42 }", mode);
        let version = vm.module_version();
        for declaration in [
            "extern fn record() -> Int32",
            "extern fn record(x: Bool) -> Int32",
            "extern fn record(x: Int32) -> Bool",
            "extern fn record(x: Int32)",
            "extern fn delayed_value(x: Bool) @suspends -> Int32",
            "extern fn delayed_value(x: Int32) @suspends -> Bool",
        ] {
            let error = vm.load_source(declaration).unwrap_err();
            assert!(error.message().contains("incompatible signature"));
            assert_eq!(error.span().unwrap().slice(declaration), Some(declaration));
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        }
    }
}

#[test]
fn direct_and_cps_share_unit_bindings_shadowing_and_evaluation_order() {
    for attribute in ["", "@suspends"] {
        let source = format!(
            r#"
            extern fn record(x: Int32) -> Int32
            fn identity(value: Unit) {attribute} {{ value }}
            fn pick(before: Unit, x: Int32, after: Unit) {attribute} -> Int32 {{
                before
                after
                x
            }}
            fn f() {attribute} -> Int32 {{
                let unit: Unit = {{}}
                identity(unit)
                let x: Int32 = 10
                let nested = {{ let x = 20; x }}
                let x: Int32 = x + 1
                pick({{ record(1); identity(unit) }}, record(x + nested), {{ record(2); unit }})
            }}
        "#
        );
        let vm = vm(&source);
        let result = if attribute.is_empty() {
            vm.call_int32_0("f").unwrap()
        } else {
            vm.call_async("f", &[]).unwrap().cps_result().unwrap() as i32
        };
        assert_eq!(result, 31);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [1, 31, 2]));
    }
}

#[test]
fn replacing_host_registration_keeps_existing_modules_bound_to_old_code() {
    extern "C" fn replacement(value: i32) -> i32 {
        value + 100
    }
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn f() @suspends -> Int32 { record(delayed_value(1)) }
    "#;
    let mut vm = vm(source);
    let mut old = vm.call_async("f", &[]).unwrap();
    unsafe {
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            replacement as *const u8,
        )
        .unwrap();
    }
    let mut still_old = vm.call_async("f", &[]).unwrap();
    vm.load_source(source).unwrap();
    let mut new = vm.call_async("f", &[]).unwrap();
    drop(vm);
    READY.with(|r| r.set(true));
    for task in [&mut old, &mut still_old, &mut new] {
        task.resume().unwrap();
    }
    assert_eq!(old.cps_result(), Some(2));
    assert_eq!(still_old.cps_result(), Some(2));
    assert_eq!(new.cps_result(), Some(102));
}

#[test]
fn native_abi_mismatch_and_missing_suspends_are_compile_errors() {
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_fn(
            "host",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            record as *const u8,
        )
        .unwrap();
    }
    assert!(vm
        .load_source("extern fn host(x: Int32) @suspends -> Int32")
        .unwrap_err()
        .message()
        .contains("ABI"));
    unsafe {
        vm.register_suspending_fn(
            "host",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            delayed_value,
        )
        .unwrap();
    }
    assert!(vm
        .load_source("extern fn host(x: Int32) -> Int32")
        .unwrap_err()
        .message()
        .contains("ABI"));
    assert!(vm
        .load_source(
            r#"
        extern fn host(x: Int32) @suspends -> Int32
        fn f() -> Int32 { host(1) }
    "#
        )
        .unwrap_err()
        .message()
        .contains("@suspends"));
    assert!(vm
        .load_source("extern fn missing()")
        .unwrap_err()
        .message()
        .contains("not registered"));
}

#[test]
fn invalid_host_status_propagates_without_replaying_the_call() {
    unsafe extern "C" fn bad_host(frame: *mut CpsFrame) -> i32 {
        let frame = &mut *frame;
        if frame.state == 0 {
            frame.state = 1;
            1
        } else {
            7
        }
    }
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_suspending_fn(
            "bad_host",
            FunctionSignature::new(vec![], Type::Unit),
            bad_host,
        )
        .unwrap();
    }
    vm.load_source("extern fn bad_host() @suspends fn f() @suspends { bad_host() }")
        .unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    assert_eq!(task.resume(), Err(ResumeError::InvalidCpsStatus(7)));
    assert_eq!(task.cps_result(), None);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
}
