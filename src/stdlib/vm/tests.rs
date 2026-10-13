use super::*;

fn text(value: &str) -> crate::runtime::string::JokyText {
    crate::runtime::string::JokyText::new(value)
}

#[test]
fn handles_close_once_and_are_not_reused_or_shared_between_threads() {
    let handle = create();
    assert_ne!(handle, 0);
    assert_eq!(std::thread::spawn(move || close(handle)).join().unwrap(), 1);
    assert_eq!(close(handle), 0);
    assert_eq!(close(handle), 1);
    assert_eq!(error_code(), 1);
    let next = create();
    assert_ne!(handle, next);
    assert_eq!(error_code(), 0);
    assert_eq!(close(next), 0);
}

#[test]
fn interpreter_handle_runs_source_without_jit() {
    let handle = create_interpreter();
    unsafe {
        assert_eq!(
            load_source(
                handle,
                text("fn add(x: Int32, y: Int32) -> Int32 { x + y }").as_ptr()
            ),
            0
        );
        assert_eq!(call_int32_2(handle, text("add").as_ptr(), 20, 22), 42);
        assert_eq!(error_code(), 0);
    }
    assert_eq!(close(handle), 0);
}

#[test]
fn registered_jit_function_outlives_its_vm_in_both_target_modes() {
    for constructor in ["vm.create", "vm.create_interpreter"] {
        let mut host = VmEngine::new();
        host.load_source(&format!(
            r#"
            import joky/vm
            fn add(a: Int32, b: Int32) -> Int32 {{ a + b }}
            fn prepare() {{
                let child = {constructor}()
                {{ let vm_text = "add" vm.register_int32(child, &vm_text, add) }}
                {{ let vm_text = "extern fn add(a: Int32, b: Int32) -> Int32 fn answer() -> Int32 {{{{ add(20, 22) }}}}" vm.load_source(child, &vm_text) }};
            }}
        "#
        ))
        .unwrap();
        host.call_unit_0("prepare").unwrap();
        assert_eq!(error_code(), 0);
        let child = VMS.with(|vms| {
            let vms = vms.borrow();
            assert_eq!(vms.len(), 1);
            *vms.keys().next().unwrap()
        });
        host.load_source("fn replacement() {}").unwrap();
        drop(host);
        assert_eq!(unsafe { call_int32_0(child, text("answer").as_ptr()) }, 42);
        assert_eq!(error_code(), 0);
        assert_eq!(close(child), 0);
        VMS.with(|vms| assert!(vms.borrow().is_empty()));
    }
}

#[test]
fn reload_failure_preserves_old_code_and_errors_are_observable() {
    let handle = create();
    unsafe {
        assert_eq!(
            load_source(handle, text("fn f(x: Int32) -> Int32 { x + 1 }").as_ptr()),
            0
        );
        assert_eq!(call_int32_1(handle, text("f").as_ptr(), 41), 42);
        assert_eq!(error_code(), 0);
        assert_eq!(
            reload_source(handle, text("fn f() -> Bool { 1 }").as_ptr()),
            1
        );
        assert_eq!(error_code(), 1);
        LAST_ERROR.with(|e| {
            assert!(e
                .borrow()
                .as_ref()
                .unwrap()
                .contains("Return type mismatch"))
        });
        assert_eq!(call_int32_1(handle, text("f").as_ptr(), 2), 3);
        assert_eq!(error_code(), 0);
        assert_eq!(call_int32_0(handle, text("f").as_ptr()), 0);
        assert_eq!(error_code(), 1);
        assert_eq!(
            reload_source(handle, text("fn f(x: Int32) -> Int32 { x * 10 }").as_ptr()),
            0
        );
        assert_eq!(call_int32_1(handle, text("f").as_ptr(), 2), 20);
        assert_eq!(call_int32_1(handle, text("f\0suffix").as_ptr(), 2), 0);
        assert_eq!(error_code(), 1);
        assert_eq!(load_source(handle, std::ptr::null()), 1);
        let invalid_bytes = [255];
        let invalid_utf8 = crate::runtime::string::JokyText {
            data: invalid_bytes.as_ptr(),
            byte_length: 1,
        };
        assert_eq!(load_source(handle, invalid_utf8.as_ptr()), 1);
    }
    assert_eq!(close(handle), 0);
}

#[test]
fn nested_vm_calls_release_the_registry_borrow_before_execution() {
    let handle = create();
    unsafe {
        assert_eq!(
            load_source(
                handle,
                text(
                    r#"
            import joky/vm
            fn f() -> Int32 { let child = vm.create() vm.close(child) }
        "#
                )
                .as_ptr()
            ),
            0
        );
        assert_eq!(call_int32_0(handle, text("f").as_ptr()), 0);
        assert_eq!(error_code(), 0);
    }
    assert_eq!(close(handle), 0);
    VMS.with(|vms| assert!(vms.borrow().is_empty()));
}

#[test]
fn handled_nested_vm_failure_does_not_escape_as_the_outer_error() {
    for constructor in ["vm.create", "vm.create_interpreter"] {
        let handle = create();
        unsafe {
            assert_eq!(
                load_source(
                    handle,
                    text(&format!(
                        r#"
            import joky/vm
            fn handled() -> Int32 {{
                let child = {constructor}()
                vm.close(child)
                vm.close(child)
                vm.error_code() - 1
            }}
        "#
                    ))
                    .as_ptr(),
                ),
                0
            );
            assert_eq!(call_int32_0(handle, text("handled").as_ptr()), 0);
        }
        assert_eq!(error_code(), 0);
        unsafe {
            assert_eq!(call_int32_0(handle, text("handled").as_ptr()), 0);
        }
        assert_eq!(error_code(), 0);
        assert_eq!(close(handle), 0);
    }
}

#[test]
fn handled_nested_division_does_not_poison_outer_or_later_calls() {
    for constructor in [create, create_interpreter] {
        for nested in ["vm.create", "vm.create_interpreter"] {
            let handle = constructor();
            let source = format!(
                r#"
                import joky/vm
                fn handled() -> Int32 {{
                    let child = {nested}()
                    {{ let vm_text = "fn bad() -> Int32 {{{{ 7 / 0 }}}}" vm.load_source(child, &vm_text) }}
                    {{ let vm_text = "bad" vm.call_int32(child, &vm_text) }}
                    let failed = vm.error_code()
                    vm.close(child)
                    failed + 41
                }}
                fn bad() -> Int32 {{ 8 / 0 }}
            "#
            );
            unsafe {
                assert_eq!(load_source(handle, text(&source).as_ptr()), 0);
                assert_eq!(call_int32_0(handle, text("bad").as_ptr()), 0);
                assert_eq!(error_code(), 1);
                LAST_ERROR
                    .with(|e| assert!(e.borrow().as_ref().unwrap().contains("Division by zero")));
                assert_eq!(call_int32_0(handle, text("handled").as_ptr()), 42);
                assert_eq!(error_code(), 0);
                assert_eq!(call_int32_0(handle, text("handled").as_ptr()), 42);
            }
            assert_eq!(crate::runtime::error::joky_runtime_error_code(), 0);
            assert_eq!(close(handle), 0);
        }
    }
}

#[test]
fn busy_vm_reports_an_error_without_panicking() {
    let handle = create();
    let engine = lookup(handle).unwrap();
    let active = engine.engine.borrow_mut();
    unsafe {
        assert_eq!(
            load_source(handle, text("fn f() -> Int32 { 1 }").as_ptr()),
            1
        );
        assert_eq!(call_int32_0(handle, text("f").as_ptr()), 0);
    }
    assert_eq!(error_code(), 1);
    LAST_ERROR.with(|e| assert!(e.borrow().as_ref().unwrap().contains("another operation")));
    drop(active);
    assert_eq!(close(handle), 0);
}

#[test]
fn script_scalar_bridge_runs_reload_and_cancel_on_shared_executor() {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for host_mode in [VmMode::Jit, VmMode::Interpreter] {
        for script in ["vm.create_interpreter()", "vm.create()"] {
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut host = VmEngine::with_mode(host_mode);
            host.load_source(
                &include_str!("../../../tests/fixtures/aot_script_scalar.jk")
                    .replace("vm.create_interpreter()", script),
            )
            .unwrap();
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            let id = executor
                .spawn(&group, host.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut done = false;
            for _ in 0..1000 {
                for result in executor.poll(10).unwrap().completed {
                    if result.id == id {
                        result.result.unwrap();
                        done = true;
                    }
                }
                if done {
                    break;
                }
            }
            assert!(done);
            assert_eq!(executor.task_count(), 0);
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            VMS.with(|vms| assert!(vms.borrow().is_empty()));
            drop(executor);
            drop(host);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}

fn script_call_source(script: &str, mode: VmMode, function: &str) -> String {
    let literal = script
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
        .replace('{', "{{")
        .replace('}', "}}");
    let create = if mode == VmMode::Jit {
        "create"
    } else {
        "create_interpreter"
    };
    format!(
        r#"import joky/vm
fn main() @suspends -> Int32 {{
    let h = vm.{create}()
    let code = "{literal}"
    vm.load_source(h, &code)
    vm.call_async_int32(h, "{function}", 0)
}}"#
    )
}

fn script_bridge_failure_close_and_parent_cancel(flat: bool) {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    let script = r#"import joky/task
import joky/domain
struct State { n: Int32 }
fn make(n: Int32) -> State { State { n: n } }
fn pending(n: Int32) @suspends -> Int32 {
    let owner = domain.create(make, n)
    task.sleep_ms(60000)
    42
}
fn fail(n: Int32) @suspends -> Int32 { task.yield_now() 1 / n }
fn wrong(n: Int32) @suspends -> Bool { true }
"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for action in ["close", "cancel", "fail", "wrong", "missing"] {
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut host = VmEngine::with_mode(mode);
            let entry = if matches!(action, "close" | "cancel") {
                "pending"
            } else {
                action
            };
            let mut source = script_call_source(script, mode, entry);
            if flat {
                source = source
                    .replace("import joky/vm", "import joky/vm import joky/script")
                    .replace("vm.call_async_int32", "script.call::<Int32, Int32>");
            }
            host.load_source(&source).unwrap();
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            let id = executor
                .spawn(&group, host.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut failure = None;
            for _ in 0..10 {
                for done in executor.poll(10).unwrap().completed {
                    if done.id == id {
                        failure = Some(done.result.unwrap_err());
                    }
                }
            }
            let h = VMS.with(|vms| {
                assert_eq!(vms.borrow().len(), 1);
                *vms.borrow().keys().next().unwrap()
            });
            if action == "close" {
                assert!(failure.is_none());
                assert_eq!(close(h), 0);
                for _ in 0..10 {
                    for done in executor.poll(10).unwrap().completed {
                        if done.id == id {
                            failure = Some(done.result.unwrap_err());
                        }
                    }
                }
                assert!(failure.unwrap().to_string().contains("closed during call"));
            } else if action == "cancel" {
                assert!(failure.is_none());
                executor.cancel_group(&group);
                assert_eq!(close(h), 0);
            } else {
                let error = failure.unwrap();
                assert_eq!(
                    error.code(),
                    match action {
                        "missing" => ErrorCode::NotFound,
                        "wrong" => ErrorCode::InvalidCall,
                        _ => ErrorCode::RuntimeError,
                    }
                );
                assert_eq!(close(h), 0);
            }
            assert_eq!(executor.task_count(), 0);
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            drop(executor);
            drop(host);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}

#[test]
fn script_scalar_bridge_failure_close_and_parent_cancel_release_children() {
    script_bridge_failure_close_and_parent_cancel(false);
}

#[test]
fn script_flat_bridge_failure_close_and_parent_cancel_release_children() {
    script_bridge_failure_close_and_parent_cancel(true);
}

fn script_bridge_cancellation(
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
    flat: bool,
) {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    let script = "import joky/domain import joky/task struct S { n: Int32 } fn make(n:Int32)->S { S{n:n} } fn read(s:&S)->Int32{s.n} fn work(n:Int32) @suspends ->Int32 { let s=domain.create(make,42) task.yield_now() domain.read_from(s.read_handle(),read) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut host = VmEngine::with_mode(mode);
        let source = if flat {
            let script = script
                .replace(
                    "fn work(n:Int32) @suspends ->Int32",
                    "struct Message { n:Int32 } fn work(n:Message) @suspends ->Message",
                )
                .replace(
                    "task.yield_now() domain.read_from(s.read_handle(),read)",
                    "task.yield_now() Message { n:domain.read_from(s.read_handle(),read) }",
                );
            script_call_source(&script, mode, "work")
                .replace("import joky/vm", "import joky/vm import joky/script struct Message { n:Int32 }")
                .replace("vm.call_async_int32(h, \"work\", 0)", "let reply = script.call::<Message, Message>(h, \"work\", Message { n:0 }) reply.n")
        } else {
            script_call_source(script, mode, "work")
        };
        host.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        if mode == VmMode::Interpreter {
            host.set_interpreter_limits(crate::vm::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut completed = false;
        for boundary in boundaries.clone() {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            let id = executor
                .spawn(&group, host.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut done = false;
            for _ in 0..boundary {
                for result in executor.poll(1).unwrap().completed {
                    if result.id == id {
                        assert_eq!(result.result.unwrap(), 42);
                        done = true;
                    }
                }
                if done {
                    break;
                }
            }
            executor.cancel_group(&group);
            // Existing vm.Handle is manually closed; cancellation owns only the call.
            let handles: Vec<_> = VMS.with(|vms| vms.borrow().keys().copied().collect());
            for h in handles {
                assert_eq!(close(h), 0);
            }
            assert_eq!(executor.task_count(), 0, "{mode:?} boundary {boundary}");
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            drop(executor);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
            if done {
                completed = true;
                if exhaustive {
                    break;
                }
            }
        }
        assert!(completed);
    }
}

#[test]
fn script_scalar_bridge_cancellation_reclaims_frames_and_domain_requests() {
    script_bridge_cancellation([0, 1, 2, 5, 10, 20, 50, 100, 200, 400, 800], false, false);
}

#[test]
#[ignore = "exhaustive: cancel scalar Script bridge at every host instruction"]
fn exhaustive_script_scalar_bridge_cancellation() {
    script_bridge_cancellation(0..=2048, true, false);
}

#[test]
fn script_scalar_bridge_uses_host_clock_without_polling_early() {
    use crate::runtime::executor::{Clock, HostExecutor, TaskGroup};
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };
    struct TestClock(Cell<Instant>);
    impl Clock for TestClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
    }
    for host_mode in [VmMode::Jit, VmMode::Interpreter] {
        for script_mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut host = VmEngine::with_mode(host_mode);
            host.load_source(&script_call_source(
                "import joky/task fn work(n:Int32) @suspends ->Int32 { task.sleep_ms(50) 42 }",
                script_mode,
                "work",
            ))
            .unwrap();
            let clock = Rc::new(TestClock(Cell::new(Instant::now())));
            let executor = HostExecutor::new(clock.clone(), 100);
            let group = TaskGroup::new();
            executor
                .spawn(&group, host.prepare_async("main", &[]).unwrap())
                .unwrap();
            assert!(executor.poll(10).unwrap().completed.is_empty());
            clock.0.set(clock.0.get() + Duration::from_millis(49));
            assert!(executor.poll(10).unwrap().completed.is_empty());
            clock.0.set(clock.0.get() + Duration::from_millis(1));
            let report = executor.poll(10).unwrap();
            assert_eq!(report.completed.len(), 1);
            assert_eq!(*report.completed[0].result.as_ref().unwrap(), 42);
            let handles: Vec<_> = VMS.with(|vms| vms.borrow().keys().copied().collect());
            for h in handles {
                assert_eq!(close(h), 0);
            }
            assert_eq!(executor.task_count(), 0);
        }
    }
}

#[test]
fn script_flat_bridge_cancellation_reclaims_packets() {
    script_bridge_cancellation(
        [0, 1, 2, 5, 10, 20, 50, 100, 200, 400, 800, 1600],
        false,
        true,
    );
}
#[test]
#[ignore = "exhaustive: cancel typed Script packets at every host instruction"]
fn exhaustive_script_flat_bridge_cancellation() {
    script_bridge_cancellation(0..=4096, true, true);
}

#[test]
fn script_flat_bridge_checks_contract_before_running_target() {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for target in [
            "fn work(n:UInt32) @suspends ->Int32 { 1/0 }",
            "fn work(n:Int32) @suspends ->UInt32 { let fail=1/0 0 }",
            "fn work(n:Int32) ->Int32 { 1/0 }",
            "fn work(n:Int32, other:Int32) @suspends ->Int32 { 1/0 }",
            "struct Message { other:Int32 } fn work(n:Message) @suspends ->Message { let fail=1/0 n }",
        ] {
            let mut source = script_call_source(target, mode, "work").replace("import joky/vm", "import joky/vm import joky/script struct Message { n:Int32 }");
            source = source.replace("vm.call_async_int32(h, \"work\", 0)", if target.contains("struct Message") {
                "let reply = script.call::<Message, Message>(h, \"work\", Message { n:0 }) reply.n"
            } else { "script.call::<Int32, Int32>(h, \"work\", 0)" });
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut host = VmEngine::with_mode(mode);
            host.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            let id = executor.spawn(&group, host.prepare_async("main", &[]).unwrap()).unwrap();
            let mut failed = false;
            for _ in 0..100 {
                for done in executor.poll(10).unwrap().completed {
                    if done.id == id {
                        assert_eq!(done.result.unwrap_err().code(), ErrorCode::InvalidCall);
                        failed = true;
                    }
                }
                if failed { break; }
            }
            assert!(failed);
            let handles: Vec<_> = VMS.with(|vms| vms.borrow().keys().copied().collect());
            for h in handles { assert_eq!(close(h), 0); }
            assert_eq!(executor.task_count(), 0);
            assert_eq!(executor.domains().borrow().usage(), (0,0,0,0));
            drop(executor); drop(host);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}
