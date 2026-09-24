//! Exported continuation ABI wrappers.

use super::*;

pub(crate) fn task_continuation(
    context: *mut crate::runtime::task::TaskContext,
) -> Option<Continuation> {
    task_continuations()
        .lock()
        .expect("task continuation mutex")
        .get(&(context as usize))
        .cloned()
}

pub(crate) unsafe fn cancel_task_continuation(context: *mut crate::runtime::task::TaskContext) {
    if let Some(continuation) = task_continuation(context) {
        continuation.cancel();
    }
}

#[no_mangle]
pub(crate) extern "C" fn jk_continuation_new(generation: u64) -> *mut Continuation {
    register_handle(Continuation::new(generation))
}

/// Begin a non-blocking continuation sleep. A return value of `1` means the
/// continuation entered `Sleeping`/`Pending`; the timer will later transition
/// it to `Ready` and dispatch its registered machine entry or callback.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_begin_sleep(
    continuation: *mut Continuation,
    milliseconds: u64,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| {
            if !crate::runtime::task::has_current_task_context() {
                // A worker may win the resume race while this thread sleeps,
                // so pin the handler chain for the machine entry first.
                value.inner.handler_frame.store(
                    crate::runtime::handler::current() as usize,
                    Ordering::Release,
                );
                crate::runtime::task::sleep_current_task(milliseconds);
                let machine_entry = value.inner.machine_entry.load(Ordering::Acquire);
                let callback = value.inner.resume_callback.load(Ordering::Acquire);
                if !machine_entry.is_null() || !callback.is_null() {
                    let _ = value.dispatch(continuation, value.generation());
                    return true;
                }
                return false;
            }
            let machine_entry = value.inner.machine_entry.load(Ordering::Acquire);
            let callback = value.inner.resume_callback.load(Ordering::Acquire);
            if machine_entry.is_null() && callback.is_null() {
                crate::runtime::task::sleep_current_task(milliseconds);
                return false;
            }
            if let Some(context) = crate::runtime::task::current_task_context() {
                if crate::runtime::task::task_context_is_cancelled(context) {
                    // The task was cancelled before it reached the suspend
                    // point. Do not publish a new pending continuation.
                    unsafe { crate::runtime::task::cancel_task_context(context) };
                    return false;
                }
            }
            if let Some(context) = crate::runtime::task::current_task_context() {
                value.bind_task_context(context, continuation);
            }
            let pending = value.sleep_with_handle(continuation, milliseconds, true);
            if pending {
                crate::runtime::task::mark_current_task_continuation_pending();
            } else if let Some(context) = crate::runtime::task::current_task_context() {
                value.unbind_task_context(context, continuation);
            }
            pending
        })
        .into()
}

/// Reserve a function call activation without changing the current task.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_begin_function_pending(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.begin_function_pending(continuation))
        .into()
}

/// Reserve a call activation whose final result belongs to its parent's call.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_discard_ready_call(
    continuation: *mut Continuation,
) {
    let Some(value) = (unsafe { retain_handle(continuation) }) else {
        return;
    };
    let state = value.inner.state.lock().expect("continuation state mutex");
    let phase = value
        .inner
        .function_call
        .lock()
        .expect("function call mutex");
    if *state != ContinuationState::Ready || *phase != FunctionCallPhase::Claimed {
        return;
    }
    // Ready never transferred frame ownership away from the running entry.
    // The call result was atomically claimed, so all remaining registrations
    // describe unused saved copies, not independently owned values.
    value
        .inner
        .cleanup_entries
        .lock()
        .expect("continuation cleanup mutex")
        .clear();
    value
        .inner
        .owned_handlers
        .lock()
        .expect("owned handlers mutex")
        .clear();
    let parent = value.inner.function_parent.swap(0, Ordering::AcqRel);
    drop(phase);
    drop(state);
    value.detach_function_parent(parent);
    unsafe { jk_continuation_free(continuation) };
}

/// Transfer a resumed frame's lifetime to a fresh call activation.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_retire_function_activation(
    continuation: *mut Continuation,
    successor: *mut Continuation,
) {
    let Some(previous) = (unsafe { retain_handle(continuation) }) else {
        return;
    };
    let Some(next) = (unsafe { retain_handle(successor) }) else {
        return;
    };
    if continuation == successor {
        return;
    }
    let state = previous
        .inner
        .state
        .lock()
        .expect("continuation state mutex");
    if *state != ContinuationState::Resuming {
        return;
    }
    // The generated entry has transferred all live frame/spill ownership to
    // successor. Retire the obsolete activation without dropping those copies
    // or publishing an early final result to the enclosing caller.
    previous
        .inner
        .cleanup_entries
        .lock()
        .expect("continuation cleanup mutex")
        .clear();
    next.inner
        .owned_handlers
        .lock()
        .expect("owned handlers mutex")
        .extend(std::mem::take(
            &mut *previous
                .inner
                .owned_handlers
                .lock()
                .expect("owned handlers mutex"),
        ));
    let parent = previous.inner.function_parent.swap(0, Ordering::AcqRel);
    let context = previous.inner.task_context.swap(0, Ordering::AcqRel);
    if context != 0 {
        next.bind_task_context(context as *mut crate::runtime::task::TaskContext, successor);
    }
    drop(state);
    previous.detach_function_parent(parent);
    previous.complete();
    if previous.is_cancelled() {
        unsafe {
            jk_continuation_fail_function_chain(
                successor,
                crate::runtime::abi::FunctionCallStatus::Cancelled as u8,
            )
        };
    }
    unsafe { jk_continuation_free(continuation) };
}

/// Terminate a failed resumed call without leaving parent scope leases asleep.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_fail_function_chain(
    continuation: *mut Continuation,
    status: u8,
) {
    let mut handle = continuation;
    let mut seen = std::collections::HashSet::new();
    while !handle.is_null() && seen.insert(handle as usize) {
        let Some(value) = (unsafe { retain_handle(handle) }) else {
            break;
        };
        // A resumed caller may fail before its fresh call activation ever
        // publishes Pending. This terminal handoff transfers that activation
        // to the runtime too; otherwise cancellation cleans its buffers but
        // leaves the public handle registered forever.
        value.inner.runtime_owned.store(true, Ordering::Release);
        let parent = value.inner.function_parent.load(Ordering::Acquire) as *mut Continuation;
        // Task cancellation and handled aborts belong to the enclosing task
        // group; they must not poison the whole program's root status. A
        // chain without a task context reports only genuine failures:
        // cancellation reached this chain through an enclosing task's
        // teardown, which owns the terminal state.
        if value.function_task_context() == 0
            && status != crate::runtime::abi::FunctionCallStatus::Cancelled as u8
        {
            value.inner.scope.record_function_failure(status);
        }
        value.cancel();
        // Generated callers invoke this only after storing live ownership in
        // the frame and immediately return. Resuming owns its deferred cleanup.
        value.complete();
        handle = parent;
    }
}

/// Return the final-result destination retained across successive operations.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_function_parent(
    continuation: *mut Continuation,
) -> *mut Continuation {
    unsafe { retain_handle(continuation) }.map_or(std::ptr::null_mut(), |value| {
        value.inner.function_parent.load(Ordering::Acquire) as *mut Continuation
    })
}

/// Reserve a call activation whose final result belongs to its parent's call.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_begin_function_pending_with_parent(
    continuation: *mut Continuation,
    parent: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.begin_function_pending_with_parent(continuation, parent))
        .into()
}

/// Inspect completion after the callee returns, before unwinding the caller.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_poll_function_pending(
    continuation: *mut Continuation,
) -> u8 {
    let status = unsafe { retain_handle(continuation) }.map_or(
        crate::runtime::abi::FunctionCallStatus::Cancelled,
        |value| value.poll_function_pending(),
    );
    if status == crate::runtime::abi::FunctionCallStatus::Pending {
        super::function_calls::defer_function_pending(continuation);
    }
    status as u8
}

/// Resume entries reload their saved frame even when a child finishes inline.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_poll_function_pending_resume(
    continuation: *mut Continuation,
) -> u8 {
    let status = unsafe { retain_handle(continuation) }.map_or(
        crate::runtime::abi::FunctionCallStatus::Cancelled,
        |value| value.poll_function_pending_resume(),
    );
    if status == crate::runtime::abi::FunctionCallStatus::Pending {
        super::function_calls::defer_function_pending(continuation);
    }
    status as u8
}

/// Arm the caller's resume only after its native invocation has unwound.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_publish_function_pending(
    continuation: *mut Continuation,
) -> u8 {
    if !super::function_calls::function_handoff_allowed() {
        return 0;
    }
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.publish_function_pending(continuation))
        .into()
}

/// Transfer a callee result into the caller's suspend-result slot. On rejection
/// the ABI caller retains ownership of the payload, including managed words.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_complete_function_pending(
    continuation: *mut Continuation,
    payload: *const u8,
    payload_size: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| unsafe {
            value.complete_function_pending(continuation, payload, payload_size)
        })
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_complete_function_pending_failure(
    continuation: *mut Continuation,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| unsafe {
            value.complete_function_pending_failure(continuation, operation, payload, payload_size)
        })
        .into()
}

/// Move the result once after Ready or inside the dispatched resume entry.
/// Ready transfers ownership to output. All other statuses leave it untouched.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_take_function_pending_result(
    continuation: *mut Continuation,
    output: *mut u8,
    size: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }.map_or(
        crate::runtime::abi::FunctionCallStatus::Cancelled as u8,
        |value| unsafe { value.take_function_pending_result(output, size) as u8 },
    )
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_take_function_pending_failure(
    continuation: *mut Continuation,
    output: *mut u8,
    size: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }.map_or(
        crate::runtime::abi::FunctionCallStatus::Cancelled as u8,
        |value| unsafe { value.take_function_pending_failure(output, size) as u8 },
    )
}

/// Start a timer-backed `@suspends` operation for the synchronous root path:
/// a thread with no task context (the `main` invocation) parks until the
/// timer completes and then continues its resume tail on the same thread.
/// Task contexts must use the Pending operation ABI instead, which hands the
/// activation to the scheduler instead of blocking the frame.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_start_suspend(
    continuation: *mut Continuation,
    operation: u64,
    milliseconds: u64,
) -> u8 {
    if crate::runtime::task::has_current_task_context() {
        return 0;
    }
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| {
            value
                .inner
                .active_operation
                .store(operation.wrapping_add(1), Ordering::Release);
            value.inner.handler_frame.store(
                crate::runtime::handler::current() as usize,
                Ordering::Release,
            );
            crate::runtime::task::sleep_current_task(milliseconds);
            let machine_entry = value.inner.machine_entry.load(Ordering::Acquire);
            let callback = value.inner.resume_callback.load(Ordering::Acquire);
            if !machine_entry.is_null() || !callback.is_null() {
                let _ = value.dispatch(continuation, value.generation());
                return true;
            }
            // Keep the operation token until the generated Resume block
            // calls `complete_suspend`. The synchronous root path follows
            // the same state transition as an asynchronous provider, even
            // though no machine entry is available.
            false
        })
        .into()
}

/// Start a suspending operation whose argument list is represented by a
/// flattened ABI byte buffer, for the synchronous root path (no task
/// context). The runtime copies the buffer into continuation owned storage,
/// so the registered provider can inspect it through the pointer/size ABI.
/// Managed pointer words are released when the operation completes or is
/// cancelled. An operation without a registered provider is rejected
/// synchronously. Task contexts must use the Pending provider ABI instead.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_start_suspend_payload(
    continuation: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
) -> u8 {
    if crate::runtime::task::has_current_task_context() {
        return 0;
    }
    if arguments.is_null() && arguments_size != 0 {
        return 0;
    }
    let Some(value) = (unsafe { retain_handle(continuation) }) else {
        return 0;
    };
    if arguments_size != 0
        && value
            .allocate_suspend_arguments(arguments, arguments_size)
            .is_null()
    {
        return 0;
    }
    let pending = value.start_suspend_provider(continuation, operation);
    if !pending {
        value.cleanup_storage(CONTINUATION_SUSPEND_ARGUMENT_STORAGE);
        value.release_suspend_arguments();
    }
    pending.into()
}

/// Register the native provider for one stable effect operation id. The start
/// hook receives `(continuation, operation, args, args_size, result,
/// result_size)` and returns non-zero after accepting the request. The optional
/// cancel hook is called exactly once when an accepted request is cancelled.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_register_suspend_provider(
    operation: u64,
    start: *mut c_void,
    cancel: *mut c_void,
) -> u8 {
    register_suspend_provider(scope::current_id(), operation, start, cancel)
}

pub(crate) unsafe fn register_suspend_provider_for_scope(
    scope_id: ScopeId,
    operation: u64,
    start: *mut c_void,
    cancel: *mut c_void,
) -> u8 {
    register_suspend_provider(scope_id, operation, start, cancel)
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_unregister_suspend_provider(operation: u64) {
    unregister_suspend_provider(scope::current_id(), operation);
}

pub(crate) unsafe fn unregister_suspend_provider_for_scope(scope_id: ScopeId, operation: u64) {
    unregister_suspend_provider(scope_id, operation);
}

/// Re-arm an already-resuming continuation without completing its structured
/// task. The next timer wake dispatches the newly installed machine entry.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_resuspend_sleep(
    continuation: *mut Continuation,
    milliseconds: u64,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.resuspend_sleep(continuation, milliseconds))
        .into()
}

/// Re-arm a timer-backed suspending operation from a machine entry.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_resuspend_suspend(
    continuation: *mut Continuation,
    operation: u64,
    milliseconds: u64,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| {
            value
                .inner
                .active_operation
                .store(operation.wrapping_add(1), Ordering::Release);
            let pending = value.resuspend_sleep(continuation, milliseconds);
            if !pending {
                let state = value.state();
                if state == ContinuationState::Resuming {
                    value.finish_cancelled();
                } else if state != ContinuationState::Cancelled {
                    value.inner.active_operation.store(0, Ordering::Release);
                }
            }
            pending
        })
        .into()
}

/// Re-arm a non-timer suspending operation from a machine entry using the
/// registered provider for the operation.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_resuspend_suspend_payload(
    continuation: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
) -> u8 {
    if arguments.is_null() && arguments_size != 0 {
        return 0;
    }
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| {
            if arguments_size != 0
                && value
                    .allocate_suspend_arguments(arguments, arguments_size)
                    .is_null()
            {
                value.finish_cancelled();
                return false;
            }
            // Re-arm the task association when a resumed machine entry starts
            // another provider-backed operation.
            let context = crate::runtime::task::current_task_context();
            if let Some(context) = context {
                if crate::runtime::task::task_context_is_cancelled(context) {
                    unsafe { crate::runtime::task::cancel_task_context(context) };
                    value.finish_cancelled();
                    return false;
                }
                value.bind_task_context(context, continuation);
            }
            let pending = value.resuspend_provider(continuation, operation);
            if !pending {
                let state = value.state();
                if state == ContinuationState::Resuming {
                    // A failed re-arm must not leave the machine entry in
                    // `Resuming` forever. Treat it like cancellation so all
                    // frame, result and argument ownership is reclaimed.
                    value.finish_cancelled();
                } else if state != ContinuationState::Cancelled {
                    value.inner.active_operation.store(0, Ordering::Release);
                    value.cleanup_storage(CONTINUATION_SUSPEND_ARGUMENT_STORAGE);
                    value.release_suspend_arguments();
                }
                if let Some(context) = context {
                    value.unbind_task_context(context, continuation);
                }
            } else if context.is_some() {
                crate::runtime::task::mark_current_task_continuation_pending();
            }
            pending
        })
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_resume(continuation: *mut Continuation) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.resume())
        .into()
}

/// Machine entries resume on a scheduler thread rather than the original
/// task thread, so they must query cancellation through their continuation.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_is_cancelled(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.inner.cancellation_requested.load(Ordering::Acquire) != 0)
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_resume_at(
    continuation: *mut Continuation,
    generation: u64,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.resume_at(generation))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_cancel(continuation: *mut Continuation) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.cancel())
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_complete(continuation: *mut Continuation) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.complete())
        .into()
}

/// Complete the active suspending operation only if its operation token still
/// matches. This prevents a stale provider completion from consuming a newer
/// continuation request.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_complete_suspend(
    continuation: *mut Continuation,
    operation: u64,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.complete_operation(operation))
        .into()
}

/// Complete an active `@suspends` operation with a flattened ABI result. The
/// call is intended for native providers; managed pointer words are transferred
/// into the continuation and must not be released by the provider afterwards.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_complete_suspend_with_payload(
    continuation: *mut Continuation,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| {
            value.complete_suspend_with_payload(continuation, operation, payload, payload_size)
        })
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_state(continuation: *const Continuation) -> u8 {
    unsafe { retain_const_handle(continuation) }
        .map_or(ContinuationState::Cancelled as u8, |continuation| {
            continuation.state() as u8
        })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_generation(
    continuation: *const Continuation,
) -> u64 {
    unsafe { retain_const_handle(continuation) }.map_or(0, |continuation| continuation.generation())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_set_spill(
    continuation: *mut Continuation,
    pointer: *mut u8,
    size: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.set_spill(pointer, size))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_alloc_spill(
    continuation: *mut Continuation,
    size: usize,
) -> *mut u8 {
    unsafe { retain_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.allocate_spill(size)
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_release_spill(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.release_spill())
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_spill_pointer(
    continuation: *const Continuation,
) -> *mut u8 {
    unsafe { retain_const_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.spill_pointer()
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_spill_size(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }.map_or(0, |continuation| continuation.spill_size())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_set_resume_entry(
    continuation: *mut Continuation,
    entry: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.set_resume_entry(entry))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_resume_entry(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }
        .map_or(0, |continuation| continuation.resume_entry())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_set_root_group(
    continuation: *mut Continuation,
    group: *mut c_void,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .map(|continuation| {
            continuation.set_root_group(group as usize);
            1
        })
        .unwrap_or(0)
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_root_group(
    continuation: *const Continuation,
) -> *mut c_void {
    unsafe { retain_const_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.root_group() as *mut c_void
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_set_resume_callback(
    continuation: *mut Continuation,
    callback: *mut c_void,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.set_resume_callback(callback))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_dispatch(
    continuation: *mut Continuation,
    generation: u64,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.dispatch(continuation, generation))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_set_machine_entry(
    continuation: *mut Continuation,
    entry: *mut c_void,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.set_machine_entry(entry))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_machine_trampoline(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.invoke_machine_entry(continuation))
        .into()
}

/// Register a finalized JIT machine entry under its stable MIR resume key.
/// Registration is process-local and intentionally independent of any one
/// continuation allocation.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_register_machine_entry(
    key: usize,
    entry: *mut c_void,
) -> u8 {
    register_machine_entry(scope::current_id(), key, entry)
}

pub(crate) fn register_machine_entry_for_scope(
    scope_id: ScopeId,
    key: usize,
    entry: *mut c_void,
) -> u8 {
    register_machine_entry(scope_id, key, entry)
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_unregister_machine_entry(key: usize) {
    unregister_machine_entry(scope::current_id(), key);
}

pub(crate) fn unregister_machine_entry_for_scope(scope_id: ScopeId, key: usize) {
    unregister_machine_entry(scope_id, key);
}

pub(crate) fn unregister_machine_entries_for_scope(scope_id: ScopeId) {
    unregister_all_machine_entries_for_scope(scope_id);
}
