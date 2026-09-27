//! Interpreter boundary, resource and continuation regressions.
use super::{InterpreterLimits, VmEngine, VmMode};
use crate::{
    runtime::{
        cps::CpsFrame,
        scheduler::Scheduler,
        suspension::{ContinuationStatus, ResumeError},
    },
    ErrorCode, ErrorPhase, FunctionSignature, Type,
};
use std::cell::RefCell;

thread_local! { static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) }; }

extern "C" fn record(value: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}

unsafe extern "C" fn delayed_unit(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    let value = frame.slots[0] as i32;
    record(value * 10 + frame.state as i32);
    frame.state += 1;
    i32::from(frame.state < 3)
}

unsafe extern "C" fn fail(frame: *mut CpsFrame) -> i32 {
    record(90 + (*frame).state as i32);
    (*frame).state += 1;
    if (*frame).state < 3 {
        1
    } else {
        7
    }
}

fn engine(mode: VmMode) -> VmEngine {
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            record as *const u8,
        )
        .unwrap();
        vm.register_suspending_fn(
            "wait",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            delayed_unit,
        )
        .unwrap();
        vm.register_suspending_fn("fail", FunctionSignature::new(vec![], Type::Unit), fail)
            .unwrap();
    }
    vm
}

fn events() -> Vec<i32> {
    EVENTS.with(|events| events.borrow().clone())
}

#[test]
fn scope_accepts_native_and_user_branches_and_preserves_poll_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = engine(mode);
        vm.load_source(
            r#"
            extern fn record(x: Int32) -> Int32
            extern fn wait(x: Int32) @suspends
            fn wrapped(x: Int32) @suspends { wait(x) }
            fn f() @suspends -> Int32 {
                scope { | wait(record(1)) | wrapped(record(2)) | wait(record(3)) }
                record(7)
            }
        "#,
        )
        .unwrap();
        let mut task = vm.call_async("f", &[]).unwrap();
        assert_eq!(events(), [1, 2, 3, 10, 20, 30]);
        task.resume().unwrap();
        assert!(!task.is_finished());
        assert_eq!(events(), [1, 2, 3, 10, 20, 30, 11, 21, 31]);
        vm.load_source("fn f() -> Int32 { 99 }").unwrap();
        drop(vm);
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(7));
        assert_eq!(events(), [1, 2, 3, 10, 20, 30, 11, 21, 31, 12, 22, 32, 7]);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}

#[test]
fn scope_host_failure_is_terminal_and_skips_later_siblings() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = engine(mode);
        vm.load_source("extern fn wait(x: Int32) @suspends extern fn fail() @suspends fn f() @suspends { scope { | fail() | wait(2) } }").unwrap();
        let mut task = vm.call_async("f", &[]).unwrap();
        task.resume().unwrap();
        assert!(task.resume().is_err());
        assert!(matches!(task.status(), ContinuationStatus::Failed(_)));
        assert_eq!(task.cps_result(), None);
        assert_eq!(events(), [90, 20, 91, 21, 92]);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(events(), [90, 20, 91, 21, 92]);
    }
}

#[test]
fn unsupported_host_is_rejected_before_publication_or_side_effects() {
    extern "C" fn sum3(a: i32, b: i32, c: i32) -> i32 {
        a + b + c
    }
    let mut vm = engine(VmMode::Interpreter);
    unsafe {
        vm.register_fn(
            "sum3",
            FunctionSignature::new(vec![Type::Int32; 3], Type::Int32),
            sum3 as *const u8,
        )
        .unwrap();
    }
    vm.load_source("fn previous() -> Int32 { 42 }").unwrap();
    let version = vm.module_version();
    let source = "extern fn record(x: Int32) -> Int32 extern fn sum3(a: Int32,b: Int32,c: Int32) -> Int32 fn f() -> Int32 { record(1); sum3(1,2,3) }";
    let error = vm.load_source(source).unwrap_err();
    assert_eq!(error.phase(), ErrorPhase::Abi);
    assert_eq!(error.code(), ErrorCode::AbiError);
    assert!(error
        .message()
        .contains("unsupported interpreter host signature"));
    assert_eq!(error.span().unwrap().slice(source), Some("sum3(1,2,3)"));
    assert!(events().is_empty());
    assert_eq!(vm.module_version(), version);
    assert_eq!(vm.call_int32_0("previous").unwrap(), 42);
    // Merely declaring an unavailable adapter or importing unused built-ins is safe.
    vm.load_source("extern fn sum3(a: Int32,b: Int32,c: Int32) -> Int32 fn f() -> Int32 { 9 }")
        .unwrap();
    assert_eq!(vm.call_int32_0("f").unwrap(), 9);
}

#[test]
fn native_function_references_fail_during_loading() {
    let mut vm = engine(VmMode::Interpreter);
    let source = r#"
        import joky/vm
        fn value() -> Int32 { 42 }
        fn f() { let child = vm.create_interpreter(); vm.register_int32(child, "value", value); }
    "#;
    let error = vm.load_source(source).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AbiError);
    assert!(error.message().contains("Native function references"));
    assert_eq!(vm.module_version(), None);
}

#[test]
fn direct_host_adapters_erase_unit_and_support_two_argument_unit_results() {
    extern "C" fn sum(a: i32, b: i32) -> i32 {
        a + b
    }
    extern "C" fn report(a: i32, b: i32) {
        record(a);
        record(b);
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = engine(mode);
        unsafe {
            vm.register_fn(
                "sum",
                FunctionSignature::new(
                    vec![Type::Unit, Type::Int32, Type::Unit, Type::Int32],
                    Type::Int32,
                ),
                sum as *const u8,
            )
            .unwrap();
            vm.register_fn(
                "report",
                FunctionSignature::new(vec![Type::Int32, Type::Int32], Type::Unit),
                report as *const u8,
            )
            .unwrap();
        }
        vm.load_source("extern fn sum(u: Unit,a: Int32,v: Unit,b: Int32) -> Int32 extern fn report(a: Int32,b: Int32) fn f() -> Int32 { report(2,3); sum({}, 20, {}, 22) }").unwrap();
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
        assert_eq!(events(), [2, 3]);
    }
}

#[test]
fn builtin_two_argument_vm_call_has_an_interpreter_adapter() {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(
        r#"
        import joky/vm
        fn f() -> Int32 {
            let child = vm.create_interpreter()
            vm.load_source(child, "fn sum(a: Int32,b: Int32) -> Int32 { a+b }")
            let result = vm.call_int32(child, "sum", 20, 22)
            vm.close(child)
            result
        }
    "#,
    )
    .unwrap();
    assert_eq!(vm.call_int32_0("f").unwrap(), 42);
}

#[test]
fn scope_native_string_arguments_survive_pending_and_module_drop() {
    unsafe extern "C" fn check(frame: *mut CpsFrame) -> i32 {
        let frame = &mut *frame;
        let text = crate::runtime::string::borrow(frame.slots[1] as *const u8).unwrap();
        record(text.len() as i32);
        assert_eq!(frame.slots[0], 0);
        frame.state += 1;
        i32::from(frame.state < 3)
    }
    let mut vm = engine(VmMode::Interpreter);
    unsafe {
        vm.register_suspending_fn(
            "check",
            FunctionSignature::new(vec![Type::Unit, Type::String], Type::Unit),
            check,
        )
        .unwrap();
    }
    vm.load_source(r#"extern fn check(u: Unit, s: String) @suspends fn f() @suspends { scope { | check({}, "héllo") } }"#).unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    drop(vm);
    task.resume().unwrap();
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(0));
    assert_eq!(events(), [6, 6, 6]);
}

#[test]
fn deep_calls_return_on_a_small_native_stack() {
    std::thread::Builder::new().stack_size(256 * 1024).spawn(|| {
        for suspends in [false, true] {
            let mut vm = VmEngine::new_interpreter();
            vm.set_interpreter_limits(InterpreterLimits { max_frames: 6000, ..Default::default() }).unwrap();
            let annotation = if suspends { "@suspends" } else { "" };
            vm.load_source(&format!("fn f(n: Int32) {annotation} -> Int32 {{ var value=0; var k=n; while k>0 {{ value=f(k-1)+1; k=0; }} value }}")).unwrap();
            if suspends { assert_eq!(vm.call_async("f", &[5000]).unwrap().cps_result(), Some(5000)); }
            else { assert_eq!(vm.call_int32_1("f", 5000).unwrap(), 5000); }
        }
    }).unwrap().join().unwrap();
}

#[test]
fn deep_nested_scopes_resume_or_cancel_without_recursive_drop() {
    std::thread::Builder::new().stack_size(256 * 1024).spawn(|| {
        for cancel in [false, true] {
            let mut vm = engine(VmMode::Interpreter);
            vm.set_interpreter_limits(InterpreterLimits { max_frames: 6000, ..Default::default() }).unwrap();
            vm.load_source("extern fn wait(n: Int32) @suspends fn f(n: Int32) @suspends { var k=n; while k>0 { scope { | f(k-1) } k=0; } wait(n) }").unwrap();
            let mut task = vm.call_async("f", &[3000]).unwrap();
            assert!(!task.is_finished());
            drop(vm);
            if !cancel {
                for _ in 0..6100 {
                    if task.is_finished() { break; }
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(0));
            }
            drop(task);
        }
    }).unwrap().join().unwrap();
}

#[test]
fn frame_limits_cover_recursion_and_scope_width_before_starting_children() {
    let mut vm = engine(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 8,
        ..Default::default()
    })
    .unwrap();
    vm.load_source("fn f() -> Int32 { f() }").unwrap();
    let error = vm.call_int32_0("f").unwrap_err();
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert_eq!(error.runtime_trace().len(), 8);
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 2,
        ..Default::default()
    })
    .unwrap();
    vm.load_source(
        "extern fn wait(n: Int32) @suspends fn f() @suspends { scope { | wait(1) | wait(2) } }",
    )
    .unwrap();
    assert_eq!(
        vm.call_async("f", &[]).unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    assert!(events().is_empty());
}

#[test]
fn terminal_continue_loops_compile_and_obey_execution_budgets() {
    for attribute in ["", "@suspends"] {
        let source = format!("fn forever() {attribute} -> Int32 {{ while (continue) {{}} }}");
        engine(VmMode::Jit).load_source(&source).unwrap();
        let ast = crate::parser::parse(&source).unwrap();
        crate::aot::AotCompiler::new("terminal_loop")
            .unwrap()
            .compile_module(&ast)
            .unwrap();
        let mut vm = engine(VmMode::Interpreter);
        vm.set_interpreter_limits(InterpreterLimits {
            instruction_budget: Some(20),
            poll_budget: Some(3),
            ..Default::default()
        })
        .unwrap();
        vm.load_source(&source).unwrap();
        if attribute.is_empty() {
            assert_eq!(
                vm.call_int32_0("forever").unwrap_err().code(),
                ErrorCode::ResourceLimit
            );
        } else {
            let mut task = vm.call_async("forever", &[]).unwrap();
            for _ in 0..20 {
                if task.is_finished() {
                    break;
                }
                let _ = task.resume();
            }
            assert!(
                matches!(task.status(), ContinuationStatus::Failed(ResumeError::Failed(error)) if error.code() == ErrorCode::ResourceLimit)
            );
            assert_eq!(task.cps_result(), None);
        }
    }
}

#[test]
fn jump_paths_survive_single_instruction_timeslices() {
    for poll_budget in [1, 3, 17] {
        let mut vm = engine(VmMode::Interpreter);
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(poll_budget),
            ..Default::default()
        })
        .unwrap();
        vm.load_source(
            r#"
            extern fn record(x: Int32) -> Int32
            fn f() @suspends -> Int32 {
                var i = 0 var total = 0
                while (record(i); i < 10) {
                    i = i + 1
                    if i == 2 { continue }
                    if i == 4 { break }
                    total = total + record(i)
                }
                return record(total)
            }
        "#,
        )
        .unwrap();
        let mut task = vm.call_async("f", &[]).unwrap();
        for _ in 0..1000 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(4));
        assert_eq!(events(), [0, 1, 1, 2, 3, 3, 4]);
    }
}

#[test]
fn synchronous_budget_stops_infinite_loops_and_resets_per_invocation() {
    let mut vm = engine(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        instruction_budget: Some(20),
        ..Default::default()
    })
    .unwrap();
    vm.load_source("fn forever() { while true {} } fn value() -> Int32 { 42 }")
        .unwrap();
    assert_eq!(
        vm.call_unit_0("forever").unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    for _ in 0..25 {
        assert_eq!(vm.call_int32_0("value").unwrap(), 42);
    }
    vm.set_interpreter_limits(InterpreterLimits {
        instruction_budget: Some(0),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        vm.call_int32_0("value").unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
}

#[test]
fn poll_budget_yields_without_replaying_calls_assignments_or_scope_branches() {
    for poll_budget in [1, 3, 17] {
        let mut vm = engine(VmMode::Interpreter);
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(poll_budget),
            ..Default::default()
        })
        .unwrap();
        vm.load_source(
            r#"
            extern fn record(x: Int32) -> Int32
            fn count(n: Int32) -> Int32 { var i=0; while i<n { i=i+1; } i }
            fn branch(n: Int32) @suspends { record(count(n)); }
            fn f() @suspends -> Int32 {
                var i=0; while i<3 { record(i); i=i+1; }
                scope { | branch(7) | branch(9) }
                record(42)
            }
        "#,
        )
        .unwrap();
        let mut task = vm.call_async("f", &[]).unwrap();
        assert!(!task.is_finished());
        // Reconfiguring the VM must not change this task's snapshot.
        vm.set_interpreter_limits(InterpreterLimits {
            instruction_budget: Some(0),
            ..Default::default()
        })
        .unwrap();
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        for _ in 0..1000 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        assert_eq!(events(), [0, 1, 2, 7, 9, 42]);
    }
}

#[test]
fn total_budget_is_shared_across_resumes_and_host_polls() {
    unsafe extern "C" fn pending(_frame: *mut CpsFrame) -> i32 {
        record(1);
        1
    }
    let mut vm = engine(VmMode::Interpreter);
    unsafe {
        vm.register_suspending_fn(
            "pending",
            FunctionSignature::new(vec![], Type::Unit),
            pending,
        )
        .unwrap();
    }
    vm.set_interpreter_limits(InterpreterLimits {
        instruction_budget: Some(4),
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    vm.load_source("extern fn pending() @suspends fn f() @suspends { pending() }")
        .unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    for _ in 0..10 {
        if task.is_finished() {
            break;
        }
        let _ = task.resume();
    }
    assert!(
        matches!(task.status(), ContinuationStatus::Failed(ResumeError::Failed(error)) if error.code() == ErrorCode::ResourceLimit)
    );
    assert_eq!(events(), [1, 1, 1]);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
}

#[test]
fn budgeted_cpu_loop_can_be_cancelled() {
    let mut vm = engine(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(10),
        ..Default::default()
    })
    .unwrap();
    vm.load_source(
        "extern fn record(x: Int32) -> Int32 fn f() @suspends { while true { record(1); } }",
    )
    .unwrap();
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("f", &[]).unwrap());
    scheduler.resume(id).unwrap();
    assert!(!events().is_empty());
    scheduler.cancel(id).unwrap();
    let before = events();
    assert_eq!(scheduler.resume_all().unwrap(), 0);
    assert_eq!(events(), before);
}

#[test]
fn interpreter_limit_configuration_is_checked() {
    let mut vm = VmEngine::new();
    assert_eq!(
        vm.set_interpreter_limits(InterpreterLimits::default())
            .unwrap_err()
            .code(),
        ErrorCode::InvalidCall
    );
    let mut vm = VmEngine::new_interpreter();
    let original = vm.interpreter_limits();
    for limits in [
        InterpreterLimits {
            max_frames: 0,
            ..original
        },
        InterpreterLimits {
            poll_budget: Some(0),
            ..original
        },
    ] {
        assert_eq!(
            vm.set_interpreter_limits(limits).unwrap_err().code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(vm.interpreter_limits(), original);
    }
}

#[test]
fn runtime_trace_preserves_exact_spans_after_optimization_and_resume() {
    let source = r#"
        extern fn wait(n: Int32) @suspends
        fn inner(n: Int32) -> Int32 { var x=n; x=x/0; x }
        fn outer() @suspends -> Int32 { wait(1); inner(7) }
    "#;
    let mut vm = engine(VmMode::Interpreter);
    vm.load_source(source).unwrap();
    let mut task = vm.call_async("outer", &[]).unwrap();
    task.resume().unwrap();
    drop(vm);
    let ResumeError::Failed(error) = task.resume().unwrap_err() else {
        panic!("structured error required")
    };
    assert_eq!(error.code(), ErrorCode::RuntimeError);
    assert_eq!(error.message(), "Division by zero");
    assert_eq!(error.span().unwrap().slice(source), Some("x/0"));
    let trace = error.runtime_trace();
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0].function, "inner");
    assert_eq!(trace[1].function, "outer");
    assert_eq!(trace[1].span.slice(source), Some("inner(7)"));
    assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
}

#[test]
fn tiny_poll_slices_preserve_nested_scope_host_poll_order() {
    for slice in [None, Some(1), Some(2), Some(5)] {
        let mut vm = engine(VmMode::Interpreter);
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: slice,
            ..Default::default()
        })
        .unwrap();
        vm.load_source("extern fn wait(n: Int32) @suspends fn inner() @suspends { scope { | wait(1) | wait(2) } } fn f() @suspends { scope { | inner() | wait(3) } }").unwrap();
        let mut task = vm.call_async("f", &[]).unwrap();
        for _ in 0..100 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(0));
        assert_eq!(events(), [10, 20, 30, 11, 21, 31, 12, 22, 32]);
    }
}

#[test]
fn static_binding_keeps_old_host_after_replacement_and_reload() {
    unsafe extern "C" fn old(frame: *mut CpsFrame) -> i32 {
        let frame = &mut *frame;
        frame.state += 1;
        if frame.state < 3 {
            1
        } else {
            frame.result = 42;
            0
        }
    }
    unsafe extern "C" fn new(frame: *mut CpsFrame) -> i32 {
        (*frame).result = 99;
        0
    }
    let mut vm = engine(VmMode::Interpreter);
    let signature = FunctionSignature::new(vec![], Type::Int32);
    unsafe {
        vm.register_suspending_fn("host", signature.clone(), old)
            .unwrap();
    }
    let source = "extern fn host() @suspends -> Int32 fn f() @suspends -> Int32 { host() }";
    vm.load_source(source).unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    unsafe {
        vm.register_suspending_fn("host", signature, new).unwrap();
    }
    vm.load_source(source).unwrap();
    assert_eq!(vm.call_async("f", &[]).unwrap().cps_result(), Some(99));
    drop(vm);
    task.resume().unwrap();
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(42));
}

#[test]
fn frame_capacity_is_reused_after_completed_calls_and_scopes() {
    let mut vm = engine(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 2,
        ..Default::default()
    })
    .unwrap();
    vm.load_source("fn unit() @suspends {} fn value() -> Int32 { 1 } fn f() @suspends -> Int32 { var n=0; while n<2000 { scope { | unit() } n=n+value(); } n }").unwrap();
    assert_eq!(vm.call_async("f", &[]).unwrap().cps_result(), Some(2000));
}

#[test]
fn budget_exhaustion_is_shared_by_nested_calls_and_scope_branches() {
    let mut vm = engine(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        instruction_budget: Some(30),
        poll_budget: Some(3),
        ..Default::default()
    })
    .unwrap();
    vm.load_source("extern fn record(n: Int32) -> Int32 fn work() { var n=0; while n<100 { n=n+1; } } fn branch() @suspends { work() } fn f() @suspends { scope { | branch() | branch() } record(9); }").unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    for _ in 0..20 {
        if task.is_finished() {
            break;
        }
        let _ = task.resume();
    }
    assert!(
        matches!(task.status(), ContinuationStatus::Failed(ResumeError::Failed(error)) if error.code()==ErrorCode::ResourceLimit)
    );
    assert!(events().is_empty());
}

#[test]
fn record_calls_obey_cooperative_budgets_and_frame_limits() {
    let source = r#"
        struct Pair { x: Int32 y: Int32 }
        fn descend(p: Pair, n: Int32) @suspends -> Pair {
            if n == 0 { return p }
            descend(Pair { x: p.x + 1 y: p.y + 2 }, n - 1)
        }
        fn run(n: Int32) @suspends -> Int32 {
            let p = descend(Pair { x: 1 y: 2 }, n)
            p.x + p.y
        }
    "#;
    let mut vm = engine(VmMode::Interpreter);
    vm.load_source(source).unwrap();
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 64,
        instruction_budget: Some(5000),
        poll_budget: Some(1),
    })
    .unwrap();
    let mut task = vm.call_async("run", &[20]).unwrap();
    let mut polls = 0;
    while !task.is_finished() {
        task.resume().unwrap();
        polls += 1;
        assert!(polls < 5000);
    }
    assert!(polls > 20);
    assert_eq!(task.cps_result(), Some(63));
    for limits in [
        InterpreterLimits {
            max_frames: 4,
            instruction_budget: None,
            poll_budget: Some(1),
        },
        InterpreterLimits {
            max_frames: 64,
            instruction_budget: Some(20),
            poll_budget: Some(1),
        },
    ] {
        vm.set_interpreter_limits(limits).unwrap();
        let mut task = vm.call_async("run", &[20]).unwrap();
        loop {
            if let Err(ResumeError::Failed(error)) = task.resume() {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                break;
            }
            assert!(!task.is_finished());
        }
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}

#[test]
fn enum_calls_obey_cooperative_budgets_and_frame_limits() {
    let source = r#"
        enum E { Empty Value(Int32) }
        fn descend(p: E, n: Int32) @suspends -> E {
            if n == 0 { return p }
            match p { | Value(x) => descend(E.Value(x + 3), n - 1) | Empty => E.Empty }
        }
        fn run(n: Int32) @suspends -> Int32 {
            match descend(E.Value(3), n) { | Value(x) => x | Empty => 0 }
        }
    "#;
    let mut vm = engine(VmMode::Interpreter);
    vm.load_source(source).unwrap();
    vm.set_interpreter_limits(InterpreterLimits {
        max_frames: 64,
        instruction_budget: Some(5000),
        poll_budget: Some(1),
    })
    .unwrap();
    let mut task = vm.call_async("run", &[20]).unwrap();
    let mut polls = 0;
    while !task.is_finished() {
        task.resume().unwrap();
        polls += 1;
        assert!(polls < 5000);
    }
    assert!(polls > 20);
    assert_eq!(task.cps_result(), Some(63));
    for limits in [
        InterpreterLimits {
            max_frames: 4,
            instruction_budget: None,
            poll_budget: Some(1),
        },
        InterpreterLimits {
            max_frames: 64,
            instruction_budget: Some(20),
            poll_budget: Some(1),
        },
    ] {
        vm.set_interpreter_limits(limits).unwrap();
        let mut task = vm.call_async("run", &[20]).unwrap();
        loop {
            if let Err(ResumeError::Failed(error)) = task.resume() {
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                break;
            }
            assert!(!task.is_finished());
        }
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}
