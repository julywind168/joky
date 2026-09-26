use super::{VmEngine, VmMode};
use crate::runtime::{
    cps::CpsFrame,
    scheduler::{Scheduler, SchedulerError},
    suspension::ResumeError,
};
use crate::{ErrorPhase, FunctionSignature, Type};
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
    assert_eq!(error.phase(), ErrorPhase::Compile);
    assert!(error.contains("Unknown opaque type `Host.Context`"));
}

#[test]
fn parse_errors_keep_their_source_line() {
    let mut vm = VmEngine::new();
    let error = vm.load_source("fn f() {\n#\n}").unwrap_err();
    assert_eq!(error.phase(), ErrorPhase::Parse);
    assert_eq!(error.line(), Some(2));
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
    assert_eq!(
        statuses,
        [
            "CPS start failed: InvalidCpsStatus(7)".to_string(),
            "CPS start failed: InvalidCpsStatus(7)".to_string(),
        ]
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
            vm.load_source(source).unwrap_err().contains(message),
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
    assert!(format!("{error:?}").contains("InvalidCpsStatus(7)"));
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
    let mut vm = vm("fn f() -> Int32 { 42 }");
    for declaration in [
        "extern fn record() -> Int32",
        "extern fn record(x: Bool) -> Int32",
        "extern fn record(x: Int32) -> Bool",
        "extern fn record(x: Int32)",
        "extern fn delayed_value(x: Bool) @suspends -> Int32",
        "extern fn delayed_value(x: Int32) @suspends -> Bool",
    ] {
        assert!(vm
            .load_source(declaration)
            .unwrap_err()
            .contains("incompatible signature"));
        assert_eq!(vm.call_int32_0("f").unwrap(), 42);
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
        .contains("ABI"));
    assert!(vm
        .load_source(
            r#"
        extern fn host(x: Int32) @suspends -> Int32
        fn f() -> Int32 { host(1) }
    "#
        )
        .unwrap_err()
        .contains("@suspends"));
    assert!(vm
        .load_source("extern fn missing()")
        .unwrap_err()
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
