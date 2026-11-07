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
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for constructor in ["vm.Vm.new", "vm.Vm.interpreter"] {
        let mut host = VmEngine::new();
        host.load_source(&format!(
            r#"
            import joky/vm
            import joky/task
            fn add(a: Int32, b: Int32) -> Int32 {{ a + b }}
            fn prepare() @suspends {{
                let child = {constructor}()
                {{ let vm_text = "add"; vm.register_int32_2(child, &vm_text, add) }}
                {{ let vm_text = "extern fn add(a: Int32, b: Int32) -> Int32 fn answer() -> Int32 {{{{ add(20, 22) }}}}"; vm.load_source(child, &vm_text) }}
                task.sleep(60s)
            }}
        "#
        )).unwrap();
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, host.prepare_async("prepare", &[]).unwrap())
            .unwrap();
        assert!(executor.poll(10).unwrap().completed.is_empty());
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
        executor.cancel_group(&group);
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
            fn f() -> Int32 { let child = vm.Vm.new(); vm.close(child) }
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
    for constructor in ["vm.Vm.new", "vm.Vm.interpreter"] {
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
                let invalid = "fn broken("
                vm.load_source(child, &invalid)
                let status = vm.error_code()
                vm.close(child)
                status - 1
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
        for nested in ["vm.Vm.new", "vm.Vm.interpreter"] {
            let handle = constructor();
            let source = format!(
                r#"
                import joky/vm
                fn handled() -> Int32 {{
                    let child = {nested}()
                    {{ let vm_text = "fn bad() -> Int32 {{{{ 7 / 0 }}}}"; vm.load_source(child, &vm_text) }}
                    {{ let vm_text = "bad"; vm.call_int32_0(child, &vm_text) }}
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
        for script in ["vm.Vm.interpreter()", "vm.Vm.new()"] {
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut host = VmEngine::with_mode(host_mode);
            host.load_source(
                &include_str!("../../../tests/fixtures/aot_script_scalar.jk")
                    .replace("vm.Vm.interpreter()", script),
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
        "new"
    } else {
        "interpreter"
    };
    format!(
        r#"import joky/vm
fn main() @suspends -> Int32 {{
    let h = vm.Vm.{create}()
    let code = "{literal}"
    vm.load_source(h, &code)
    h.call::<Int32, Int32>("{function}", 0)
}}"#
    )
}

#[test]
fn vm_call_failure_close_and_parent_cancel_release_children() {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    let script = r#"import joky/task
import joky/domain
struct State { n: Int32 }
fn make(n: Int32) -> State { State { n: n } }
fn pending(n: Int32) @suspends -> Int32 {
    let owner = domain.create(make, n)
    task.sleep(60s)
    42
}
fn fail(n: Int32) @suspends -> Int32 { task.yield_now(); 1 / n }
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
            let source = script_call_source(script, mode, entry);
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
            if action == "close" {
                let h = VMS.with(|vms| {
                    assert_eq!(vms.borrow().len(), 1);
                    *vms.borrow().keys().next().unwrap()
                });
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
            }
            VMS.with(|vms| assert!(vms.borrow().is_empty()));
            assert_eq!(executor.task_count(), 0);
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            drop(executor);
            drop(host);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}

fn script_bridge_cancellation(
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
    flat: bool,
    checked: bool,
) {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    let script = "import joky/domain import joky/task struct S { n: Int32 } fn make(n:Int32)->S { S{n:n} } fn read(s:&S)->Int32{s.n} fn work(n:Int32) @suspends ->Int32 { let s=domain.create(make,42); task.yield_now(); domain.read_from(s.read_handle(),read).unwrap() }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut host = VmEngine::with_mode(mode);
        let source = if flat {
            let script = script
                .replace(
                    "fn work(n:Int32) @suspends ->Int32",
                    "struct Message { n:Int32 } fn work(n:Message) @suspends ->Message",
                )
                .replace(
                    "task.yield_now(); domain.read_from(s.read_handle(),read).unwrap()",
                    "task.yield_now(); Message { n:domain.read_from(s.read_handle(),read).unwrap() }",
                );
            script_call_source(&script, mode, "work")
                .replace(
                    "import joky/vm",
                    "import joky/vm struct Message { n:Int32 }",
                )
                .replace(
                    "h.call::<Int32, Int32>(\"work\", 0)",
                    "let reply = h.call::<Message, Message>(\"work\", Message { n:0 }); reply.n",
                )
        } else {
            script_call_source(script, mode, "work")
        };
        let source = if checked {
            source
                .replace(
                    "h.call::<Int32, Int32>(\"work\", 0)",
                    "match h.try_call::<Int32, Int32>(\"work\", 0) { Err(Failed) => 42, _ => 0 }",
                )
                .replace(
                    "domain.read_from(s.read_handle(),read).unwrap()",
                    "domain.read_from(s.read_handle(),read).unwrap() / n",
                )
        } else {
            source
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
            VMS.with(|vms| assert!(vms.borrow().is_empty()));
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
    script_bridge_cancellation(
        [0, 1, 2, 5, 10, 20, 50, 100, 200, 400, 800],
        false,
        false,
        false,
    );
}

#[test]
#[ignore = "exhaustive: cancel scalar Script bridge at every host instruction"]
fn exhaustive_script_scalar_bridge_cancellation() {
    script_bridge_cancellation(0..=2048, true, false, false);
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
                "import joky/task fn work(n:Int32) @suspends ->Int32 { task.sleep(50ms); 42 }",
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
            VMS.with(|vms| assert!(vms.borrow().is_empty()));
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
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel typed Script packets at every host instruction"]
fn exhaustive_script_flat_bridge_cancellation() {
    script_bridge_cancellation(0..=4096, true, true, false);
}

#[test]
fn script_flat_bridge_checks_contract_before_running_target() {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for target in [
            "fn work(n:UInt32) @suspends ->Int32 { 1/0 }",
            "fn work(n:Int32) @suspends ->UInt32 { let fail=1/0; 0 }",
            "fn work(n:Int32) ->Int32 { 1/0 }",
            "fn work(n:Int32, other:Int32) @suspends ->Int32 { 1/0 }",
            "struct Message { other:Int32 } fn work(n:Message) @suspends ->Message { let fail=1/0; n }",
        ] {
            let mut source = script_call_source(target, mode, "work").replace("import joky/vm", "import joky/vm struct Message { n:Int32 }");
            source = source.replace("h.call::<Int32, Int32>(\"work\", 0)", if target.contains("struct Message") {
                "let reply = h.call::<Message, Message>(\"work\", Message { n:0 }); reply.n"
            } else { "h.call::<Int32, Int32>(\"work\", 0)" });
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
            VMS.with(|vms| assert!(vms.borrow().is_empty()));
            assert_eq!(executor.task_count(), 0);
            assert_eq!(executor.domains().borrow().usage(), (0,0,0,0));
            drop(executor); drop(host);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}

fn migration_cancellation(
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
    owned: bool,
    incremental: bool,
) {
    let old = (if incremental {
        include_str!("../../../tests/fixtures/migration_incremental_old.jk")
    } else if owned {
        include_str!("../../../tests/fixtures/migration_owned_old.jk")
    } else {
        include_str!("../../../tests/fixtures/migration_old.jk")
    })
    .replace('{', "{{")
    .replace('}', "}}");
    let next = (if incremental {
        include_str!("../../../tests/fixtures/migration_incremental_new.jk")
    } else if owned {
        include_str!("../../../tests/fixtures/migration_owned_new.jk")
    } else {
        include_str!("../../../tests/fixtures/migration_new.jk")
    })
    .replace('{', "{{")
    .replace('}', "}}");
    let prepare = if incremental {
        r#"let method="start"; let advance="advance"
    assert(vm.begin_migration(h,vm.domain_schema(h),&next,&method,&advance)==0)
    var status=vm.step_migration(h,2)
    while status==1 {task.yield_now(); status=vm.step_migration(h,2)}
    assert(status==0)"#
    } else {
        r#"let method="upgrade"
    assert(vm.prepare_migration(h,vm.domain_schema(h),&next,&method)==0)"#
    };
    let source = format!(
        r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="migration cancellation"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{task.yield_now()}}
    let next={next:?}
    {prepare}
    task.yield_now()
    assert(vm.publish_migration(h)==0)
    task.yield_now()
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==43)
}}
fn main() @suspends ->Int32 {{
    let owner=domain.directory(1)
    let directory=owner.handle()
    let h=vm.Vm.interpreter()
    let source={old:?}
    assert(vm.load_source(h,&source)==0)
    race {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(vm.close(h)==0)
    0
}}"#
    );
    migration_cancellation_source(&source, boundaries, exhaustive);
}
fn migration_cancellation_source(
    source: &str,
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
) {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut host = VmEngine::with_mode(mode);
        host.load_source(source).unwrap();
        if mode == VmMode::Interpreter {
            host.set_interpreter_limits(crate::vm::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut completed = false;
        let mut saw_pending = false;
        for boundary in boundaries.clone() {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, host.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut done = false;
            for _ in 0..boundary {
                for result in executor.poll(1).unwrap().completed {
                    assert_eq!(
                        result
                            .result
                            .unwrap_or_else(|e| panic!("{mode:?} boundary={boundary}: {e}")),
                        0
                    );
                    done = true;
                }
                if done {
                    break;
                }
            }
            saw_pending |= executor.domains().borrow().usage().3 != 0;
            executor.cancel_group(&group);
            VMS.with(|vms| assert!(vms.borrow().is_empty()));
            assert_eq!(executor.task_count(), 0);
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            drop(executor);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
            if done {
                completed = true;
                break;
            }
        }
        assert!(completed, "{mode:?}: cancellation sweep missed completion");
        if exhaustive {
            assert!(
                saw_pending,
                "{mode:?}: cancellation sweep missed a prepared candidate"
            );
        }
    }
}
#[test]
fn migration_script_parent_cancellation_and_vm_close_release_candidates() {
    migration_cancellation(
        [0, 1, 2, 5, 10, 20, 50, 100, 200, 400, 800, 1600, 3200],
        false,
        false,
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel migration preparation and publication at every host instruction"]
fn exhaustive_migration_script_cancellation() {
    migration_cancellation(0..=4096, true, false, false);
}

#[test]
fn migration_owned_script_cancellation_releases_pending_graphs() {
    migration_cancellation(
        [0, 1, 10, 50, 100, 200, 400, 800, 1600, 3200],
        false,
        true,
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel owning migration at every host instruction"]
fn exhaustive_migration_owned_script_cancellation() {
    migration_cancellation(0..=4096, true, true, false);
}

#[test]
fn migration_incremental_script_cancellation_releases_partial_candidates() {
    migration_cancellation(
        [0, 1, 10, 50, 100, 200, 400, 800, 1600, 3200, 6400],
        false,
        true,
        true,
    );
}
#[test]
#[ignore = "exhaustive: cancel incremental migration at every host instruction"]
fn exhaustive_migration_incremental_script_cancellation() {
    migration_cancellation(0..=8192, true, true, true);
}

fn batch_cancellation(boundaries: impl Clone + IntoIterator<Item = usize>, exhaustive: bool) {
    let old = include_str!("../../../tests/fixtures/migration_batch_old.jk")
        .replace('{', "{{")
        .replace('}', "}}");
    let next = include_str!("../../../tests/fixtures/migration_batch_new.jk")
        .replace('{', "{{")
        .replace('}', "}}");
    let source = format!(
        r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="migration batch cancellation"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{task.yield_now()}}
    let next={next:?}; let start="start"; let advance="advance"; let ledger="upgrade_ledger"; let empty=""
    assert(vm.begin_migration_batch(h,&next)==0)
    task.yield_now()
    assert(vm.stage_migration(h,&start,&advance)==0)
    task.yield_now()
    assert(vm.stage_migration(h,&ledger,&empty)==0)
    var status=vm.step_migration(h,2)
    while status==1 {{task.yield_now(); status=vm.step_migration(h,2)}}
    assert(status==0)
    task.yield_now()
    assert(vm.publish_migration(h)==0)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==194)
}}
fn main() @suspends ->Int32 {{
    let owner=domain.directory(4); let directory=owner.handle()
    let h=vm.Vm.interpreter(); let old={old:?}
    assert(vm.load_source(h,&old)==0)
    race {{ h.call::<domain.DirectoryHandle,Unit>("hold",directory), control(&h,directory) }};
    assert(vm.close(h)==0)
    0
}}"#
    );
    migration_cancellation_source(&source, boundaries, exhaustive);
}
#[test]
fn migration_batch_script_cancellation_releases_all_staged_roots() {
    batch_cancellation(
        [0, 1, 10, 50, 100, 200, 400, 800, 1600, 3200, 6400, 12800],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel batch migration at every host instruction"]
fn exhaustive_migration_batch_script_cancellation() {
    batch_cancellation(0..=16384, true);
}

#[test]
fn vm_resources_move_return_and_drop_on_success_or_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut host = VmEngine::with_mode(mode);
        host.load_source(
            r#"
            import joky/vm
            struct Engine { value: vm.Vm }
            fn make() -> Engine { Engine { value: vm.Vm.interpreter() } }
            fn run(fail: Int32) -> Int32 {
                let first = make()
                let moved = first
                let source = "fn answer() -> Int32 {{ 42 }}"
                let name = "answer"
                vm.load_source(moved.value, &source)
                vm.call_int32_0(moved.value, &name) / fail
            }
            fn early() -> Int32 { let engine = make(); return 42 }
            fn close() -> Int32 { let engine = vm.Vm.new(); engine.close() }
            fn status() -> Int32 {
                { let engine = make(); let invalid = "fn broken("; vm.load_source(engine.value, &invalid) }
                vm.error_code()
            }
        "#,
        )
        .unwrap();
        assert_eq!(host.call_int32_1("run", 1).unwrap(), 42);
        VMS.with(|vms| assert!(vms.borrow().is_empty()));
        assert!(host.call_int32_1("run", 0).is_err());
        VMS.with(|vms| assert!(vms.borrow().is_empty()));
        assert_eq!(host.call_int32_0("status").unwrap(), 1);
        VMS.with(|vms| assert!(vms.borrow().is_empty()));
        assert_eq!(host.call_int32_0("early").unwrap(), 42);
        assert_eq!(host.call_int32_0("close").unwrap(), 0);
        VMS.with(|vms| assert!(vms.borrow().is_empty()));
        drop(host);
        assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
    }
}

#[test]
fn vm_resource_factory_rolls_back_when_owner_allocation_fails() {
    use crate::runtime::{
        arena::{Arena, Budget},
        resource,
    };
    let budget = std::sync::Arc::new(Budget {
        limit: (0).into(),
        ..Default::default()
    });
    let mut arena = Arena::with_budget(budget.clone(), None);
    for mode in [0, 1] {
        assert!(unsafe { resource::invoke(&mut arena, &managed::FACTORY, mode) }.is_err());
        VMS.with(|vms| assert!(vms.borrow().is_empty()));
        assert_eq!(budget.bytes.get(), 0);
    }
}

#[test]
fn vm_resource_rejects_copy_double_close_and_close_during_borrow() {
    for body in [
        "let h = vm.Vm.new(); let moved = h; vm.module_count(h)",
        "let h = vm.Vm.new(); h.close(); h.close()",
        "let h = vm.Vm.new(); join { h.call::<Int32,Int32>(\"run\",0), h.close() };",
        "let h = vm.Vm.new(); let raw = h.owner",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut host = VmEngine::with_mode(mode);
            let source = format!("import joky/vm fn main() @suspends {{ {body}; }}");
            assert!(host.load_source(&source).is_err(), "{source}");
            VMS.with(|vms| assert!(vms.borrow().is_empty()));
        }
    }
}

#[test]
fn vm_native_registration_rejects_source_module_internal_entries() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("engine", r#"
        import joky/vm as runtime.{type Vm as Engine, register_int32_1 as register}
        fn increment(n: Int32) -> Int32 { n + 1 }
        pub fn answer() -> Int32 {
            let engine = Engine.interpreter()
            let name = "increment"
            register(engine, &name, increment)
            let code = "extern fn increment(n: Int32) -> Int32 fn answer() -> Int32 {{ increment(41) }}"
            runtime.load_source(engine, &code)
            let entry = "answer"
            runtime.call_int32_0(engine, &entry)
        }
    "#).unwrap();
    let mut host = VmEngine::new();
    let error = host
        .load_source_with_modules(
            "import engine fn answer() -> Int32 { engine.answer() }",
            &modules,
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidCall);
    assert!(error
        .message()
        .contains("Function reference `engine.increment`"));
    VMS.with(|vms| assert!(vms.borrow().is_empty()));
}

#[path = "../../../tests/support/enum_values.rs"]
mod enum_values;

fn value_bridge_cancellation(
    source: fn(&str) -> String,
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
) {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for create in ["new", "interpreter"] {
            let mut host = VmEngine::with_mode(mode);
            host.load_source(&source(create))
                .unwrap_or_else(|e| panic!("{e}"));
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
                VMS.with(|vms| assert!(vms.borrow().is_empty()));
                assert_eq!(executor.task_count(), 0);
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
            assert!(completed, "{mode:?}/{create}: sweep missed completion");
        }
    }
}

#[test]
fn pure_enum_script_cancellation_reclaims_packets() {
    value_bridge_cancellation(
        enum_values::source,
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel typed enum Script inputs and results at every instruction"]
fn exhaustive_pure_enum_script_cancellation() {
    value_bridge_cancellation(enum_values::source, 0..=32768, true);
}

#[path = "../../../tests/support/composite_values.rs"]
mod composite_values;

#[test]
fn pure_composite_script_cancellation_reclaims_packets() {
    value_bridge_cancellation(
        composite_values::source,
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel composite Script inputs and results at every instruction"]
fn exhaustive_pure_composite_script_cancellation() {
    value_bridge_cancellation(composite_values::source, 0..=32768, true);
}

#[path = "../../../tests/support/owned_values.rs"]
mod owned_values;

#[test]
fn owned_script_cancellation_reclaims_inputs_results_and_packets() {
    value_bridge_cancellation(
        owned_values::source,
        [0, 1, 2, 5, 10, 30, 100, 300, 1000, 4000, 16000, 65536],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel owning Script inputs and results at every instruction"]
fn exhaustive_owned_script_cancellation() {
    value_bridge_cancellation(owned_script_lifecycle_source, 0..=32768, true);
}

fn owned_script_lifecycle_source(create: &str) -> String {
    let script = r#"import joky/array.{type Array} import joky/result.{type Result} import joky/task
fn exchange(value:Array<Result<String,String>>) @suspends ->Array<Result<String,String>> {
    task.yield_now()
    var result=value
    result.push(Result.Ok("a longer result that grows the caller packet and allocates a fresh returned String"))
    result
}"#;
    let quoted = format!("{:?}", script.replace('{', "{{").replace('}', "}}"));
    format!(
        r#"import joky/vm import joky/array.{{type Array}} import joky/result.{{type Result}}
fn main() @suspends ->Int32 {{
    let h=vm.Vm.{create}(); h.load_source({quoted})!
    var values=Array.new::<Result<String,String>>(); values.push(Result.Err("input🙂"))
    let result=h.call::<Array<Result<String,String>>,Array<Result<String,String>>>("exchange",values)
    h.close();
    if result.len()==2 {{42}} else {{0}}
}}"#
    )
}

#[test]
fn owned_script_allocation_failures_reclaim_inputs_results_and_packets() {
    use crate::runtime::{
        arena,
        executor::{HostExecutor, TaskGroup},
    };
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for create in ["new", "interpreter"] {
            let mut host = VmEngine::with_mode(mode);
            host.load_source(&owned_script_lifecycle_source(create))
                .unwrap_or_else(|e| panic!("{e}"));
            let baseline = arena::LIVE.with(|n| n.get());
            let mut successes = 0;
            let mut failures = 0;
            for limit in (0..=16384).step_by(128) {
                arena::LIMIT.with(|n| n.set(limit));
                let executor = HostExecutor::default();
                let group = TaskGroup::new();
                let outcome = (|| {
                    let id = executor.spawn(&group, host.prepare_async("main", &[])?)?;
                    for _ in 0..1000 {
                        for done in executor.poll(1)?.completed {
                            if done.id == id {
                                return done.result;
                            }
                        }
                    }
                    panic!("owned Script allocation probe stalled");
                })();
                match outcome {
                    Ok(value) => {
                        assert_eq!(value, 42);
                        successes += 1;
                    }
                    Err(_) => failures += 1,
                }
                executor.cancel_group(&group);
                VMS.with(|vms| assert!(vms.borrow().is_empty()));
                assert_eq!(executor.task_count(), 0);
                assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
                drop(executor);
                assert_eq!(
                    arena::LIVE.with(|n| n.get()),
                    baseline,
                    "{mode:?}/{create} limit={limit}"
                );
                // Every failure budget has been checked; larger budgets only
                // repeat the same successful nested compilation and transfer.
                if successes > 0 {
                    break;
                }
            }
            assert!(successes > 0 && failures > 0, "{mode:?}/{create}");
            arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
        }
    }
}

#[test]
fn script_try_call_cancellation_and_failure_reclaim_children() {
    script_bridge_cancellation([0, 1, 5, 20, 100, 400, 800], false, false, true);
}
#[test]
#[ignore = "exhaustive: cancel a handled Script failure at every host instruction"]
fn exhaustive_script_try_call_cancellation() {
    script_bridge_cancellation(0..=4096, true, false, true);
}
