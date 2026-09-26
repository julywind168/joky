use super::*;

fn text(value: &str) -> Box<[u8]> {
    crate::runtime::string::encode(value)
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
        let invalid_utf8 = [1, 0, 0, 0, 0, 0, 0, 0, 255];
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
fn busy_vm_reports_an_error_without_panicking() {
    let handle = create();
    let engine = lookup(handle).unwrap();
    let active = engine.borrow_mut();
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
