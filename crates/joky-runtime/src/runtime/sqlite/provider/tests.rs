use super::*;
use crate::runtime::continuation::*;
use crate::runtime::provider::ProviderOperationEntry;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn wait_until(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "SQLite work did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}

unsafe extern "C" fn error_start(
    handle: *mut Continuation,
    operation: u64,
    _: *const u8,
    _: usize,
    _: *mut u8,
    result_size: usize,
) -> u8 {
    let continuation = Continuation::retain_registered(handle).unwrap();
    complete(
        &continuation,
        handle,
        operation,
        result_size / WORD,
        Err("sqlite error".into()),
    );
    1
}

#[test]
fn error_payloads_use_the_exact_result_layout() {
    let scope = crate::runtime::scope::RuntimeScope::new();
    let _scope_guard = scope.enter();
    let mut provider = crate::runtime::provider::ProviderScope::new(Arc::clone(&scope));
    assert!(provider.register_operation(
        23,
        error_start as *mut c_void,
        sqlite_cancel as *mut c_void
    ));
    for result_words in [3, 4, 5, 6] {
        let handle = jk_continuation_new(1);
        unsafe {
            jk_continuation_alloc_suspend_result(handle, result_words * WORD);
            assert_eq!(
                jk_continuation_start_suspend_payload(handle, 23, std::ptr::null(), 0),
                1
            );
            assert_eq!(
                jk_continuation_state(handle),
                ContinuationState::Ready as u8
            );
            let pointer = jk_continuation_suspend_result_pointer(handle);
            assert_eq!(words(pointer, 0), 1);
            let text = words(pointer, result_words - 2) as *mut u8;
            let length = words(pointer, result_words - 1);
            assert_eq!(std::slice::from_raw_parts(text, length), b"sqlite error");
            jk_drop(text);
            jk_continuation_free(handle);
        }
        assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn rejected_completion_releases_strings_and_native_handles() {
    let scope = crate::runtime::scope::RuntimeScope::new();
    let _scope_guard = scope.enter();
    let handle = jk_continuation_new(1);
    let continuation = unsafe { Continuation::retain_registered(handle) }.unwrap();
    // An inactive operation rejects publication without the cancellation fast path.
    let db = SqliteConnection::open(":memory:").unwrap();
    let weak = Arc::downgrade(&db);
    let result = Output::handle(allocate_handle(db, scope.id()));
    complete(&continuation, handle, 23, 4, result);
    assert!(weak.upgrade().is_none());
    assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);

    let db = SqliteConnection::open(":memory:").unwrap();
    let statement = db.prepare("select 1").unwrap();
    let finalized = Arc::clone(&statement.finalized);
    let result = Output::handle(allocate_statement_handle(statement, scope.id()));
    complete(&continuation, handle, 23, 4, result);
    wait_until(|| finalized.load(Ordering::SeqCst) == 1);
    // Late string payloads clean up through the error writer, which owns the
    // same managed-string slot layout a successful string result used.
    complete(&continuation, handle, 23, 5, Err("late error".into()));
    assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn rejected_row_completion_drops_the_entire_payload() {
    let scope = crate::runtime::scope::RuntimeScope::new();
    let _guard = scope.enter();
    let handle = jk_continuation_new(1);
    let continuation = unsafe { Continuation::retain_registered(handle) }.unwrap();
    let db = SqliteConnection::open(":memory:").unwrap();
    let statement = db.prepare("select 1").unwrap();
    let finalized = Arc::clone(&statement.finalized);
    let row = row_list(vec![
        Value::Integer(i64::MIN),
        Value::Real(1.25),
        Value::Text("retained".into()),
        Value::Blob(vec![0, 1, 2]),
        Value::Null,
    ])
    .unwrap();
    let next = Managed(allocate_statement_handle(statement, scope.id()) as usize);
    complete(
        &continuation,
        handle,
        23,
        6,
        Ok(Output::Row(Some((row, next)))),
    );
    wait_until(|| finalized.load(Ordering::SeqCst) == 1);
    assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn running_query_keeps_statement_alive_until_cancelled_work_returns() {
    let scope = crate::runtime::scope::RuntimeScope::new();
    let _scope_guard = scope.enter();
    let entries = [ProviderOperationEntry {
        effect: "sqlite",
        name: "step",
        operation: 23,
    }];
    let _provider = register_operations(&entries).unwrap();
    let db = SqliteConnection::open(":memory:").unwrap();
    let statement = db.prepare("select 'late row'").unwrap();
    let finalized = Arc::clone(&statement.finalized);
    let pointer = allocate_statement_handle(Arc::clone(&statement), scope.id());
    let connection_lock = db.db.lock().unwrap();
    let handle = jk_continuation_new(1);
    unsafe {
        jk_continuation_alloc_suspend_result(handle, 6 * WORD);
        jk_continuation_register_cleanup(
            handle,
            joky_runtime_abi::CONTINUATION_SUSPEND_ARGUMENT_STORAGE,
            0,
            jk_drop as *mut c_void,
        );
        assert_eq!(
            jk_continuation_start_suspend_payload(
                handle,
                23,
                [pointer as usize].as_ptr().cast(),
                WORD
            ),
            1
        );
    }
    wait_until(|| statement.executing.load(Ordering::Acquire));
    unsafe {
        assert_eq!(jk_continuation_cancel(handle), 1);
        jk_continuation_free(handle);
    }
    drop(statement);
    assert_eq!(finalized.load(Ordering::SeqCst), 0);
    assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);
    let (drained, drained_rx) = std::sync::mpsc::channel();
    let waiting = Arc::clone(&scope);
    let waiter = std::thread::spawn(move || {
        waiting.close_and_wait();
        drained.send(()).unwrap();
    });
    assert!(drained_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(connection_lock);
    drained_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    waiter.join().unwrap();
    wait_until(|| finalized.load(Ordering::SeqCst) == 1);
    wait_until(|| scope.resources.snapshot()[0] == 0);
    assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);
}
