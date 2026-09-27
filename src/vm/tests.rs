use super::{VmEngine, VmMode};
use crate::runtime::{
    cps::CpsFrame,
    scheduler::{Scheduler, SchedulerError},
    suspension::ResumeError,
};
use crate::{ErrorCode, ErrorPhase, FunctionSignature, Span, Type};
use std::cell::{Cell, RefCell};

thread_local! {
    static READY: Cell<bool> = const { Cell::new(false) };
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
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
    assert!(error
        .message()
        .contains("Unknown opaque type `Host.Context`"));
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
        vm.load_source("fn value(x: Int32) -> Int32 { x } fn flag(x: Bool) -> Int32 { 42 } fn unit(x: Unit) -> Int32 { 42 } fn text() -> String { \"value\" }").unwrap();
        let mut signature = vm.function_signature("value").unwrap();
        signature.params.clear();
        signature.ret = Type::Unit;
        assert_eq!(vm.call_int32_1("value", 42).unwrap(), 42);
        assert_eq!(
            vm.function_signature("value").unwrap(),
            FunctionSignature::new(vec![Type::Int32], Type::Int32)
        );
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
        );
    }
    vm.load_source(
        "extern fn host(value: Host.Context) -> Int32 fn f(value: Host.Context) -> Int32 { host(value) }",
    )
    .unwrap();
}

unsafe extern "C" fn delayed_value(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        EVENTS.with(|events| events.borrow_mut().push(frame.slots[0] as i32));
        frame.host_data = (frame.slots[0] as i32 + 1).to_ne_bytes().to_vec();
        frame.state = 1;
        return 1;
    }
    if !READY.with(Cell::get) {
        return 1;
    }
    frame.result = i32::from_ne_bytes(frame.host_data[..].try_into().unwrap()) as u32 as u64;
    0
}

unsafe extern "C" fn fail_now(_frame: *mut CpsFrame) -> i32 {
    7
}

extern "C" fn record(value: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}

fn parity_vm(source: &str, mode: VmMode) -> VmEngine {
    READY.with(|ready| ready.set(false));
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_suspending_fn(
            "delayed_value",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            delayed_value,
        );
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            record as *const u8,
        );
        vm.register_suspending_fn(
            "fail_now",
            FunctionSignature::new(vec![], Type::Unit),
            fail_now,
        );
    }
    vm.load_source(source).unwrap();
    vm
}

fn vm(source: &str) -> VmEngine {
    parity_vm(source, VmMode::Jit)
}

#[test]
fn mutable_bindings_and_loops_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/loops.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            // Only declarations get the attribute, not calls or parentheses.
            let source = if suspends {
                source
                    .lines()
                    .map(|line| {
                        if line.starts_with("fn ") {
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
            let source = source
                .lines()
                .map(|line| {
                    if suspends && line.starts_with("fn ") {
                        line.replacen(')', ") @suspends", 1)
                    } else {
                        line.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
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
fn builtin_calls_inside_sequences_resolve_and_execute() {
    let source = r#"
        import joky/vm;
        fn compute() -> Int32 {
            let engine = vm.create_interpreter();
            let result = (vm.load_source(engine, "fn value() -> Int32 { 42 }");
                          vm.call_int32(engine, "value"));
            vm.close(engine);
            result
        };
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(parity_vm(source, mode).call_int32_0("compute").unwrap(), 42);
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
            );
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
        );
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
        );
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
        );
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
        );
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
        );
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
        );
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
        vm.register_suspending_fn("bad", FunctionSignature::new(vec![], Type::Unit), bad_host);
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
        );
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
