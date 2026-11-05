//! Alias resolution and reload semantics across both execution modes.

use super::support::*;
use super::VmMode;
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Type};

#[test]
fn aliases_match_canonical_types_in_direct_and_cps_execution() {
    let source = include_str!("../../../../tests/fixtures/aliases.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            assert_eq!(
                vm.function_signature("alias_add").unwrap(),
                FunctionSignature::new(vec![Type::Int32, Type::Int32], Type::Int32)
            );
            assert_eq!(
                vm.function_signature("alias_flag").unwrap(),
                FunctionSignature::new(vec![Type::Bool], Type::Bool)
            );
            assert_eq!(
                vm.function_signature("alias_text").unwrap(),
                FunctionSignature::new(vec![Type::String], Type::String)
            );
            assert_eq!(
                vm.function_signature("alias_unit").unwrap(),
                FunctionSignature::new(vec![Type::Unit], Type::Unit)
            );
            if suspends {
                assert_eq!(
                    vm.call_async("alias_compute", &[]).unwrap().cps_result(),
                    Some(7)
                );
                assert_eq!(
                    vm.call_async("alias_choose", &[0]).unwrap().cps_result(),
                    Some(42)
                );
                assert_eq!(
                    vm.call_async("alias_add", &[20, 22]).unwrap().cps_result(),
                    Some(42)
                );
            } else {
                assert_eq!(vm.call_int32_0("alias_compute").unwrap(), 7);
                assert_eq!(vm.call_int32_1("alias_choose", 0).unwrap(), 42);
                assert_eq!(vm.call_int32_2("alias_add", 20, 22).unwrap(), 42);
            }
        }
        // Type and value namespaces are separate; aliases are not constructors.
        let vm = parity_vm(
            "type Count = Int32 fn Count() -> Count { let Count: Count = 42; Count }",
            mode,
        );
        assert_eq!(vm.call_int32_0("Count").unwrap(), 42);
        let vm = parity_vm("import joky/vm type Handle = vm.Vm fn compute() -> Int32 { let h: Handle = vm.Vm.interpreter(); vm.close(h) }", mode);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 0);
    }
}

#[test]
fn alias_errors_preserve_codes_and_source_ranges() {
    for (body, marked, code) in [
        ("type A = Missing", "Missing", ErrorCode::UnknownType),
        (
            "type A = Int32 type A = Bool",
            "A",
            ErrorCode::DuplicateDefinition,
        ),
        ("type A = A", "A", ErrorCode::CyclicTypeAlias),
        ("type A = B type B = A", "A", ErrorCode::CyclicTypeAlias),
        (
            "type Count = Int32 fn f() -> Count { true }",
            "fn f() -> Count { true }",
            ErrorCode::TypeMismatch,
        ),
        (
            "type Count = Int32 fn f() { let x: Count = true }",
            "let x: Count = true",
            ErrorCode::TypeMismatch,
        ),
        (
            "type Handle = vm.Function type Callback = domain.Handle fn f(h: Handle) -> Callback { h }",
            "fn f(h: Handle) -> Callback { h }",
            ErrorCode::TypeMismatch,
        ),
        (
            "type Count = Int32 fn f() -> Count { Count(1) }",
            "Count(1)",
            ErrorCode::UnknownName,
        ),
    ] {
        let source = format!("// 🙂\n{body}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{body}: {error}");
        assert_eq!(error.code(), code, "{body}: {error}");
        assert_eq!(error.span().unwrap().slice(&source), Some(marked), "{body}");
    }
}

#[test]
fn alias_host_signatures_are_checked_after_normalization() {
    unsafe extern "C" fn context_value(value: u64) -> i32 {
        value as i32
    }
    extern "C" fn context() -> u64 {
        42
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_fn(
                "context",
                FunctionSignature::new(vec![], Type::Named("Host.Context".into())),
                context as *const u8,
            )
            .unwrap();
            vm.register_fn(
                "context_value",
                FunctionSignature::new(vec![Type::Named("Host.Context".into())], Type::Int32),
                context_value as *const u8,
            )
            .unwrap();
        }
        vm.load_source("type Context = Host.Context type Count = Int32 extern fn context() -> Context extern fn context_value(c: Context) -> Count extern fn record(x: Count) -> Count fn compute() -> Count { let ctx: Context = context(); record(context_value(ctx)) }").unwrap();
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [42]));
        let version = vm.module_version();
        let error = vm
            .load_source("type Wrong = Bool extern fn record(x: Wrong) -> Int32")
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::AbiError);
        assert_eq!(vm.module_version(), version);
        vm.register_opaque_type("Context").unwrap();
        let error = vm.load_source("type Context = Int32").unwrap_err();
        assert_eq!(error.code(), ErrorCode::DuplicateDefinition);
    }
}

#[test]
fn alias_reload_keeps_old_tasks_and_does_not_register_source_types() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(
            r#"
            type Value = Int32
            type Nothing = Unit
            extern fn delayed_value(x: Value) @suspends -> Value
            extern fn record(x: Value) -> Value
            fn current() -> Value { 42 }
            fn branch_task() @suspends -> Nothing { let n: Value = delayed_value(record(7)); record(n); }
            fn pending() @suspends -> Value { join { branch_task() }; 42 }
            fn failing() @suspends -> Value { let n: Value = delayed_value(record(-1)); 7 / n }
        "#,
            mode,
        );
        let mut task = vm.call_async("pending", &[]).unwrap();
        let mut failing = vm.call_async("failing", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failing.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        let version = vm.module_version();
        for invalid in [
            "type Value = Value",
            "type Value = Int32 type Value = Bool",
            "fn current() -> Value { 0 }",
        ] {
            assert!(vm.load_source(invalid).is_err());
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
        vm.load_source("type Value = Bool fn current() -> Value { true }")
            .unwrap();
        assert_eq!(vm.function_signature("current").unwrap().ret, Type::Bool);
        assert!(vm.call_int32_0("current").is_err());
        // A previously used source alias is still available for host registration.
        vm.register_opaque_type("Value").unwrap();
        assert!(vm.load_source("type Value = Int32").is_err());
        scheduler.cancel(cancelled).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(failing.cps_result(), None);
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [7, 7, -1, -1, 7, 7, 8]));
    }
}
