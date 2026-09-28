use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::{
    cps::CpsFrame,
    scheduler::{Scheduler, SchedulerError},
    suspension::ResumeError,
};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Span, Type};

#[test]
fn records_preserve_values_and_calls_in_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/records.jk"),
                suspends,
                Suspends::All,
            );
            let vm = parity_vm(&source, mode);
            for (name, expected) in [
                ("snapshots", 87108),
                ("argument_snapshots", 109),
                ("initializer_snapshots", 91),
                ("nested_updates", 389),
                ("conditions", 42),
                ("unit_effects", 3),
                ("calls", 89),
                ("empty_calls", 42),
            ] {
                let actual = if suspends {
                    vm.call_async(name, &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            let actual = if suspends {
                vm.call_async("control_flow", &[6])
                    .unwrap()
                    .cps_result()
                    .unwrap() as i32
            } else {
                vm.call_int32_1("control_flow", 6).unwrap()
            };
            assert_eq!(actual, 42);
        }
    }
}

#[test]
fn aliases_match_canonical_types_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/aliases.jk");
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
            "type Count = Int32 fn Count() -> Count { let Count: Count = 42 Count }",
            mode,
        );
        assert_eq!(vm.call_int32_0("Count").unwrap(), 42);
        let vm = parity_vm("import joky/vm type Handle = vm.Handle fn compute() -> Int32 { let h: Handle = vm.create_interpreter() vm.close(h) }", mode);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 0);
    }
}

#[test]
fn alias_errors_match_across_backends_and_preserve_source_ranges() {
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
            "type Handle = vm.Handle type Callback = vm.Function fn f(h: Handle) -> Callback { h }",
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
        let mut errors = Vec::new();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            errors.push(VmEngine::with_mode(mode).load_source(&source).unwrap_err());
        }
        let mut compiler = crate::aot::AotCompiler::new("invalid_alias").unwrap();
        errors.push(
            compiler
                .compile_module(&crate::parser::parse(&source).unwrap())
                .unwrap_err(),
        );
        for error in errors {
            assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{body}: {error}");
            assert_eq!(error.code(), code, "{body}: {error}");
            assert_eq!(error.span().unwrap().slice(&source), Some(marked), "{body}");
        }
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
        vm.load_source("type Context = Host.Context type Count = Int32 extern fn context() -> Context extern fn context_value(c: Context) -> Count extern fn record(x: Count) -> Count fn compute() -> Count { let ctx: Context = context() record(context_value(ctx)) }").unwrap();
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
            fn branch() @suspends -> Nothing { let n: Value = delayed_value(record(7)); record(n); }
            fn pending() @suspends -> Value { scope { | branch() } 42 }
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

#[test]
fn mutable_bindings_and_loops_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/loops.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            // Only declarations get the attribute, not calls or parentheses.
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            for (name, args, expected) in [
                ("sum", vec![0], 0),
                ("sum", vec![1], 0),
                ("sum", vec![10], 45),
                ("sum", vec![-1], 0),
                ("nested", vec![4], 10),
                ("snapshots", vec![], 20123),
                ("shadowing", vec![], 47),
                ("condition_effects", vec![], 53),
                ("bool_loop", vec![], 3),
                ("comparison_snapshot", vec![], 2),
                ("precedence", vec![], 9),
            ] {
                let actual = if suspends {
                    let task = vm.call_async(name, &args).unwrap();
                    assert!(task.is_finished());
                    task.cps_result().unwrap() as i32
                } else if args.is_empty() {
                    vm.call_int32_0(name).unwrap()
                } else {
                    vm.call_int32_1(name, args[0]).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            if suspends {
                assert!(vm.call_async("unit_values", &[]).unwrap().is_finished());
            } else {
                vm.call_unit_0("unit_values").unwrap();
            }
        }
    }
}

#[test]
fn semicolon_sequences_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/sequences.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            for (name, expected) in [
                ("sequence_condition", 43),
                ("sequence_bindings", 45),
                ("sequence_scope", 33),
                ("sequence_arguments", 23),
                ("sequence_precedence", 13),
                ("sequence_mixed_separators", 15),
            ] {
                let actual = if suspends {
                    let task = vm.call_async(name, &[]).unwrap();
                    assert!(task.is_finished());
                    task.cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            if suspends {
                assert!(vm
                    .call_async("sequence_trailing", &[])
                    .unwrap()
                    .is_finished());
            } else {
                vm.call_unit_0("sequence_trailing").unwrap();
            }
        }
    }
}

#[test]
fn semicolon_sequences_resume_without_replaying_prior_expressions() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32;
        extern fn record(x: Int32) -> Int32;
        fn branch(x: Int32) @suspends { delayed_value(x); }
        fn compute() @suspends -> Int32 {
            var checks = 0; var bodies = 0;
            while (checks = checks + 1; delayed_value(record(checks)) < 4) {
                bodies = bodies + 1;
                scope { | branch(bodies); | branch(bodies + 10); };
            };
            (checks * 10) + bodies
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 1]);
        READY.with(|ready| ready.set(true));
        for _ in 0..10 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(32));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [1, 1, 1, 11, 2, 2, 2, 12, 3, 3]
        );
    }
}

#[test]
fn core_expressions_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/expressions.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let call = |name: &str, args: &[i32]| {
                if suspends {
                    let task = vm.call_async(name, args).unwrap();
                    assert!(task.is_finished());
                    task.cps_result().unwrap() as i32
                } else {
                    match args {
                        [] => vm.call_int32_0(name).unwrap(),
                        [a] => vm.call_int32_1(name, *a).unwrap(),
                        [a, b] => vm.call_int32_2(name, *a, *b).unwrap(),
                        _ => unreachable!(),
                    }
                }
            };
            for (name, expected) in [
                ("expression_precedence", 7),
                ("expression_grouping", 9),
                ("expression_associativity", 14),
                ("expression_unary_precedence", -2),
                ("expression_minimum", i32::MIN),
                ("expression_wrapped_negation", i32::MIN),
                ("expression_snapshots", 89),
                ("expression_sequences", -5),
                ("expression_newline", 3),
                ("expression_sequence_arguments", -23),
            ] {
                assert_eq!(
                    call(name, &[]),
                    expected,
                    "{mode:?} suspends={suspends} {name}"
                );
            }
            for value in [i32::MIN, -1, 0, 1, i32::MAX] {
                assert_eq!(call("expression_negate", &[value]), value.wrapping_neg());
                assert_eq!(
                    call("expression_not_chain", &[value]),
                    i32::from(value == 0)
                );
            }
            for a in [0, 1] {
                for b in [0, 1] {
                    let expected = if a == b { 10 } else { 1 };
                    assert_eq!(call("expression_truth", &[a, b]), expected);
                    assert_eq!(call("expression_bool_loops", &[a, b]), expected);
                }
            }
        }
    }
}

#[test]
fn conditionals_and_short_circuiting_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/conditionals.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let call = |name: &str, args: &[i32]| {
                if suspends {
                    let task = vm.call_async(name, args).unwrap();
                    assert!(task.is_finished());
                    task.cps_result().unwrap() as i32
                } else {
                    match args {
                        [] => vm.call_int32_0(name).unwrap(),
                        [a] => vm.call_int32_1(name, *a).unwrap(),
                        [a, b] => vm.call_int32_2(name, *a, *b).unwrap(),
                        _ => unreachable!(),
                    }
                }
            };
            for (name, expected) in [
                ("conditional_scopes", 34),
                ("logical_precedence", 1),
                ("logical_effects", 3),
                ("logical_snapshots", 7),
                ("conditional_grouping", 11),
            ] {
                assert_eq!(
                    call(name, &[]),
                    expected,
                    "{mode:?} suspends={suspends} {name}"
                );
            }
            for value in [i32::MIN, -1, 0, 1, i32::MAX] {
                assert_eq!(call("choose", &[value]), value.signum() * 10);
            }
            for a in [0, 1] {
                for b in [0, 1] {
                    assert_eq!(call("logical_values", &[a, b]), (a & b) * 10 + (a | b));
                    assert_eq!(
                        call("conditional_nested", &[a, b]),
                        if a == 0 {
                            20
                        } else if b == 0 {
                            12
                        } else {
                            11
                        }
                    );
                }
            }
            for (n, expected) in [(-1, 0), (0, 0), (1, 1), (3, 12), (5, 32), (10, 32)] {
                assert_eq!(call("conditional_loop", &[n]), expected);
            }
            for (x, expected) in [(0, 0), (3, 4), (-1, 0)] {
                assert_eq!(call("conditional_safe_division", &[x]), expected);
            }
            if suspends {
                assert!(vm
                    .call_async("conditional_empty", &[])
                    .unwrap()
                    .is_finished());
            } else {
                vm.call_unit_0("conditional_empty").unwrap();
            }
        }
    }
}

#[test]
fn division_matches_in_direct_and_cps_execution_and_keeps_error_spans() {
    let source = include_str!("../../tests/fixtures/division.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            for a in [i32::MIN, -7, -1, 0, 1, 7, i32::MAX] {
                for b in [i32::MIN, -3, -1, 0, 1, 3, i32::MAX] {
                    let result = if suspends {
                        vm.call_async("divide", &[a, b])
                            .map(|task| task.cps_result().unwrap() as i32)
                    } else {
                        vm.call_int32_2("divide", a, b)
                    };
                    if b == 0 {
                        let error = result.unwrap_err();
                        assert_eq!(error.code(), ErrorCode::RuntimeError);
                        assert_eq!(error.message(), "Division by zero");
                        assert_eq!(error.span().unwrap().slice(&source), Some("a / b"));
                    } else {
                        assert_eq!(result.unwrap(), a.wrapping_div(b));
                    }
                }
            }
            let constant = if suspends {
                vm.call_async("divide_constant", &[])
                    .unwrap()
                    .cps_result()
                    .unwrap() as i32
            } else {
                vm.call_int32_0("divide_constant").unwrap()
            };
            assert_eq!(constant, i32::MIN);
            for (name, args, span) in [
                ("divide_nested", vec![7, 0], "a / b"),
                ("divide_recurse", vec![4], "7 / n"),
                ("divide_bool_caller", vec![0], "a / b"),
            ] {
                let error = if suspends {
                    vm.call_async(name, &args).unwrap_err()
                } else if args.len() == 1 {
                    vm.call_int32_1(name, args[0]).unwrap_err()
                } else {
                    vm.call_int32_2(name, args[0], args[1]).unwrap_err()
                };
                assert_eq!(error.code(), ErrorCode::RuntimeError);
                assert_eq!(error.span().unwrap().slice(&source), Some(span));
            }
            let error = if suspends {
                vm.call_async("divide_unit", &[0]).unwrap_err()
            } else {
                vm.call_unit_1("divide_unit", 0).unwrap_err()
            };
            assert_eq!(error.message(), "Division by zero");
            let value = if suspends {
                vm.call_async("divide_skipped", &[0])
                    .unwrap()
                    .cps_result()
                    .unwrap() as i32
            } else {
                vm.call_int32_1("divide_skipped", 0).unwrap()
            };
            assert_eq!(value, 42);
        }
    }
}

#[test]
fn division_failures_stop_later_operands_arguments_and_unit_calls() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for attribute in ["", "@suspends"] {
            for expression in [
                "record(1) / record(0) + record(99)",
                "pair(divide(record(1), record(0)), record(99))",
                "unit(record(0)); record(99)",
                "if flag(record(0)) { record(99) } else { record(98) }",
                "let text = text(record(0)); record(99)",
            ] {
                let source = format!(
                    r#"
                    extern fn record(x: Int32) -> Int32
                    fn divide(a: Int32, b: Int32) {attribute} -> Int32 {{ a / b }}
                    fn unit(b: Int32) {attribute} {{ divide(1, b); }}
                    fn flag(b: Int32) {attribute} -> Bool {{ divide(1, b) > 0 }}
                    fn text(b: Int32) {attribute} -> String {{ divide(1, b); "safe" }}
                    fn pair(a: Int32, b: Int32) {attribute} -> Int32 {{ record(97); a + b }}
                    fn compute() {attribute} -> Int32 {{ {expression} }}
                "#
                );
                let vm = parity_vm(&source, mode);
                let error = if attribute.is_empty() {
                    vm.call_int32_0("compute").unwrap_err()
                } else {
                    vm.call_async("compute", &[]).unwrap_err()
                };
                assert_eq!(error.code(), ErrorCode::RuntimeError, "{expression}");
                let span = error.span().unwrap().slice(&source).unwrap();
                assert!(span == "a / b" || span == "record(1) / record(0)", "{span}");
                EVENTS.with(|events| {
                    assert_eq!(
                        *events.borrow(),
                        if expression.contains("record(1)") {
                            vec![1, 0]
                        } else {
                            vec![0]
                        }
                    )
                });
            }
        }
    }
}

#[test]
fn division_after_pending_preserves_snapshots_spans_and_terminal_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "return record(7) / delayed_value(record(x))",
            "return divide(record(7), delayed_value(record(x)))",
            "var value = 7; value = value / delayed_value((value = 99; record(x))); value",
        ] {
            for (input, expected) in [(-1, None), (0, Some(7)), (1, Some(3))] {
                let source = format!(
                    r#"
                    extern fn delayed_value(x: Int32) @suspends -> Int32
                    extern fn record(x: Int32) -> Int32
                    fn divide(a: Int32, b: Int32) -> Int32 {{ a / b }}
                    fn compute(x: Int32) @suspends -> Int32 {{ {expression} }}
                "#
                );
                let mut vm = parity_vm(&source, mode);
                let mut task = vm.call_async("compute", &[input]).unwrap();
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                let events = if expression.starts_with("var") {
                    vec![input, input]
                } else {
                    vec![7, input, input]
                };
                EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
                vm.load_source("fn replacement() -> Int32 { 42 }").unwrap();
                drop(vm);
                READY.with(|ready| ready.set(true));
                if let Some(expected) = expected {
                    task.resume().unwrap();
                    assert_eq!(task.cps_result(), Some(expected));
                } else {
                    let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                        panic!("expected structured division error")
                    };
                    assert_eq!(error.code(), ErrorCode::RuntimeError);
                    assert_eq!(error.message(), "Division by zero");
                    let expected = if expression.contains("divide(") {
                        "a / b"
                    } else if expression.starts_with("var") {
                        "value / delayed_value((value = 99; record(x)))"
                    } else {
                        "record(7) / delayed_value(record(x))"
                    };
                    assert_eq!(error.span().unwrap().slice(&source), Some(expected));
                    assert_eq!(task.cps_result(), None);
                }
                assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
                EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
            }
        }
    }
}

#[test]
fn division_failure_stops_scope_siblings_and_cancelled_tasks() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn failing() @suspends { let value = 7 / delayed_value(record(-1)); record(99); }
        fn sibling() @suspends { delayed_value(record(2)); record(98); }
        fn compute() @suspends -> Int32 {
            scope { | failing() | sibling() }
            record(97)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, -1, 2, 2]));
        READY.with(|ready| ready.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected division error")
        };
        assert_eq!(
            error.span().unwrap().slice(source),
            Some("7 / delayed_value(record(-1))")
        );
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, -1, 2, 2]));
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
        scheduler.resume(cancelled).unwrap();
        scheduler.cancel(cancelled).unwrap();
        READY.with(|ready| ready.set(true));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        assert_eq!(crate::runtime::error::joky_runtime_error_code(), 0);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, -1, 2, 2]));
    }
}

#[test]
fn generated_native_host_failures_are_checked_before_decoding_results() {
    let source = "fn text() -> String { 7 / 0; \"invalid\" } fn number() -> Int32 { 8 / 0 }";
    let ast = crate::parser::parse(source).unwrap();
    let module = std::rc::Rc::new(
        crate::backends::jit::compile_with_registry(
            &ast,
            &[],
            &std::collections::HashMap::new(),
            vec![],
            crate::runtime::suspension::ModuleId(1),
            &crate::core::resolver::BindingRegistry::default(),
            &crate::TypeRegistry::default(),
        )
        .unwrap(),
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_owned_fn(
                "text",
                FunctionSignature::new(vec![], Type::String),
                module.exports["text"].entry,
                Some(module.clone()),
            )
            .unwrap();
            vm.register_owned_fn(
                "number",
                FunctionSignature::new(vec![], Type::Int32),
                module.exports["number"].entry,
                Some(module.clone()),
            )
            .unwrap();
        }
        vm.load_source(
            r#"
            extern fn text() -> String
            extern fn number() -> Int32
            extern fn record(x: Int32) -> Int32
            fn string_call() -> Int32 { let value = text(); record(99) }
            fn number_call() -> Int32 { number() + record(99) }
        "#,
        )
        .unwrap();
        for (name, expected) in [("string_call", "7 / 0"), ("number_call", "8 / 0")] {
            let error = vm.call_int32_0(name).unwrap_err();
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert_eq!(error.span().unwrap().slice(source), Some(expected));
            EVENTS.with(|events| assert!(events.borrow().is_empty()));
        }
    }
}

#[test]
fn jumps_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/jumps.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let call = |name: &str, args: &[i32]| {
                if suspends {
                    let task = vm.call_async(name, args).unwrap();
                    assert!(task.is_finished(), "{mode:?} {name}");
                    task.cps_result().unwrap() as i32
                } else {
                    match args {
                        [] => vm.call_int32_0(name).unwrap(),
                        [a] => vm.call_int32_1(name, *a).unwrap(),
                        _ => unreachable!(),
                    }
                }
            };
            for (name, expected) in [
                ("return_multiline", 42),
                ("return_sequence", 3),
                ("return_initializer", 42),
                ("return_assignment", 42),
                ("return_unary", 42),
                ("return_nested", 42),
                ("return_trailing", 42),
                ("return_condition", 42),
                ("break_only_condition", 42),
                ("break_in_return", 42),
                ("continue_in_return", 3),
                ("nested_condition_loops", 3),
                ("nested_breaks", 66),
                ("break_join", 6),
                ("continue_condition", 8),
                ("break_condition", 3),
                ("continue_and_break_paths", 3),
                ("short_circuit_jumps", 8),
            ] {
                assert_eq!(
                    call(name, &[]),
                    expected,
                    "{mode:?} suspends={suspends} {name}"
                );
            }
            for x in [-1i32, 0, 1, 3, 10] {
                assert_eq!(call("return_value", &[x]), x + 1);
                assert_eq!(call("return_flag", &[x]), i32::from(x != 0));
                assert_eq!(call("return_both", &[x]), if x < 0 { -1 } else { 1 });
                for name in ["return_mixed", "return_else"] {
                    assert_eq!(call(name, &[x]), if x < 0 { -1 } else { (x + 1) * 2 });
                }
                assert_eq!(call("return_argument", &[x]), if x < 0 { 9 } else { x + 1 });
                assert_eq!(call("return_operand", &[x]), if x < 0 { 9 } else { x * 2 });
                assert_eq!(call("return_short_circuit", &[x]), x.signum());
                assert_eq!(call("diverging_condition", &[x]), x.max(1));
                assert_eq!(call("break_or_return", &[x]), if x < 0 { -1 } else { 42 });
            }
            for (n, expected, returned) in
                [(0, 0, 0), (1, 1, 1), (2, 1, 2), (5, 13, 30), (10, 13, 30)]
            {
                assert_eq!(call("break_continue", &[n]), expected);
                assert_eq!(call("return_loop", &[n]), returned);
            }
            if suspends {
                assert!(vm.call_async("return_unit", &[]).unwrap().is_finished());
            } else {
                vm.call_unit_0("return_unit").unwrap();
            }
            assert_eq!(call("return_or_diverge", &[0]), 42);
        }
    }
}

#[test]
fn jumps_resume_without_replaying_conditions_operands_or_return_values() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn compute() @suspends -> Int32 {
            var i = 0 var total = 0
            while delayed_value(record(i)) < 10 {
                i = i + 1
                if i == 2 { continue }
                if i == 4 { break }
                total = total + delayed_value(record(i + 10))
            }
            return record(total) + delayed_value(record(20))
        }
        fn nested() @suspends -> Int32 {
            return delayed_value(record(1)) + delayed_value(record(2))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, inputs, expected) in [
            ("compute", vec![0, 11, 1, 2, 13, 3, 20], 47),
            ("nested", vec![1, 2], 5),
        ] {
            let mut vm = parity_vm(source, mode);
            let mut task = vm.call_async(name, &[]).unwrap();
            let mut events = Vec::new();
            for (index, input) in inputs.iter().enumerate() {
                if name == "compute" && index == inputs.len() - 1 {
                    events.push(26);
                    vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
                }
                events.extend([*input, *input]);
                for _ in 0..3 {
                    task.resume().unwrap();
                    assert!(!task.is_finished());
                }
                EVENTS.with(|actual| assert_eq!(*actual.borrow(), events, "{mode:?} {name}"));
                READY.with(|ready| ready.set(true));
                task.resume().unwrap();
                READY.with(|ready| ready.set(false));
            }
            assert_eq!(task.cps_result(), Some(expected));
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        }
    }
}

#[test]
fn jumps_wait_for_scope_completion_and_do_not_start_aborted_scopes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            fn branch(x: Int32) @suspends { return (delayed_value(x);) }
            fn skipped() @suspends -> Int32 {
                scope { | branch(record(1)) | branch(return 42) }
            }
            fn compute() @suspends -> Int32 {
                var i = 0
                while true {
                    i = i + 1
                    scope { | branch(i) | branch(i + 10) }
                    if i == 1 { continue }
                    break
                }
                return record(42)
            }
        "#,
            mode,
        );
        assert_eq!(
            vm.call_async("skipped", &[]).unwrap().cps_result(),
            Some(42)
        );
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1]));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("compute", &[]).unwrap();
        for iteration in 1..=2 {
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            let expected = if iteration == 1 {
                vec![1, 11]
            } else {
                vec![1, 11, 2, 12]
            };
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            READY.with(|ready| ready.set(false));
        }
        assert_eq!(task.cps_result(), Some(42));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 11, 2, 12, 42]));
    }
}

#[test]
fn return_failures_and_cancellation_do_not_complete_successfully() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        extern fn fail_now() @suspends
        fn compute() @suspends -> Int32 {
            return (delayed_value(record(1)); fail_now(); record(99))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        READY.with(|ready| ready.set(true));
        let error = task.resume().unwrap_err();
        match mode {
            VmMode::Jit => assert_eq!(error, ResumeError::InvalidCpsStatus(7)),
            VmMode::Interpreter => {
                let ResumeError::Failed(error) = error else {
                    panic!("expected host failure")
                };
                assert_eq!(error.code(), ErrorCode::HostError);
                assert_eq!(error.span().unwrap().slice(source), Some("fail_now()"));
            }
        }
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut scheduler = Scheduler::new();
        let id = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
        scheduler.resume(id).unwrap();
        scheduler.cancel(id).unwrap();
        READY.with(|ready| ready.set(true));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
    }
}

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
fn conditional_evaluation_skips_effects_failures_and_suspensions() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        extern fn fail_now() @suspends
        fn compute() @suspends -> Int32 {
            let a = false && (fail_now(); delayed_value(99) > 0)
            let b = true || (fail_now(); delayed_value(99) > 0)
            let x = if (record(1); a || b) { record(2) } else { fail_now(); delayed_value(99) }
            let y = if false { fail_now(); delayed_value(99) } else if (record(3); true) { record(4) } else { record(99) }
            if false { fail_now() }
            x * 10 + y
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let task = vm.call_async("compute", &[]).unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(24));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3, 4]));
    }
}

#[test]
fn conditional_paths_resume_once_and_keep_string_results_after_reload() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).unwrap().len() as i32
    }
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        extern fn byte_length(text: String) -> Int32
        fn compute(x: Int32) @suspends -> Int32 {
            var flag = true
            let text = if flag && delayed_value(record(x)) > 0 {
                flag = false
                delayed_value(record(10)); "left"
            } else {
                delayed_value(record(20)); "right"
            }
            let joined = byte_length(text) + if flag { 100 } else { 200 }
            record(joined) + delayed_value(record(30))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (input, arm, joined) in [(-2, 20, 105), (0, 10, 204)] {
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_suspending_fn(
                    "delayed_value",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    delayed_value,
                )
                .unwrap();
                vm.register_fn(
                    "record",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    record as *const u8,
                )
                .unwrap();
                vm.register_fn(
                    "byte_length",
                    FunctionSignature::new(vec![Type::String], Type::Int32),
                    byte_length as *const u8,
                )
                .unwrap();
            }
            vm.load_source(source).unwrap();
            let mut task = vm.call_async("compute", &[input]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [input, input]));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert!(!task.is_finished());
            READY.with(|ready| ready.set(false));
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [input, input, arm, arm]));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert!(!task.is_finished());
            READY.with(|ready| ready.set(false));
            for _ in 0..3 {
                task.resume().unwrap();
            }
            vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
            drop(vm);
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert_eq!(task.cps_result(), Some((joined + 31) as u64));
            EVENTS.with(|events| {
                assert_eq!(*events.borrow(), [input, input, arm, arm, joined, 30, 30])
            });
        }
    }
}

#[test]
fn short_circuit_operands_suspend_only_when_selected() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (operator, left, runs_right, expected) in [
            ("&&", -1, false, 0),
            ("&&", 0, true, 1),
            ("||", -1, true, 1),
            ("||", 0, false, 1),
        ] {
            let source = format!(
                r#"
                extern fn delayed_value(x: Int32) @suspends -> Int32
                extern fn record(x: Int32) -> Int32
                fn compute() @suspends -> Int32 {{
                    let selected = delayed_value(record({left})) > 0 {operator} delayed_value(record(2)) > 0
                    if selected {{ record(1) }} else {{ record(0) }}
                }}
            "#
            );
            let vm = parity_vm(&source, mode);
            let mut task = vm.call_async("compute", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [left, left]));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert_eq!(task.is_finished(), !runs_right);
            if runs_right {
                READY.with(|ready| ready.set(false));
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                EVENTS.with(|events| assert_eq!(*events.borrow(), [left, left, 2, 2]));
                READY.with(|ready| ready.set(true));
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(expected as u64));
            let events = if runs_right {
                vec![left, left, 2, 2, expected]
            } else {
                vec![left, left, expected]
            };
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
        }
    }
}

#[test]
fn conditional_failures_and_cancellation_skip_later_effects() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "if delayed_value(record(1)) > 0 { fail_now(); 1 } else { record(99) }",
            "if true && (delayed_value(record(1)); fail_now(); true) { record(99) } else { 0 }",
            "if false || (delayed_value(record(1)); fail_now(); true) { record(99) } else { 0 }",
        ] {
            let source = format!(
                r#"
                extern fn delayed_value(x: Int32) @suspends -> Int32
                extern fn record(x: Int32) -> Int32
                extern fn fail_now() @suspends
                fn compute() @suspends -> Int32 {{ {expression}; record(100) }}
            "#
            );
            let vm = parity_vm(&source, mode);
            let mut task = vm.call_async("compute", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            READY.with(|ready| ready.set(true));
            let error = task.resume().unwrap_err();
            match mode {
                VmMode::Jit => assert_eq!(error, ResumeError::InvalidCpsStatus(7)),
                VmMode::Interpreter => {
                    let ResumeError::Failed(error) = error else {
                        panic!("expected host failure")
                    };
                    assert_eq!(error.code(), ErrorCode::HostError);
                    assert_eq!(error.span().unwrap().slice(&source), Some("fail_now()"));
                }
            }
            assert_eq!(task.cps_result(), None);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
            EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut scheduler = Scheduler::new();
            let id = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
            scheduler.cancel(id).unwrap();
            READY.with(|ready| ready.set(true));
            scheduler.resume_all().unwrap();
            assert_eq!(scheduler.task_count(), 0);
            EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
        }
    }
}

#[test]
fn conditional_scope_branches_start_only_on_selected_paths() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "if x != 0 { scope { | branch(1) | branch(2) } }",
            "x != 0 && (scope { | branch(1) | branch(2) }; true)",
            "x == 0 || (scope { | branch(1) | branch(2) }; true)",
        ] {
            for input in [0, 1] {
                let source = format!(
                    r#"
                    extern fn delayed_value(x: Int32) @suspends -> Int32
                    extern fn record(x: Int32) -> Int32
                    fn branch(x: Int32) @suspends {{ delayed_value(x); }}
                    fn compute(x: Int32) @suspends -> Int32 {{ {expression}; record(42) }}
                "#
                );
                let vm = parity_vm(&source, mode);
                let mut task = vm.call_async("compute", &[input]).unwrap();
                if input == 1 {
                    assert!(!task.is_finished());
                    for _ in 0..3 {
                        task.resume().unwrap();
                    }
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
                    READY.with(|ready| ready.set(true));
                    task.resume().unwrap();
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 42]));
                } else {
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [42]));
                }
                assert_eq!(task.cps_result(), Some(42));
            }
        }
    }
}

#[test]
fn conditional_builtin_calls_and_opaque_results_resolve_and_execute() {
    let source = r#"
        import joky/vm
        fn compute(x: Int32) -> Int32 {
            let engine = if x == 0 { vm.create_interpreter() } else { vm.create() }
            vm.load_source(engine, "fn value() -> Int32 { 42 }")
            let result = if vm.call_int32(engine, "value") == 42 && vm.call_int32(engine, "value") != 0 {
                vm.call_int32(engine, "value")
            } else { vm.call_int32(engine, "value") + 1 }
            vm.close(engine)
            result
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        for input in [0, 1] {
            assert_eq!(vm.call_int32_1("compute", input).unwrap(), 42);
        }
    }
}

#[test]
fn unary_and_bool_operands_resume_once_with_snapshots_after_reload() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn delayed_bool(x: Int32) @suspends -> Bool { delayed_value(x) > 0 }
        fn compute() @suspends -> Int32 {
            var x = 5
            let result = -record(x) + -delayed_value((x = 10; record(x))) * 2
            var flag = true
            var test = flag == !delayed_bool((flag = false; record(-1)))
            var extra = 0
            while test { extra = record(42) test = false }
            result + extra
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [5, 10, 10]));
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        READY.with(|ready| ready.set(false));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [5, 10, 10, -1, -1]));
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(15));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [5, 10, 10, -1, -1, 42]));
    }
}

#[test]
fn unary_operands_propagate_failure_without_later_effects() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "-(delayed_value(record(1)); fail_now(); record(99))",
            "!(delayed_value(record(1)); fail_now(); record(99); true)",
        ] {
            let source = format!(
                r#"
                extern fn delayed_value(x: Int32) @suspends -> Int32
                extern fn record(x: Int32) -> Int32
                extern fn fail_now() @suspends
                fn compute() @suspends {{ {expression}; }}
            "#
            );
            let vm = parity_vm(&source, mode);
            let mut task = vm.call_async("compute", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            READY.with(|ready| ready.set(true));
            let error = task.resume().unwrap_err();
            match mode {
                VmMode::Jit => assert_eq!(error, ResumeError::InvalidCpsStatus(7)),
                VmMode::Interpreter => {
                    let ResumeError::Failed(error) = error else {
                        panic!("expected host failure")
                    };
                    assert_eq!(error.code(), ErrorCode::HostError);
                    assert_eq!(error.span().unwrap().slice(&source), Some("fail_now()"));
                }
            }
            assert_eq!(task.cps_result(), None);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
            EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
        }
    }
}

#[test]
fn builtin_calls_inside_sequences_resolve_and_execute() {
    let source = r#"
        import joky/vm;
        fn compute() -> Int32 {
            let engine = vm.create_interpreter();
            let result = (vm.load_source(engine, "fn value() -> Int32 { 42 }");
                          -vm.call_int32(engine, "value"));
            vm.close(engine);
            result
        };
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(
            parity_vm(source, mode).call_int32_0("compute").unwrap(),
            -42
        );
    }
}

#[test]
fn terminal_and_adjacent_loop_labels_complete_in_direct_and_cps_execution() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        fn empty() { while false {} }
        fn adjacent() { while false { while false {} } while false {} }
        fn counted(n: Int32) {
            var i = 0
            while i < n {
                record(i)
                i = i + 1
                while false {}
            }
        }
        fn counted_negative() { counted(0 - 1) }
        fn counted_zero() { counted(0) }
        fn counted_one() { counted(1) }
        fn counted_four() { counted(4) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            // Declarations here are indented, so match them anywhere in a line.
            let source = if suspends {
                source
                    .lines()
                    .map(|line| {
                        if line.trim_start().starts_with("fn ") {
                            line.replacen(')', ") @suspends", 1)
                        } else {
                            line.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                source.to_owned()
            };
            let vm = parity_vm(&source, mode);
            for name in ["empty", "adjacent"] {
                if suspends {
                    assert!(vm.call_async(name, &[]).unwrap().is_finished());
                } else {
                    vm.call_unit_0(name).unwrap();
                }
            }
            for (name, n) in [
                ("counted_negative", -1),
                ("counted_zero", 0),
                ("counted_one", 1),
                ("counted_four", 4),
            ] {
                EVENTS.with(|events| events.borrow_mut().clear());
                if suspends {
                    assert!(vm.call_async(name, &[]).unwrap().is_finished());
                } else {
                    vm.call_unit_0(name).unwrap();
                }
                assert_eq!(
                    EVENTS.with(|events| events.borrow().clone()),
                    (0..n).collect::<Vec<_>>(),
                    "{mode:?} suspends={suspends} n={n}"
                );
            }
        }
    }
}

#[test]
fn terminal_loop_condition_resumes_without_replaying_after_reload() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute(n: Int32) @suspends {
            var i = 0
            while delayed_value(record(i)) < n {
                i = i + 1
                while false {}
            }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[3]).unwrap();
        for _ in 0..3 {
            assert!(!task.is_finished());
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [0, 0]);
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        for _ in 0..3 {
            assert!(!task.is_finished());
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [0, 0, 1, 1, 2, 2]
        );
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}

#[test]
fn signed_int32_comparisons_cover_true_false_and_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for operator in ["==", "!=", "<", "<=", ">", ">="] {
            let source = format!(
                r#"
                fn compare(a: Int32, b: Int32) -> Int32 {{
                    var result = 0
                    var test = a {operator} b
                    while test {{ result = 1 test = false }}
                    result
                }}
            "#
            );
            let vm = parity_vm(&source, mode);
            for (a, b) in [
                (0, 0),
                (-1, 0),
                (0, -1),
                (i32::MIN, i32::MAX),
                (i32::MAX, i32::MIN),
            ] {
                let expected = match operator {
                    "==" => a == b,
                    "!=" => a != b,
                    "<" => a < b,
                    "<=" => a <= b,
                    ">" => a > b,
                    ">=" => a >= b,
                    _ => unreachable!(),
                };
                assert_eq!(
                    vm.call_int32_2("compare", a, b).unwrap(),
                    i32::from(expected),
                    "{mode:?}: {a} {operator} {b}"
                );
            }
        }
    }
}

#[test]
fn loops_resume_without_replaying_mutations_or_argument_effects() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute(n: Int32) @suspends -> Int32 {
            var i = 0
            var total = 0
            while i < n {
                let before = record(i + 100)
                total = total + delayed_value({ i = i + 1 i })
                record(before)
            }
            total
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[3]).unwrap();
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [100, 1]);
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [100, 1]);
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(9));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [100, 1, 100, 101, 2, 101, 102, 3, 102]
        );
    }
}

#[test]
fn immediate_comparison_conditions_resume_once_after_reload() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var checks = 0
            var total = 0
            while (checks = checks + 1; delayed_value(record(checks)) < 4) {
                total = total + (checks * 3)
                record(total)
            }
            checks * 10 + total
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        for expected in [
            vec![1, 1],
            vec![1, 1, 3, 2, 2],
            vec![1, 1, 3, 2, 2, 9, 3, 3],
        ] {
            READY.with(|ready| ready.set(false));
            for _ in 0..3 {
                task.resume().unwrap();
                assert!(!task.is_finished());
            }
            assert_eq!(EVENTS.with(|events| events.borrow().clone()), expected);
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(39));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [1, 1, 3, 2, 2, 9, 3, 3]
        );
    }
}

#[test]
fn fused_condition_keeps_operand_snapshots_across_assignment_and_pending() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var x = 0
            var bodies = 0
            while (x = x + 1; x + delayed_value({ x = x + 1; x }) < 8) {
                bodies = bodies + 1
            }
            x * 10 + bodies
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [2]);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        task.resume().unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(41));
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [2, 4]);
    }
}

#[test]
fn loop_conditions_can_suspend_and_scope_children_restart_each_iteration() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn branch(x: Int32) @suspends { delayed_value(x) {} }
        fn compute() @suspends -> Int32 {
            var i = 0
            while delayed_value(record(i)) <= 2 {
                scope { | branch(i + 10) | branch({ i = i + 1 i + 20 }) }
                record(i + 30)
            }
            i
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..2 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [0, 0]);
        READY.with(|ready| ready.set(true));
        for _ in 0..10 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(2));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [0, 0, 10, 21, 31, 1, 1, 11, 22, 32, 2, 2]
        );
    }
}

#[test]
fn in_place_arithmetic_preserves_snapshots_across_repeated_pending_and_reload() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var x = 1
            var i = 0
            while i < 2 {
                let before = x
                x = x + 1
                x = x + delayed_value({ x = x + 10; record(x) })
                record(before)
                i = i + 1
            }
            x
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [12, 12]);
        vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [12, 12, 1, 26, 26]
        );
        READY.with(|ready| ready.set(false));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [12, 12, 1, 26, 26]
        );
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(43));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [12, 12, 1, 26, 26, 15]
        );
    }
}

#[test]
fn interpreter_in_place_division_preserves_failure_order_after_resume() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var x = 12
            var i = 0
            while i < 1 {
                delayed_value(record(x))
                x = x / 0
                record(999)
                i = i + 1
            }
            x
        }
    "#;
    let vm = parity_vm(source, VmMode::Interpreter);
    let mut task = vm.call_async("compute", &[]).unwrap();
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(EVENTS.with(|events| events.borrow().clone()), [12, 12]);
    READY.with(|ready| ready.set(true));
    assert!(task.resume().is_err());
    assert!(task.is_finished());
    assert_eq!(task.cps_result(), None);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    assert_eq!(EVENTS.with(|events| events.borrow().clone()), [12, 12]);
}

#[test]
fn mutable_reads_are_snapshots_across_suspending_operands_and_scope_arguments() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pair(a: Int32, b: Int32) -> Int32 { (a * 10) + b }
        fn branch(x: Int32) @suspends { delayed_value(x) {} }
        fn compute() @suspends -> Int32 {
            var x = 1
            let saved = x + delayed_value({ x = 2 x })
            let args = pair(x, delayed_value({ x = 3 x }))
            scope { | branch(x) | branch({ x = 4 x }) }
            (saved * 100) + args
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        READY.with(|ready| ready.set(true));
        for _ in 0..4 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(424));
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [2, 3, 3, 4]);
    }
}

#[test]
fn loop_failures_and_cancellation_do_not_execute_later_iterations() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn fail_now() @suspends
        fn compute() @suspends {
            var i = 0
            while i < 3 {
                i = i + 1
                delayed_value(record(i))
                fail_now()
                record(999)
            }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        READY.with(|ready| ready.set(true));
        assert!(task.resume().is_err());
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 1]);
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut scheduler = Scheduler::new();
        let id = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
        scheduler.cancel(id).unwrap();
        READY.with(|ready| ready.set(true));
        scheduler.resume_all().unwrap();
        assert_eq!(scheduler.task_count(), 0);
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 1]);
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
fn builtin_calls_resolve_inside_mutable_initializers_assignments_and_loops() {
    let source = r#"
        import joky/vm
        fn compute() -> Int32 {
            var engine: vm.Handle = vm.create_interpreter()
            vm.close(engine)
            engine = vm.create_interpreter()
            var source = "fn value() -> Int32 { 7 }"
            var i = 0
            var total = 0
            while i < 2 {
                var status = vm.reload_source(engine, source)
                total = total + vm.call_int32(engine, "value") + status
                source = "fn value() -> Int32 { 9 }"
                i = i + 1
            }
            while vm.error_code() != 0 { vm.close(engine) }
            vm.close(engine)
            total
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(parity_vm(source, mode).call_int32_0("compute").unwrap(), 16);
    }
}

#[test]
fn jit_and_interpreter_match_sync_results_and_host_side_effects() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        fn compute(x: Int32) -> Int32 {
            let doubled = x * 2
            record((doubled + 3) * 4)
        }
    "#;
    let mut results = Vec::new();
    let mut effects = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        results.push(vm.call_int32_1("compute", 5).unwrap());
        effects.push(EVENTS.with(|events| events.borrow().clone()));
    }
    assert_eq!(results, [52, 52]);
    assert_eq!(effects, [vec![52], vec![52]]);
}

#[test]
fn jit_and_interpreter_match_cps_scope_results_and_poll_order() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn left(value: Int32) @suspends { delayed_value(value) {} }
        fn right(value: Int32) @suspends { delayed_value(value) {} }
        fn all(x: Int32) @suspends -> Int32 {
            let y = delayed_value(x)
            scope { | left(y) | right(y + 1) }
            record(y * 2)
        }
    "#;
    let mut results = Vec::new();
    let mut effects = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("all", &[4]).unwrap();
        READY.with(|ready| ready.set(true));
        for _ in 0..8 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        results.push(task.cps_result().unwrap() as i32);
        effects.push(EVENTS.with(|events| events.borrow().clone()));
    }
    assert_eq!(results, [10, 10]);
    assert_eq!(effects, [vec![4, 5, 6, 10], vec![4, 5, 6, 10]]);
}

#[test]
fn jit_and_interpreter_match_cps_failure_status() {
    let source = "extern fn fail_now() @suspends fn f() @suspends { fail_now() }";
    let mut statuses = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let error = vm.call_async("f", &[]).unwrap_err();
        statuses.push(error);
    }
    // Both backends must fail on the host status before any result exists,
    // with the same structured classification.
    for error in &statuses {
        assert_eq!(error.phase(), ErrorPhase::Runtime);
        assert_eq!(error.code(), ErrorCode::HostError);
        assert!(error.to_string().contains("failure status 7"), "{error}");
    }
    // The interpreter knows the failing binding's name; the native path only
    // sees the raw status through the C entry.
    assert!(
        statuses[1].to_string().contains("`fail_now`"),
        "{}",
        statuses[1]
    );
}

#[test]
fn jit_and_interpreter_match_module_reload_results() {
    let mut results = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn value(x: Int32) -> Int32 { x + 1 }", mode);
        let before = vm.call_int32_1("value", 41).unwrap();
        vm.load_source("fn value(x: Int32) -> Int32 { x + 10 }")
            .unwrap();
        let after = vm.call_int32_1("value", 41).unwrap();
        results.push((before, after, vm.module_version()));
    }
    assert_eq!(results[0].0, 42);
    assert_eq!(results[1].0, 42);
    assert_eq!(results[0].1, 51);
    assert_eq!(results[1].1, 51);
    assert_eq!(results[0].2, results[1].2);
}

#[test]
fn nested_calls_preserve_temporaries_and_resume_without_replaying_side_effects() {
    let vm = vm(r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn add(x: Int32, y: Int32) -> Int32 { x + y }
        fn inner(x: Int32) @suspends -> Int32 {
            record(100) + delayed_value(record(x))
        }
        fn outer(x: Int32) @suspends -> Int32 {
            let y = add(record(10), inner(x))
            let y = y + delayed_value(20)
            record(y)
        }
    "#);
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("outer", &[2]).unwrap());
    assert_eq!(scheduler.suspended_count(), 1);
    assert_eq!(
        scheduler.take_completed(id).unwrap_err(),
        SchedulerError::NotCompleted(id)
    );
    assert_eq!(scheduler.continuation(id).unwrap().cps_result(), None);
    EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 100, 2, 2]));
    // Polling before readiness must preserve the pending native frame.
    scheduler.resume(id).unwrap();
    EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 100, 2, 2]));
    // Other work can run while the native operation is pending.
    scheduler.spawn(|| {
        record(999);
    });
    scheduler.run_all();
    READY.with(|r| r.set(true));
    scheduler.resume(id).unwrap();
    assert_eq!(scheduler.suspended_count(), 1); // second pending call
    assert_eq!(scheduler.resume_all().unwrap(), 1);
    assert_eq!(scheduler.suspended_count(), 0);
    assert_eq!(scheduler.resume_all().unwrap(), 0);
    assert_eq!(scheduler.continuation(id).unwrap().cps_result(), Some(134));
    assert_eq!(
        scheduler.resume(id),
        Err(SchedulerError::Resume(ResumeError::AlreadyFinished))
    );
    EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 100, 2, 2, 999, 20, 134]));
    let result = scheduler.take_completed(id).unwrap();
    assert_eq!(result.cps_result(), Some(134));
    assert_eq!(
        scheduler.resume(id),
        Err(SchedulerError::UnknownContinuation(id))
    );
}

#[test]
fn scope_polls_all_branches_and_waits_for_each_completion() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn left(value: Int32) @suspends { delayed_value(value) {} }
        fn right(value: Int32) @suspends { delayed_value(value) {} }
        fn all() @suspends -> Int32 {
            scope {
                | left(1)
                | right(2)
            }
            42
        }
    "#);
    let mut task = vm.call_async("all", &[]).unwrap();
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
    assert!(!task.is_finished());
    READY.with(|ready| ready.set(true));
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(42));
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
}

#[test]
fn scope_requires_suspending_unit_function_branches() {
    let mut vm = VmEngine::new();
    for (source, message) in [
        (
            "fn f() { scope { | f() } }",
            "scope must be inside an @suspends function",
        ),
        (
            "fn branch() {} fn f() @suspends { scope { | branch() } }",
            "must be an @suspends function returning Unit",
        ),
        (
            "fn branch() @suspends -> Int32 { 1 } fn f() @suspends { scope { | branch() } }",
            "must be an @suspends function returning Unit",
        ),
        (
            "fn f() @suspends { scope { } }",
            "scope requires at least one branch",
        ),
    ] {
        assert!(
            vm.load_source(source)
                .unwrap_err()
                .message()
                .contains(message),
            "{source}"
        );
    }
}

#[test]
fn scope_failure_stops_polling_and_releases_siblings() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn fail_now() @suspends
        fn pending() @suspends { delayed_value(1) {} }
        fn failed() @suspends { fail_now() }
        fn all() @suspends { scope { | pending() | failed() } }
    "#);
    let error = vm.call_async("all", &[]).unwrap_err();
    assert!(format!("{error:?}").contains("failure status 7"));
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1]));
}

#[test]
fn nested_suspending_arguments_are_evaluated_left_to_right() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pair(x: Int32, y: Int32) @suspends -> Int32 { x * 10 + y }
        fn f() @suspends -> Int32 { pair(delayed_value(2), delayed_value(3)) }
    "#);
    let mut task = vm.call_async("f", &[]).unwrap();
    READY.with(|r| r.set(true));
    task.resume().unwrap();
    assert!(!task.is_finished());
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(34));
    EVENTS.with(|e| assert_eq!(*e.borrow(), [2, 3]));
}

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
                let nested = {{ let x = 20 x }}
                let x: Int32 = x + 1
                pick({{ record(1) identity(unit) }}, record(x + nested), {{ record(2) unit }})
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
fn immediate_completion_and_repeated_suspension_have_distinct_lifecycles() {
    let vm = vm(r#"
        fn immediate(x: Int32) @suspends -> Int32 { x + 1 }
        fn unit() @suspends { }
    "#);
    let mut task = vm.call_async("immediate", &[-42]).unwrap();
    assert_eq!(task.cps_result().unwrap() as i32, -41);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    assert!(vm.call_async("unit", &[]).unwrap().is_finished());
    assert!(vm.call_int32_1("immediate", 1).is_err());
    assert!(vm.call_async("immediate", &[]).is_err());
}

#[test]
fn cancellation_drops_pending_frames_and_does_not_reuse_ids() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn f() @suspends -> Int32 { delayed_value(1) }
    "#);
    let mut scheduler = Scheduler::new();
    let old = scheduler.suspend(vm.call_async("f", &[]).unwrap());
    scheduler.cancel(old).unwrap();
    let new = scheduler.suspend(vm.call_async("f", &[]).unwrap());
    assert_ne!(new, old);
    assert_eq!(
        scheduler.resume(old),
        Err(SchedulerError::UnknownContinuation(old))
    );
    assert_eq!(scheduler.suspended_count(), 1);
    READY.with(|r| r.set(true));
    scheduler.resume(new).unwrap();
    assert_eq!(scheduler.take_completed(new).unwrap().cps_result(), Some(2));
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

#[test]
fn interpreter_executes_register_bytecode_and_sync_host_calls() {
    let mut vm = VmEngine::new_interpreter();
    unsafe {
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source(
        r#"
        extern fn record(x: Int32) -> Int32
        fn add(x: Int32, y: Int32) -> Int32 { x + y }
        fn compute(x: Int32) -> Int32 { record(add(x, 2) * 3) }
    "#,
    )
    .unwrap();
    assert_eq!(vm.call_int32_1("compute", 4).unwrap(), 18);
    EVENTS.with(|events| assert_eq!(*events.borrow(), [18]));
}

#[test]
fn interpreter_preserves_string_values_and_borrowed_host_calls() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).map_or(-1, |text| text.len() as i32)
    }
    let mut vm = VmEngine::new_interpreter();
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
        fn text() -> String { "héllo" }
        fn identity(value: String) -> String { value }
        fn size() -> Int32 { byte_length(identity(text())) }
    "#,
    )
    .unwrap();
    assert_eq!(vm.call_int32_0("size").unwrap(), "héllo".len() as i32);
}

#[test]
fn interpreter_preserves_cps_frames_and_scope_branches() {
    let vm = parity_vm(
        r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn left(value: Int32) @suspends { delayed_value(value) {} }
        fn right(value: Int32) @suspends { delayed_value(value) {} }
        fn all(x: Int32) @suspends -> Int32 {
            let y = delayed_value(x)
            scope { | left(y) | right(y + 1) }
            y + 10
        }
    "#,
        VmMode::Interpreter,
    );
    let mut task = vm.call_async("all", &[4]).unwrap();
    EVENTS.with(|events| assert_eq!(*events.borrow(), [4]));
    READY.with(|ready| ready.set(true));
    task.resume().unwrap();
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(15));
    EVENTS.with(|events| assert_eq!(*events.borrow(), [4, 5, 6]));
}

#[test]
fn interpreter_continuation_keeps_old_module_after_reload() {
    let mut vm = parity_vm(
        "extern fn delayed_value(x: Int32) @suspends -> Int32 fn f(x: Int32) @suspends -> Int32 { delayed_value(x) + 10 }",
        VmMode::Interpreter,
    );
    let mut old = vm.call_async("f", &[1]).unwrap();
    vm.load_source(
        "extern fn delayed_value(x: Int32) @suspends -> Int32 fn f(x: Int32) @suspends -> Int32 { delayed_value(x) + 100 }",
    )
    .unwrap();
    drop(vm);
    READY.with(|ready| ready.set(true));
    old.resume().unwrap();
    assert_eq!(old.cps_result(), Some(12));
}

#[test]
fn failed_task_keeps_its_error_and_cannot_be_taken_as_completed() {
    use crate::runtime::suspension::ContinuationStatus;
    unsafe extern "C" fn bad_host(frame: *mut CpsFrame) -> i32 {
        if (*frame).state == 0 {
            (*frame).state = 1;
            1
        } else {
            9
        }
    }
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_suspending_fn("bad", FunctionSignature::new(vec![], Type::Unit), bad_host)
            .unwrap();
    }
    vm.load_source("extern fn bad() @suspends fn f() @suspends { bad() }")
        .unwrap();
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("f", &[]).unwrap());
    assert_eq!(
        scheduler.take_failed(id).unwrap_err(),
        SchedulerError::NotFailed(id)
    );
    assert_eq!(
        scheduler.resume_all(),
        Err(SchedulerError::Resume(ResumeError::InvalidCpsStatus(9)))
    );
    assert_eq!(scheduler.suspended_count(), 0);
    assert_eq!(
        scheduler.take_completed(id).unwrap_err(),
        SchedulerError::Resume(ResumeError::InvalidCpsStatus(9))
    );
    assert_eq!(
        scheduler.resume(id),
        Err(SchedulerError::Resume(ResumeError::AlreadyFinished))
    );
    assert_eq!(scheduler.resume_all().unwrap(), 0);
    let failed = scheduler.take_failed(id).unwrap();
    assert_eq!(
        failed.status(),
        ContinuationStatus::Failed(ResumeError::InvalidCpsStatus(9))
    );
    assert_eq!(failed.cps_result(), None);
    assert_eq!(
        scheduler.continuation(id).unwrap_err(),
        SchedulerError::UnknownContinuation(id)
    );
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

#[test]
fn record_suspensions_keep_snapshots_through_reload_failure_and_cancellation() {
    let source = r#"
        struct Point { x: Int32 y: Int32 }
        struct Empty {}
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn original() -> Point { Point { x: 1 y: 2 } }
        fn build(p: Point, n: Int32) @suspends -> Point {
            Point { y: delayed_value(record(n)) x: p.x }
        }
        fn pass(p: Point) -> Point { p }
        fn empty() @suspends -> Empty { delayed_value(0); Empty {} }
        fn pending() @suspends -> Int32 {
            var p = original()
            let q = build(p, { p.x = 9 7 })
            let r = pass(q)
            r.x * 100 + r.y * 10 + p.x
        }
        fn failing() @suspends -> Int32 {
            let p = build(original(), -1)
            let q = Point { x: 7 / p.y y: record(99) }
            q.x
        }
        fn child(p: Point, delta: Int32) @suspends {
            let q = build(p, delta)
            record(q.x + q.y);
        }
        fn scoped() @suspends -> Int32 {
            var p = original()
            scope { | child(p, 1) | child({ p.x = 9 p }, 2) }
            empty()
            p.x
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let old_type = vm.function_signature("original").unwrap().ret;
        let mut task = vm.call_async("pending", &[]).unwrap();
        let mut failing = vm.call_async("failing", &[]).unwrap();
        let mut scope = vm.call_async("scoped", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failing.resume().unwrap();
            scope.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 1, 1, 2, 2, 7, 7]));
        let version = vm.module_version();
        assert!(vm.load_source("struct Point { next: Point }").is_err());
        assert_eq!(vm.module_version(), version);
        vm.load_source(
            "struct Point { x: Int32 y: Int32 } fn original() -> Point { Point { x: 8 y: 9 } }",
        )
        .unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert!(vm.call_int32_0("original").is_err());
        assert!(vm.call_async("original", &[]).is_err());
        // Canonical source records cannot be reinterpreted as host scalar handles.
        let error = unsafe {
            vm.register_fn(
                "host_record",
                FunctionSignature::new(vec![], old_type),
                record as *const u8,
            )
        }
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::AbiError);
        scheduler.cancel(cancelled).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(189));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(failing.cps_result(), None);
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        for _ in 0..4 {
            if !scope.is_finished() {
                scope.resume().unwrap();
            }
        }
        assert_eq!(scope.cps_result(), Some(9));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| {
            assert_eq!(*events.borrow(), [7, 7, -1, -1, 1, 1, 2, 2, 7, 7, 3, 12, 0])
        });
    }
}

#[test]
fn invalid_records_have_consistent_diagnostics_before_publication() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn current() -> Int32 { 42 }", mode);
        let version = vm.module_version();
        for (source, code, message) in [
            ("struct P { x: Int32 x: Bool }", ErrorCode::DuplicateDefinition, "Duplicate field"),
            ("struct P {} struct P {}", ErrorCode::DuplicateDefinition, "already defined"),
            ("struct P {} type P = Int32", ErrorCode::DuplicateDefinition, "already defined"),
            ("struct P { x: Missing }", ErrorCode::UnknownType, "Unknown type"),
            ("struct P { x: String }", ErrorCode::TypeMismatch, "copyable record"),
            ("struct P { x: vm.Handle }", ErrorCode::TypeMismatch, "copyable record"),
            ("struct P { x: P }", ErrorCode::RecursiveRecord, "P -> P"),
            ("struct A { b: B } struct B { a: A }", ErrorCode::RecursiveRecord, "A -> B -> A"),
            ("struct P { x: Alias } type Alias = P", ErrorCode::RecursiveRecord, "P -> Alias -> P"),
            ("struct P {} type A = B type B = A", ErrorCode::CyclicTypeAlias, "A -> B -> A"),
            ("struct P {} fn f() { P {} }", ErrorCode::TypeMismatch, "Return type mismatch"),
            ("struct P { x: Int32 } fn f() -> P { P {} }", ErrorCode::TypeMismatch, "Missing field"),
            ("struct P { x: Int32 } fn f() -> P { P { x: 1 x: 2 } }", ErrorCode::DuplicateDefinition, "Duplicate initializer"),
            ("struct P { x: Int32 } fn f() -> P { P { z: 1 } }", ErrorCode::UnknownName, "no field `z`"),
            ("struct P { x: Int32 } fn f() -> P { P { x: true } }", ErrorCode::TypeMismatch, "expects Int32"),
            ("struct P {} type A = P fn f() -> P { A {} }", ErrorCode::UnknownType, "Unknown record constructor"),
            ("struct P {} struct Q {} fn f() -> P { Q {} }", ErrorCode::TypeMismatch, "Return type mismatch"),
            ("struct P { x: Int32 } fn f() { let p = P { x: 1 } p.x = 2 }", ErrorCode::InvalidAssignment, "immutable binding"),
            ("struct P { x: Int32 } fn f() { var p = P { x: 1 } p.x = true }", ErrorCode::TypeMismatch, "Field assignment"),
            ("struct P { x: Int32 } fn f() -> Int32 { P { x: 1 }.y }", ErrorCode::UnknownName, "no field `y`"),
            ("fn f() -> Int32 { 1.x }", ErrorCode::TypeMismatch, "has no fields"),
            ("struct P {} fn f() -> Bool { P {} == P {} }", ErrorCode::TypeMismatch, "Comparison operands"),
            ("struct P {} extern fn host(p: P)", ErrorCode::AbiError, "host ABI"),
            ("struct P {} extern fn host() @suspends -> P", ErrorCode::AbiError, "host ABI"),
            ("import joky/vm struct P {} fn f() { let vm = P {} vm.create(); }", ErrorCode::UnknownName, "has no method"),
            ("struct P { x: Int32 y: Int32 } fn f() -> P { P { x: return P { x: 1 y: 2 } y: 3 } }", ErrorCode::UnreachableCode, "Unreachable expression"),
        ] {
            let source = format!("// 🙂\n{source}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{mode:?}: {source}: {error}");
            assert!(error.message().contains(message), "{source}: {error}");
            assert!(error.span().is_some_and(|span| span.slice(&source).is_some()), "{error}");
            let mut aot = crate::aot::AotCompiler::new("invalid_record").unwrap();
            let error = aot.compile_module(&crate::parser::parse(&source).unwrap()).unwrap_err();
            assert_eq!(error.code(), code, "AOT: {source}: {error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
}

#[test]
fn wide_record_parameters_and_mixed_fields_match_all_call_conventions() {
    let mut source = String::from("struct Wide { before: Unit flag: Bool ");
    for n in 0..12 {
        source.push_str(&format!("f{n}: Int32 "));
    }
    source.push_str("after: Unit }\nfn pass(x: Wide, delta: Int32) -> Wide { var y = x y.f11 = y.f11 + delta y }\nfn run() -> Int32 { let p = Wide { before: {} flag: true ");
    for n in 0..12 {
        source.push_str(&format!("f{n}: {n} "));
    }
    source.push_str(
        "after: {} } let q = pass(p, -20) if q.flag { q.f0 + q.f7 + q.f11 } else { 0 } }",
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(&source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let value = if suspends {
                vm.call_async("run", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("run").unwrap()
            };
            assert_eq!(value, -2, "{mode:?} suspends={suspends}");
        }
    }
}

#[test]
fn enums_preserve_values_and_matching_in_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/enums.jk"),
                suspends,
                Suspends::All,
            );
            let vm = parity_vm_with_prelude(&source, mode, false);
            for (name, expected) in [
                ("nested", 7),
                ("replacements", 1086),
                ("effects", 142),
                ("snapshots", 128),
                ("arguments", 11),
                ("aggregate_arms", 49),
                ("loops", 85),
                ("scopes", 921),
                ("calls", 56),
                ("edge_cases", 13),
                ("compute", 2498),
            ] {
                let actual = if suspends {
                    vm.call_async(name, &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
        }
    }
}

#[test]
fn invalid_enums_and_patterns_are_rejected_before_publication() {
    let cases = [
        ("enum E {}", ErrorCode::InvalidPattern, "at least one"),
        ("enum E { A A }", ErrorCode::DuplicateDefinition, "variant"),
        ("enum E { _ }", ErrorCode::DuplicateDefinition, "reserved"),
        (
            "enum E { A } struct E {}",
            ErrorCode::DuplicateDefinition,
            "already defined",
        ),
        (
            "enum E { A } type E = Int32",
            ErrorCode::DuplicateDefinition,
            "already defined",
        ),
        (
            "enum E { A(Missing) }",
            ErrorCode::UnknownType,
            "Unknown type",
        ),
        ("enum E { A(String) }", ErrorCode::TypeMismatch, "copyable"),
        (
            "enum E { A(vm.Handle) }",
            ErrorCode::TypeMismatch,
            "copyable",
        ),
        ("enum E { A(E) }", ErrorCode::RecursiveType, "E -> E"),
        (
            "enum E { A(Alias) } type Alias = E",
            ErrorCode::RecursiveType,
            "E -> Alias -> E",
        ),
        (
            "enum E { A(R) } struct R { e: E }",
            ErrorCode::RecursiveType,
            "Infinite inline",
        ),
        (
            "enum E { A } fn f() -> E { E {} }",
            ErrorCode::InvalidCall,
            "qualified variant",
        ),
        (
            "enum E { A } fn f() -> E { E.A() }",
            ErrorCode::InvalidCall,
            "no parentheses",
        ),
        (
            "enum E { A } fn f() -> E { E.Unknown }",
            ErrorCode::UnknownName,
            "no variant",
        ),
        (
            "enum E { A } fn f() -> E { E.Unknown(1) }",
            ErrorCode::UnknownName,
            "no variant",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A }",
            ErrorCode::InvalidCall,
            "one payload",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A() }",
            ErrorCode::InvalidCall,
            "one payload",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A(1, 2) }",
            ErrorCode::InvalidCall,
            "one payload",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A(true) }",
            ErrorCode::TypeMismatch,
            "expects Int32",
        ),
        (
            "enum E { A } type Alias = E fn f() -> E { Alias.A }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "enum E { A } type Alias = E fn f() -> E { Alias.A() }",
            ErrorCode::UnknownName,
            "Undeclared function",
        ),
        (
            "enum E { A } fn f() -> E { A }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "enum E { A } fn f() -> E { let E = 1 E.A }",
            ErrorCode::TypeMismatch,
            "has no fields",
        ),
        (
            "enum E { A(Int32) } fn f() -> E { let E = 1 E.A(1) }",
            ErrorCode::UnknownName,
            "has no method",
        ),
        (
            "enum E { A } enum F { A } fn f() -> E { F.A }",
            ErrorCode::TypeMismatch,
            "Return type",
        ),
        (
            "enum E { A B } fn f(e: E) { match e { | A => {} } }",
            ErrorCode::NonExhaustiveMatch,
            "B",
        ),
        (
            "enum E { A } fn f(e: E) { match e {} }",
            ErrorCode::NonExhaustiveMatch,
            "A",
        ),
        (
            "fn f() { match 1 { | _ => {} } }",
            ErrorCode::TypeMismatch,
            "enum value",
        ),
        (
            "enum E { A B } fn f(e: E) { match e { | A => {} | A => {} | _ => {} } }",
            ErrorCode::UnreachableCode,
            "Duplicate match",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | A => {} | _ => {} } }",
            ErrorCode::UnreachableCode,
            "complete coverage",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | _ => {} | A => {} } }",
            ErrorCode::UnreachableCode,
            "complete coverage",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | Unknown => {} } }",
            ErrorCode::InvalidPattern,
            "no variant",
        ),
        (
            "enum E { A } enum F { A } fn f(e: E) { match e { | F.A => {} } }",
            ErrorCode::InvalidPattern,
            "does not belong",
        ),
        (
            "enum E { A } type Alias = E fn f(e: E) { match e { | Alias.A => {} } }",
            ErrorCode::InvalidPattern,
            "does not belong",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | A(x) => {} } }",
            ErrorCode::InvalidPattern,
            "payload arity",
        ),
        (
            "enum E { A(Int32) } fn f(e: E) { match e { | A => {} } }",
            ErrorCode::InvalidPattern,
            "payload arity",
        ),
        (
            "enum E { A(Int32) B } fn f(e: E) -> Int32 { match e { | A(x) => x | B => true } }",
            ErrorCode::TypeMismatch,
            "same type",
        ),
        (
            "enum E { A(Int32) B } fn f(e: E) { match e { | A(x) => {} | B => x; } }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "enum E { A(Int32) } fn f(e: E) { match e { | A(x) => x = 2 } }",
            ErrorCode::InvalidAssignment,
            "immutable",
        ),
        (
            "enum E { A } fn f(e: E) { match e { | _ => let inside = 1 } inside; }",
            ErrorCode::UnknownName,
            "inside",
        ),
        (
            "enum E { A } fn f(e: E) { match { let inside = e inside } { | _ => {} } inside; }",
            ErrorCode::UnknownName,
            "inside",
        ),
        (
            "enum E { A } fn f(e: E) -> Bool { e == E.A }",
            ErrorCode::TypeMismatch,
            "Comparison operands",
        ),
        (
            "enum E { A } fn f() { E.A.field; }",
            ErrorCode::TypeMismatch,
            "has no fields",
        ),
        (
            "enum E { A } extern fn host(e: E)",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "enum E { A } extern fn host() @suspends -> E",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "enum E { A(Int32) } fn host() @suspends -> Int32 { 1 } fn f() -> E { E.A(host()) }",
            ErrorCode::InvalidCall,
            "@suspends",
        ),
        (
            "enum E { A } fn f(e: E) -> Int32 { match e { | A => return 1 } 2 }",
            ErrorCode::UnreachableCode,
            "Unreachable expression",
        ),
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn current() -> Int32 { 42 }", mode);
        let version = vm.module_version();
        for (source, code, message) in cases {
            let source = format!("// 🙂\n{source}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{mode:?}: {source}: {error}");
            assert!(error.message().contains(message), "{source}: {error}");
            assert!(
                error
                    .span()
                    .is_some_and(|span| span.slice(&source).is_some()),
                "{error}"
            );
            let mut aot = crate::aot::AotCompiler::new("invalid_enum").unwrap();
            let error = aot
                .compile_module(&crate::parser::parse(&source).unwrap())
                .unwrap_err();
            assert_eq!(error.code(), code, "AOT: {source}: {error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
}

#[test]
fn enum_payloads_and_match_arms_suspend_without_replay_or_lost_ownership() {
    let source = r#"
        struct Pair { x: Int32 flag: Bool }
        enum E { Empty Number(Int32) Pair(Pair) }
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn build(n: Int32) @suspends -> E { E.Pair(Pair { x: delayed_value(record(n)) flag: true }) }
        fn original() -> E { E.Number(1) }
        fn pass(e: E) -> E { e }
        fn read(e: E) -> Int32 { match e { | Number(n) => n | Pair(p) => p.x | _ => 0 } }
        fn pending() @suspends -> Int32 {
            var e = original()
            let out = match e {
                | Number(n) => {
                    e = build(7)
                    let p = pass(e)
                    delayed_value(record(n + read(p)))
                }
                | _ => record(99)
            }
            out * 10 + read(e)
        }
        fn failing() @suspends -> Int32 {
            match build(-1) { | Pair(p) => 1 / p.x | _ => record(99) }
        }
        fn child(e: E) @suspends { let p = build(read(e)) record(read(p)); }
        fn scoped() @suspends -> Int32 {
            scope { | child(E.Number(2)) | child(E.Number(3)) }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let old_type = vm.function_signature("original").unwrap().ret;
        let mut task = vm.call_async("pending", &[]).unwrap();
        let mut failing = vm.call_async("failing", &[]).unwrap();
        let mut scoped = vm.call_async("scoped", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failing.resume().unwrap();
            scoped.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 2, 2, 3, 3, 7, 7]));
        vm.load_source("enum E { Empty Number(Int32) } fn original() -> E { E.Empty }")
            .unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert!(vm.call_int32_0("original").is_err());
        assert!(vm.call_async("original", &[]).is_err());
        let error = unsafe {
            vm.register_fn(
                "host_enum",
                FunctionSignature::new(vec![], old_type),
                record as *const u8,
            )
        }
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::AbiError);
        scheduler.cancel(cancelled).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(108));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(failing.cps_result(), None);
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        for _ in 0..4 {
            if !scoped.is_finished() {
                scoped.resume().unwrap();
            }
        }
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| {
            assert_eq!(
                *events.borrow(),
                [7, 7, -1, -1, 2, 2, 3, 3, 7, 7, 9, 9, 3, 4]
            )
        });
    }
}

#[test]
fn enum_constructors_and_match_bodies_respect_import_resolution() {
    let source = r#"
        import joky/vm
        type Count = Int32
        enum E { Empty Value(Count) }
        fn run() -> Int32 {
            let h = vm.create()
            vm.load_source(h, "fn f() -> Int32 { 42 }")
            let result = match E.Value(vm.call_int32(h, "f")) {
                | Value(n) => { let x: Count = vm.call_int32(h, "f") n + x }
                | _ => 0
            }
            vm.close(h)
            result
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("run").unwrap(), 84);
        let vm = parity_vm("import joky/vm as runtime enum vm { call_int32(Int32) register_int32(Int32) } fn run() -> Int32 { match vm.call_int32(42) { | call_int32(n) => n | register_int32(n) => n } }", mode);
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
    }
}

#[test]
fn wide_enums_keep_typed_payload_lanes_across_register_and_stack_arguments() {
    let mut source = "struct Wide { flag: Bool ".to_owned();
    for n in 0..12 {
        source.push_str(&format!("f{n}: Int32 "));
    }
    source.push_str("} enum E { Empty Payload(Wide) Flag(Bool) Number(Int32) }\nfn pass(e: E, n: Int32) -> E { match e { | Payload(p) => { var copy = p copy.f11 = copy.f11 + n E.Payload(copy) } | _ => e } }\nfn run() -> Int32 { var e = E.Payload(Wide { flag: true ");
    for n in 0..12 {
        source.push_str(&format!("f{n}: {n} "));
    }
    source.push_str("}) let saved = e e = E.Empty let empty = pass(e, 0) e = E.Flag(false) let flag = pass(e, 0) e = E.Number(-3) let value = pass(saved, -20) match value { | Payload(p) => if p.flag { p.f0 + p.f7 + p.f11 } else { 0 } | _ => 0 } }");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(&source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let result = if suspends {
                vm.call_async("run", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("run").unwrap()
            };
            assert_eq!(result, -2, "{mode:?} suspends={suspends}");
        }
    }
}

#[test]
fn tuples_preserve_structure_snapshots_and_bindings_in_direct_and_cps_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/tuples.jk"),
                suspends,
                Suspends::Typed,
            );
            let vm = parity_vm(&source, mode);
            for (name, expected) in [
                ("basic", 735),
                ("nested", 5),
                ("snapshots", 1934),
                ("mutable_leaves", 18567),
                ("shadowing", 225),
                ("effects", 1114),
                ("sequence_boundaries", 12),
                ("zero_calls", 42),
                ("calls", 91),
                ("loops", 85),
                ("abrupt", 3),
                ("compute", 22813),
            ] {
                let value = if suspends {
                    vm.call_async(name, &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(value, expected, "{mode:?} suspends={suspends} {name}");
            }
        }
    }
}

#[test]
fn invalid_tuples_and_binding_patterns_preserve_the_previous_module() {
    let cases = [
        (
            "fn f() -> (Int32,) { (true,) }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() -> (Int32,) { 1 }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() -> Int32 { (1,) }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() { let (x,) = 1 }",
            ErrorCode::TypeMismatch,
            "requires a tuple",
        ),
        (
            "fn f() { let (x, y) = (1,) }",
            ErrorCode::TypeMismatch,
            "has 2 elements",
        ),
        (
            "fn f() { let (x,) = (1, 2) }",
            ErrorCode::TypeMismatch,
            "has 1 elements",
        ),
        (
            "fn f() { let (x, (y,)) = (1, 2) }",
            ErrorCode::TypeMismatch,
            "requires a tuple",
        ),
        (
            "fn f() { let (x, x) = (1, 2) }",
            ErrorCode::DuplicateDefinition,
            "Duplicate binding",
        ),
        (
            "fn f() { var (x, (x,)) = (1, (2,)) }",
            ErrorCode::DuplicateDefinition,
            "Duplicate binding",
        ),
        (
            "fn f() { let (x, y) = (1, 2) x = 3 }",
            ErrorCode::InvalidAssignment,
            "immutable",
        ),
        (
            "fn f() { var (x, y) = (1, 2) x = true }",
            ErrorCode::TypeMismatch,
            "Assignment type",
        ),
        (
            "fn f() { let (x, y): (Int32, Bool) = (1, 2) }",
            ErrorCode::TypeMismatch,
            "annotation expects",
        ),
        (
            "fn f() { let _: Bool = 1 }",
            ErrorCode::TypeMismatch,
            "annotation expects",
        ),
        (
            "fn f() { let _ = 1 _; }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f() { let (_, _) = (1, 2) _ = 3 }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f() { { let (x, y) = (1, 2) } x; }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f() { let (x, y) = (1, x) }",
            ErrorCode::UnknownName,
            "Undefined variable",
        ),
        (
            "fn f() { let _ = (\"text\",) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "fn f(h: vm.Handle) { let _ = (h,) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "fn f(p: (String,)) {}",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "type Bad = (Missing,)",
            ErrorCode::UnknownType,
            "Unknown type",
        ),
        (
            "type Bad = (String,)",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        ("type A = (A,)", ErrorCode::CyclicTypeAlias, "A -> A"),
        (
            "type A = (Int32, B) type B = (A,)",
            ErrorCode::CyclicTypeAlias,
            "A -> B -> A",
        ),
        (
            "struct R { pair: (R,) }",
            ErrorCode::RecursiveRecord,
            "R -> R",
        ),
        ("enum E { More((E,)) }", ErrorCode::RecursiveType, "E -> E"),
        (
            "struct R { pair: (Alias,) } type Alias = R",
            ErrorCode::RecursiveRecord,
            "R -> Alias -> R",
        ),
        (
            "enum E { More((R,)) } struct R { value: E }",
            ErrorCode::RecursiveType,
            "Infinite inline",
        ),
        (
            "struct R { pair: (String,) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "enum E { Value((String,)) }",
            ErrorCode::TypeMismatch,
            "copyable aggregate",
        ),
        (
            "struct R {} struct S {} fn f() -> (R,) { (S {},) }",
            ErrorCode::TypeMismatch,
            "Return type mismatch",
        ),
        (
            "fn f() -> Bool { (1,) == (1,) }",
            ErrorCode::TypeMismatch,
            "Comparison operands",
        ),
        (
            "fn f() { let p = (1,) p.x; }",
            ErrorCode::TypeMismatch,
            "has no fields",
        ),
        (
            "extern fn host(p: (Int32, Bool))",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "extern fn host() @suspends -> (Unit,)",
            ErrorCode::AbiError,
            "host ABI",
        ),
        (
            "fn f() -> (Int32, Int32) { (return (1, 2), 3) }",
            ErrorCode::UnreachableCode,
            "Unreachable expression",
        ),
        (
            "fn f() -> (Int32, Int32) { if true { (1, 2) } else { (1, false) } }",
            ErrorCode::TypeMismatch,
            "same type",
        ),
        (
            "fn inner() @suspends -> (Int32,) { (1,) } fn f() { let (x,) = inner() }",
            ErrorCode::InvalidCall,
            "@suspends",
        ),
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn current() -> Int32 { 42 }", mode);
        let version = vm.module_version();
        for (source, code, message) in cases {
            let source = format!("// 🙂\n{source}");
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{mode:?}: {source}: {error}");
            assert!(error.message().contains(message), "{source}: {error}");
            assert!(
                error
                    .span()
                    .is_some_and(|span| span.slice(&source).is_some()),
                "{error}"
            );
            let mut aot = crate::aot::AotCompiler::new("invalid_tuple").unwrap();
            let error = aot
                .compile_module(&crate::parser::parse(&source).unwrap())
                .unwrap_err();
            assert_eq!(error.code(), code, "AOT: {source}: {error}");
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("current").unwrap(), 42);
        }
    }
}

#[test]
fn tuple_construction_and_destructuring_resume_once_after_reload_and_cancellation() {
    let source = r#"
        struct Marker { value: Int32 }
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn original() -> (Marker,) { (Marker { value: 1 },) }
        fn direct(n: Int32) -> (Int32, Bool) { (n, true) }
        fn build(n: Int32) @suspends -> (Int32, Bool) { (delayed_value(record(n)), true) }
        fn empty() @suspends -> (Unit, Unit) { (delayed_value(0);, {}) }
        fn pending() @suspends -> Int32 {
            var n = 1
            var (saved, (value, flag), unit) = (n, build({ n = 9 7 }), {})
            saved = saved + 1
            let (again, _) = direct(value)
            let _ = empty()
            if flag { saved * 100 + again * 10 + n } else { 0 }
        }
        fn failing() @suspends -> Int32 {
            let (value, _) = build(-1)
            let (_, _) = (1 / value, record(99))
            42
        }
        fn child(pair: (Int32, Bool)) @suspends {
            let (n, flag) = pair
            let (value, _) = build(n)
            if flag { record(value); }
        }
        fn scoped() @suspends -> Int32 {
            scope { | child((2, true)) | child((3, true)) }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let old_type = vm.function_signature("original").unwrap().ret;
        let mut task = vm.call_async("pending", &[]).unwrap();
        let mut failing = vm.call_async("failing", &[]).unwrap();
        let mut scoped = vm.call_async("scoped", &[]).unwrap();
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("pending", &[]).unwrap());
        for _ in 0..3 {
            task.resume().unwrap();
            failing.resume().unwrap();
            scoped.resume().unwrap();
            scheduler.resume(cancelled).unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 2, 2, 3, 3, 7, 7]));
        vm.load_source(
            "struct Marker { value: Int32 } fn original() -> (Marker,) { (Marker { value: 2 },) }",
        )
        .unwrap();
        assert_ne!(old_type, vm.function_signature("original").unwrap().ret);
        assert!(vm.call_int32_0("original").is_err());
        assert!(vm.call_async("original", &[]).is_err());
        let error = unsafe {
            vm.register_fn(
                "host_tuple",
                FunctionSignature::new(vec![], old_type),
                record as *const u8,
            )
        }
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::AbiError);
        scheduler.cancel(cancelled).unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(289));
        let ResumeError::Failed(error) = failing.resume().unwrap_err() else {
            panic!()
        };
        assert_eq!(error.code(), ErrorCode::RuntimeError);
        assert_eq!(error.span().unwrap().slice(source), Some("1 / value"));
        assert_eq!(failing.cps_result(), None);
        assert_eq!(failing.resume(), Err(ResumeError::AlreadyFinished));
        for _ in 0..4 {
            if !scoped.is_finished() {
                scoped.resume().unwrap();
            }
        }
        assert_eq!(scoped.cps_result(), Some(42));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS
            .with(|events| assert_eq!(*events.borrow(), [7, 7, -1, -1, 2, 2, 3, 3, 7, 7, 0, 3, 4]));
    }
}

#[test]
fn tuple_elements_and_destructured_initializers_resolve_imported_calls() {
    let source = r#"
        import joky/vm
        type Count = Int32
        fn run() -> Int32 {
            let h = vm.create()
            vm.load_source(h, "fn f() -> Int32 { 21 }")
            let (a, (b,)): (Count, (Count,)) = (vm.call_int32(h, "f"), (vm.call_int32(h, "f"),))
            vm.close(h)
            a + b
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        assert_eq!(vm.call_int32_0("run").unwrap(), 42);
    }
}

#[test]
fn wide_tuples_keep_parameter_order_and_mutable_leaves_across_backends() {
    let fields = (0..14).map(|_| "Int32").collect::<Vec<_>>().join(", ");
    let names = (0..14)
        .map(|n| format!("x{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let values = (0..14)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!("type Wide = (Unit, Bool, ({fields}), Unit)\nfn pass(value: Wide, delta: Int32) -> Wide {{ var (unit, flag, ({names}), tail) = value x13 = x13 + delta; (unit, flag, ({names}), tail) }}\nfn run() -> Int32 {{ let original = ({{}}, true, ({values}), {{}}) let (_, flag, ({names}), _) = pass(original, -20) let (_, _, (first, _, _, _, _, _, _, _, _, _, _, _, _, last), _) = original if flag {{ first + last + x0 + x7 + x13 }} else {{ 0 }} }}");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(&source, suspends, Suspends::Typed);
            let vm = parity_vm(&source, mode);
            let value = if suspends {
                vm.call_async("run", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("run").unwrap()
            };
            assert_eq!(value, 13, "{mode:?} suspends={suspends}");
        }
    }
}
