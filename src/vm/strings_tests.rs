//! String ownership, escapes, and suspension survival tests.

use super::support::*;
use super::VmMode;
use crate::runtime::suspension::ResumeError;
use crate::{FunctionSignature, Type};

#[test]
fn returned_strings_and_opaque_values_survive_suspension_and_reload() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).unwrap().len() as i32
    }
    let source = r#"
        import joky/vm
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn byte_length(text: String) -> Int32
        fn text(flag: Bool) @suspends -> String {
            if flag { return (delayed_value(1); "héllo") }
            return "no"
        }
        fn engine() -> vm.Handle { return vm.create_interpreter() }
        fn compute() @suspends -> Int32 {
            let handle = engine()
            let saved = text(true)
            delayed_value(2)
            vm.close(handle)
            return byte_length(saved)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_fn(
                "byte_length",
                FunctionSignature::new(vec![Type::String], Type::Int32),
                byte_length as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        READY.with(|ready| ready.set(false));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(6));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
    }
}

#[test]
fn mutable_strings_preserve_snapshots_across_loop_suspension() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).map_or(-1, |text| text.len() as i32)
    }
    let source = r#"
        extern fn byte_length(text: String) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var text: String = "first"
            let initial = text
            var i = 0
            var total = 0
            while i < 2 {
                let before = text
                text = "second"
                delayed_value(i)
                total = total + byte_length(before)
                i = i + 1
            }
            total + byte_length(initial) + byte_length(text)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_fn(
                "byte_length",
                FunctionSignature::new(vec![Type::String], Type::Int32),
                byte_length as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        let mut task = vm.call_async("compute", &[]).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        task.resume().unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(22));
    }
}

#[test]
fn escaped_strings_preserve_bytes_across_calls_suspension_and_reload() {
    unsafe extern "C" fn inspect_text(text: *const u8) -> i32 {
        match crate::runtime::string::borrow(text) {
            Ok("hé🙂{value}\n\r\t\0\"\\end") => 1,
            Ok("\\n\\r\\t\\0\\\"\\\\") => 2,
            Ok("") => 3,
            _ => -1,
        }
    }
    let mut source = include_str!("../../tests/fixtures/strings.jk").to_owned();
    source.push_str(
        r#"
        extern fn inspect_text(text: String,) -> Int32
        extern fn delayed_value(value: Int32,) @suspends -> Int32
        extern fn record(value: Int32,) -> Int32
        fn direct() -> Int32 {
            inspect_text(identity_text(escaped_text(),),) * 100
                + inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
        fn suspended() @suspends -> Int32 {
            let saved = escaped_text()
            delayed_value(record(7),)
            inspect_text(identity_text(saved,),) * 100
                + inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
    "#,
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_fn(
                "inspect_text",
                FunctionSignature::new(vec![Type::String], Type::Int32),
                inspect_text as *const u8,
            )
            .unwrap();
        }
        vm.load_source(&source).unwrap();
        assert_eq!(vm.call_int32_0("direct").unwrap(), 123);
        let mut task = vm.call_async("suspended", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
            assert!(!task.is_finished());
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7]));
        vm.load_source("fn replacement() {}").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(123));
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7]));
    }
}

#[test]
fn immutable_strings_survive_calls_suspension_reload_and_vm_drop() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).map_or(-1, |s| s.len() as i32)
    }
    let mut vm = vm("fn placeholder() {}");
    unsafe {
        vm.register_fn(
            "byte_length",
            FunctionSignature::new(vec![Type::String], Type::Int32),
            byte_length as *const u8,
        )
        .unwrap();
    }
    vm.load_source(
        r#"
        extern fn byte_length(text: String) -> Int32
        extern fn delayed_value(value: Int32) @suspends -> Int32
        fn identity(text: String) -> String { text }
        fn text() -> String { "héllo {world}\n" }
        fn direct() -> Int32 { byte_length(identity(text())) + byte_length("") }
        fn suspended() @suspends -> Int32 {
            let saved: String = text()
            delayed_value(0)
            byte_length(identity(saved))
        }
    "#,
    )
    .unwrap();
    let expected = "héllo {world}\n".len() as i32;
    assert_eq!(vm.call_int32_0("direct").unwrap(), expected);
    let mut task = vm.call_async("suspended", &[]).unwrap();
    vm.load_source("fn replacement() {}").unwrap();
    drop(vm);
    READY.with(|r| r.set(true));
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(expected as u64));
}
