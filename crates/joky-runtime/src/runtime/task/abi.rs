use super::context::TaskResultDropThunk;
use super::*;
use std::ffi::c_void;
use std::sync::atomic::AtomicUsize;

#[no_mangle]
pub(crate) extern "C" fn jk_task_group_new(region: usize) -> *mut TaskGroup {
    let mut group = TaskGroup::new();
    if region != 0 {
        group.region = Some(crate::runtime::region::Region::enter());
    }
    Box::into_raw(Box::new(group))
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_group_free(group: *mut TaskGroup) {
    if !group.is_null() {
        // SAFETY: codegen frees each lexical group exactly once after all task
        // contexts in its parent stack frame are still alive.
        unsafe { drop(Box::from_raw(group)) };
    }
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_spawn(
    group: *mut TaskGroup,
    thunk: *mut c_void,
    context: *mut TaskContext,
) -> u64 {
    // JIT code passes the generated entry as an opaque pointer. Keep the
    // exported ABI pointer-only while using a Rust-ABI thunk internally so
    // panics can be contained by `catch_unwind`.
    let thunk: TaskThunk = unsafe { std::mem::transmute(thunk) };
    // SAFETY: generated code supplies a live group and task context.
    unsafe { (*group).spawn(thunk, context).0 }
}

/// Dispose a group retained in a cancelled continuation frame. Normal
/// ScopeExit clears its frame slot before freeing the group itself.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_group_cancel_free(group: *mut TaskGroup) {
    if !group.is_null() {
        unsafe {
            (*group).inner.cancel_all();
            jk_task_group_free(group);
        }
    }
}

/// Spawn a task whose capture/result storage must outlive the current machine
/// entry. The runtime copies captures into heap-owned storage and keeps the
/// complete context alive until the task group's close path has drained it.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_spawn_heap(
    group: *mut TaskGroup,
    thunk: *mut c_void,
    captures: *const u8,
    captures_size: usize,
    result_size: usize,
    result_drop: *mut c_void,
) -> u64 {
    if group.is_null() || thunk.is_null() || (captures.is_null() && captures_size != 0) {
        return 0;
    }
    let captures = if captures_size == 0 {
        Vec::new().into_boxed_slice()
    } else {
        std::slice::from_raw_parts(captures, captures_size)
            .to_vec()
            .into_boxed_slice()
    };
    let result = vec![0_u8; result_size.max(1)].into_boxed_slice();
    let mut storage = Box::new(HeapTaskStorage {
        context: TaskContext {
            captures: std::ptr::null_mut(),
            result: std::ptr::null_mut(),
            result_size,
            drop_result: if result_drop.is_null() {
                None
            } else {
                // SAFETY: codegen supplies a C-ABI drop thunk taking the result storage pointer.
                Some(std::mem::transmute::<*mut c_void, TaskResultDropThunk>(
                    result_drop,
                ))
            },
            cancelled_result: false,
            result_initialized: false,
            cancellation: std::ptr::null(),
            abort_operation: NO_TASK_ABORT,
            group: std::ptr::null(),
            task: TaskId(0),
            wake: std::ptr::null(),
            continuation_pending: AtomicBool::new(false),
            handler_frame: std::ptr::null(),
            continuation: AtomicUsize::new(0),
        },
        captures,
        result,
    });
    storage.context.captures = storage.captures.as_mut_ptr().cast();
    storage.context.result = storage.result.as_mut_ptr().cast();
    let context = &mut storage.context as *mut TaskContext;
    heap_task_storage()
        .lock()
        .expect("heap task storage mutex")
        .insert(context as usize, storage);
    #[cfg(any(test, feature = "test-support"))]
    {
        TEST_HEAP_TASK_SPAWNS.fetch_add(1, Ordering::SeqCst);
        TEST_HEAP_TASK_PEAK.fetch_max(heap_task_storage().lock().unwrap().len(), Ordering::SeqCst);
    }
    let task_thunk: TaskThunk = std::mem::transmute(thunk);
    (*group).spawn(task_thunk, context).0
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_join(group: *mut TaskGroup, task: u64) {
    // SAFETY: generated code only joins tasks created in this group.
    unsafe { (*group).join(TaskId(task)) };
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_cancel(group: *mut TaskGroup, task: u64) {
    // SAFETY: generated code only cancels tasks created in this group.
    unsafe { (*group).cancel(TaskId(task)) };
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_claim_result(group: *mut TaskGroup, task: u64) {
    // SAFETY: generated code only claims a joined task result once.
    unsafe { (*group).claim_result(TaskId(task)) };
}

/// Borrow the result buffer retained by a live group. The caller must join
/// before reading it and claim before moving managed values out. The pointer
/// becomes invalid at group close; this lookup alone transfers no ownership.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_result_pointer(
    group: *mut TaskGroup,
    task: u64,
) -> *mut std::ffi::c_void {
    let group = unsafe { &*group };
    let records = group.inner.tasks.lock().expect("task map mutex");
    records
        .get(&TaskId(task))
        .map(|record| unsafe { (*record.context).result })
        .unwrap_or(std::ptr::null_mut())
        .cast()
}

/// Returns the first abort operation for `group`, or `u64::MAX` when no task
/// has failed. The group must remain live while the caller reads its payload.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_group_failure_operation(group: *const TaskGroup) -> u64 {
    unsafe { group.as_ref() }
        .and_then(TaskGroup::failure_operation)
        .unwrap_or(NO_TASK_ABORT)
}

/// Returns a pointer to the first failure's copied flattened payload, or null
/// when there is no failure/payload. It remains valid until the group is freed.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_group_failure_payload(
    group: *const TaskGroup,
) -> *const c_void {
    unsafe { group.as_ref() }
        .and_then(TaskGroup::failure_payload)
        .map(|(payload, _)| payload)
        .unwrap_or(std::ptr::null())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_group_failure_payload_size(
    group: *const TaskGroup,
) -> usize {
    unsafe { group.as_ref() }
        .and_then(TaskGroup::failure_payload)
        .map(|(_, size)| size)
        .unwrap_or(0)
}

/// Transfer the current failure payload into generated handler locals.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_group_claim_failure(group: *const TaskGroup) {
    if let Some(group) = unsafe { group.as_ref() } {
        group.claim_failure();
    }
}

/// Copy a nested task scope failure into the current task's parent scope.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_rethrow_failure(group: *const TaskGroup) {
    let Some(failure) = unsafe { group.as_ref() }.and_then(TaskGroup::take_failure) else {
        return;
    };
    abort_current_task(failure);
}

#[no_mangle]
pub(crate) extern "C" fn jk_task_is_cancelled() -> u8 {
    if crate::runtime::destruction::active() {
        return 0;
    }
    CURRENT_CANCELLATION.with(|current| {
        let cancellation = current.get();
        if cancellation.is_null() {
            return u8::from(
                crate::runtime::scope::current_or_default()
                    .root_failure
                    .lock()
                    .expect("root failure mutex")
                    .is_some(),
            );
        }
        // SAFETY: the runtime installs the pointer only while its TaskControl
        // Arc is live on this thread.
        u8::from(unsafe { (*cancellation).load(Ordering::Acquire) })
    })
}

#[no_mangle]
pub(crate) extern "C" fn jk_task_mark_cancelled_result() {
    CURRENT_TASK_CONTEXT.with(|current| {
        let context = current.get();
        if !context.is_null() {
            // SAFETY: this executes only in the owning task thread while the
            // parent stack frame keeps the context alive.
            unsafe { (*context).cancelled_result = true };
        }
    });
}

/// Report a non-resumable language effect from the current task.
///
/// Generated code must take its normal cleanup-and-return path immediately
/// afterwards. The runtime only coordinates sibling cancellation and retains
/// the first operation identity for the parent scope.
#[no_mangle]
pub(crate) extern "C" fn jk_task_abort(operation: u64) {
    abort_current_task(TaskFailure {
        operation,
        payload: Vec::new(),
        drop_payload: None,
    });
}

/// Report a non-resumable language effect and copy its flattened ABI payload
/// out of the child task's stack frame.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_abort_payload(
    operation: u64,
    payload: *const c_void,
    payload_size: usize,
    drop_payload: *const c_void,
) {
    let payload = if payload_size == 0 {
        &[]
    } else {
        // SAFETY: generated code provides a live flattened ABI buffer for
        // this call. `abort_current_task` copies it before returning.
        unsafe { std::slice::from_raw_parts(payload.cast::<u8>(), payload_size) }
    };
    abort_current_task(TaskFailure {
        operation,
        payload: payload.to_vec(),
        drop_payload: if drop_payload.is_null() {
            None
        } else {
            // SAFETY: generated code passes a function with the
            // `TaskFailureDropThunk` ABI.
            Some(unsafe {
                std::mem::transmute::<*const c_void, TaskFailureDropThunk>(drop_payload)
            })
        },
    });
}

#[no_mangle]
pub(crate) extern "C" fn jk_time_sleep(milliseconds: u64) {
    sleep_current_task(milliseconds);
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_task_race(
    group: *mut TaskGroup,
    tasks: *const u64,
    task_count: usize,
    result: *mut c_void,
    result_size: usize,
) -> u64 {
    // SAFETY: generated code supplies `task_count` live task IDs and storage
    // valid until the surrounding group exits.
    let tasks = unsafe { std::slice::from_raw_parts(tasks, task_count) }
        .iter()
        .copied()
        .map(TaskId)
        .collect::<Vec<_>>();
    unsafe { (*group).race_into(&tasks, result, result_size) }
        .map(|task| task.0)
        .unwrap_or(0)
}
