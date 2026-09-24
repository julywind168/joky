use super::super::*;
use crate::runtime::abi::FunctionCallStatus;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Barrier;

struct Probe {
    hits: AtomicUsize,
    result: Sender<u64>,
    parent: AtomicUsize,
}

struct Call {
    handle: *mut Continuation,
    probe: Box<Probe>,
    result: Receiver<u64>,
}

impl Call {
    fn continuation(&self) -> Continuation {
        unsafe { retain_handle(self.handle) }.unwrap()
    }
    fn new() -> Self {
        let call = Self::allocate(resume);
        assert_eq!(
            unsafe { jk_continuation_begin_function_pending(call.handle) },
            1
        );
        call
    }

    fn operation() -> Self {
        Self::allocate(resume_operation)
    }

    fn allocate(callback: unsafe extern "C" fn(*mut Continuation)) -> Self {
        let (result, receiver) = mpsc::channel();
        let probe = Box::new(Probe {
            hits: AtomicUsize::new(0),
            result,
            parent: AtomicUsize::new(0),
        });
        let handle = jk_continuation_new(11);
        unsafe {
            let frame = jk_continuation_alloc_frame(handle, std::mem::size_of::<usize>());
            std::ptr::write_unaligned(frame.cast::<*const Probe>(), &*probe);
            assert!(!jk_continuation_alloc_suspend_result(handle, 8).is_null());
            assert_eq!(
                jk_continuation_set_resume_callback(handle, callback as *mut c_void),
                1
            );
        }
        Self {
            handle,
            probe,
            result: receiver,
        }
    }

    fn poll(&self) -> u8 {
        unsafe { jk_continuation_poll_function_pending(self.handle) }
    }

    fn complete(&self, value: u64) -> u8 {
        let bytes = value.to_ne_bytes();
        unsafe {
            jk_continuation_complete_function_pending(self.handle, bytes.as_ptr(), bytes.len())
        }
    }

    fn publish(&self) -> u8 {
        unsafe { jk_continuation_publish_function_pending(self.handle) }
    }

    fn no_resume(&self) {
        assert_eq!(self.probe.hits.load(Ordering::Acquire), 0);
        assert_eq!(self.result.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(
            unsafe { jk_continuation_state(self.handle) },
            ContinuationState::Sleeping as u8
        );
    }

    fn resumed(&self, expected: u64) {
        assert_eq!(
            self.result.recv_timeout(Duration::from_secs(2)).unwrap(),
            expected
        );
        assert_eq!(self.probe.hits.load(Ordering::Acquire), 1);
    }

    fn children_count(&self) -> usize {
        unsafe { retain_handle(self.handle) }
            .unwrap()
            .inner
            .function_children
            .lock()
            .unwrap()
            .len()
    }
}

#[test]
fn inline_result_from_resume_entry_waits_for_handoff() {
    let call = Call::new();
    assert_eq!(call.complete(42), 1);
    assert_eq!(
        call.continuation().poll_function_pending_resume(),
        FunctionCallStatus::Pending
    );
    call.no_resume();
    assert_eq!(call.publish(), 1);
    call.resumed(42);
}

#[test]
fn inline_result_from_resume_entry_can_be_cancelled_before_handoff() {
    let call = Call::new();
    assert_eq!(call.complete(42), 1);
    assert_eq!(
        call.continuation().poll_function_pending_resume(),
        FunctionCallStatus::Pending
    );
    assert_eq!(unsafe { jk_continuation_cancel(call.handle) }, 1);
    assert_eq!(call.publish(), 0);
    assert_eq!(call.probe.hits.load(Ordering::Acquire), 0);
}

#[test]
fn completed_function_child_is_removed_from_parent_activation_list() {
    let parent = Call::new();
    let child = Call::operation();
    assert!(child
        .continuation()
        .attach_function_parent(child.handle, parent.handle));
    assert_eq!(parent.children_count(), 1);
    child
        .continuation()
        .detach_function_parent(parent.handle as usize);
    assert_eq!(parent.children_count(), 0);
}

#[test]
fn detaching_last_child_retries_completed_parent_release() {
    let parent = Call::operation();
    let child = Call::operation();
    let parent_value = parent.continuation();
    let child_value = child.continuation();
    assert!(child_value.attach_function_parent(child.handle, parent.handle));

    parent_value.remember_owner_handle(parent.handle);
    parent_value
        .inner
        .free_requested
        .store(true, Ordering::Release);
    *parent_value.inner.state.lock().unwrap() = ContinuationState::Completed;
    parent_value.finish_free();
    assert!(!parent_value.inner.handle_released.load(Ordering::Acquire));

    child_value.detach_function_parent(parent.handle as usize);
    assert!(parent_value.inner.handle_released.load(Ordering::Acquire));
    assert!(registries::retain_owner(parent.handle as usize).is_none());
    unsafe { jk_continuation_free(child.handle) };
}

unsafe extern "C" fn resume_operation(handle: *mut Continuation) {
    unsafe {
        let probe = &*std::ptr::read_unaligned(
            jk_continuation_frame_pointer(handle).cast::<*const Probe>(),
        );
        probe.hits.fetch_add(1, Ordering::AcqRel);
        let result =
            std::ptr::read_unaligned(jk_continuation_suspend_result_pointer(handle).cast::<u64>());
        let parent = probe.parent.load(Ordering::Acquire) as *mut Continuation;
        let sender = probe.result.clone();
        assert_eq!(jk_continuation_complete(handle), 1);
        if !parent.is_null() {
            let payload = (result + 1).to_ne_bytes();
            assert_eq!(
                jk_continuation_complete_function_pending(parent, payload.as_ptr(), payload.len()),
                1
            );
        }
        sender.send(result).unwrap();
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        // Free waits for callback leases before the probe's Box is dropped.
        unsafe { jk_continuation_free(self.handle) };
    }
}

unsafe extern "C" fn resume(handle: *mut Continuation) {
    unsafe {
        let frame = jk_continuation_frame_pointer(handle);
        let probe = &*std::ptr::read_unaligned(frame.cast::<*const Probe>());
        probe.hits.fetch_add(1, Ordering::AcqRel);
        let mut result = 0_u64;
        assert_eq!(
            jk_continuation_take_function_pending_result(
                handle,
                (&mut result as *mut u64).cast(),
                8,
            ),
            FunctionCallStatus::Ready as u8
        );
        let sender = probe.result.clone();
        let parent = probe.parent.load(Ordering::Acquire) as *mut Continuation;
        assert_eq!(jk_continuation_complete(handle), 1);
        if !parent.is_null() {
            let result = (result + 1).to_ne_bytes();
            assert_eq!(
                jk_continuation_complete_function_pending(parent, result.as_ptr(), result.len()),
                1
            );
        }
        sender.send(result).unwrap();
    }
}

#[test]
fn function_pending_inline_completion_does_not_enqueue_caller() {
    let call = Call::new();
    // The call result must not overwrite the enclosing function's final result.
    unsafe {
        let final_result = jk_continuation_alloc_result(call.handle, 8);
        std::ptr::write_unaligned(final_result.cast::<u64>(), 99);
    }
    assert_eq!(call.complete(42), 1);
    call.no_resume();
    assert_eq!(call.poll(), FunctionCallStatus::Ready as u8);
    assert_eq!(call.publish(), 0);
    assert_eq!(call.complete(43), 0);
    unsafe {
        assert_eq!(
            std::ptr::read_unaligned(
                jk_continuation_suspend_result_pointer(call.handle).cast::<u64>()
            ),
            42
        );
        assert_eq!(
            std::ptr::read_unaligned(jk_continuation_result_pointer(call.handle).cast::<u64>()),
            99
        );
    }
    assert_eq!(call.probe.hits.load(Ordering::Acquire), 0);
}

#[test]
fn function_pending_completion_waits_for_native_stack_handoff() {
    let call = Call::new();
    assert_eq!(call.publish(), 0);
    assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
    assert_eq!(call.complete(42), 1);
    call.no_resume();
    assert_eq!(call.publish(), 1);
    call.resumed(42);
    assert_eq!(call.publish(), 0);
}

#[test]
fn function_pending_handoff_before_completion_wakes_exactly_once() {
    let call = Call::new();
    assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
    assert_eq!(call.publish(), 1);
    call.no_resume();
    assert_eq!(call.complete(42), 1);
    call.resumed(42);
    assert_eq!(call.complete(43), 0);
    assert_eq!(call.publish(), 0);
}

#[test]
fn function_pending_completion_racing_handoff_never_loses_wakeup() {
    for value in 0..64_u64 {
        let call = Call::new();
        assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let address = call.handle as usize;
        let worker = std::thread::spawn(move || {
            let bytes = value.to_ne_bytes();
            worker_barrier.wait();
            unsafe {
                jk_continuation_complete_function_pending(
                    address as *mut Continuation,
                    bytes.as_ptr(),
                    bytes.len(),
                )
            }
        });
        barrier.wait();
        assert_eq!(call.publish(), 1);
        assert_eq!(worker.join().unwrap(), 1);
        call.resumed(value);
        assert_eq!(call.publish(), 0);
    }
}

#[test]
fn function_pending_completion_racing_poll_selects_one_resume_path() {
    for _ in 0..64 {
        let call = Call::new();
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let address = call.handle as usize;
        let worker = std::thread::spawn(move || {
            let bytes = 42_u64.to_ne_bytes();
            worker_barrier.wait();
            unsafe {
                jk_continuation_complete_function_pending(
                    address as *mut Continuation,
                    bytes.as_ptr(),
                    bytes.len(),
                )
            }
        });
        barrier.wait();
        let status = call.poll();
        assert_eq!(worker.join().unwrap(), 1);
        if status == FunctionCallStatus::Ready as u8 {
            assert_eq!(call.probe.hits.load(Ordering::Acquire), 0);
            assert_eq!(call.publish(), 0);
        } else {
            assert_eq!(status, FunctionCallStatus::Pending as u8);
            call.no_resume();
            assert_eq!(call.publish(), 1);
            call.resumed(42);
        }
    }
}

struct OwnedResult(Arc<AtomicUsize>);

impl Drop for OwnedResult {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

unsafe extern "C" fn drop_owned_result(value: *mut c_void) {
    unsafe { drop(Box::from_raw(value.cast::<OwnedResult>())) };
}

#[test]
fn function_pending_cancellation_racing_result_transfer_drops_once() {
    for _ in 0..64 {
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = Box::into_raw(Box::new(OwnedResult(Arc::clone(&drops))));
        let handle = jk_continuation_new(11);
        unsafe {
            assert!(
                !jk_continuation_alloc_suspend_result(handle, std::mem::size_of::<usize>())
                    .is_null()
            );
            assert_eq!(
                jk_continuation_register_cleanup(
                    handle,
                    CONTINUATION_SUSPEND_RESULT_STORAGE,
                    0,
                    drop_owned_result as *mut c_void
                ),
                1
            );
            assert_eq!(jk_continuation_begin_function_pending(handle), 1);
        }
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let address = handle as usize;
        let payload_address = payload as usize;
        let worker = std::thread::spawn(move || {
            let bytes = payload_address.to_ne_bytes();
            worker_barrier.wait();
            unsafe {
                jk_continuation_complete_function_pending(
                    address as *mut Continuation,
                    bytes.as_ptr(),
                    bytes.len(),
                )
            }
        });
        barrier.wait();
        assert_eq!(unsafe { jk_continuation_cancel(handle) }, 1);
        if worker.join().unwrap() == 0 {
            // A rejected result remains owned by the callee.
            unsafe { drop(Box::from_raw(payload)) };
        }
        unsafe { jk_continuation_free(handle) };
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}

#[test]
fn function_pending_ready_result_moves_out_before_cancellation() {
    let drops = Arc::new(AtomicUsize::new(0));
    let payload = Box::into_raw(Box::new(OwnedResult(Arc::clone(&drops))));
    let handle = jk_continuation_new(11);
    unsafe {
        let slot = jk_continuation_alloc_suspend_result(handle, std::mem::size_of::<usize>());
        assert!(!slot.is_null());
        assert_eq!(
            jk_continuation_register_cleanup(
                handle,
                CONTINUATION_SUSPEND_RESULT_STORAGE,
                0,
                drop_owned_result as *mut c_void
            ),
            1
        );
        assert_eq!(jk_continuation_begin_function_pending(handle), 1);
        let bytes = (payload as usize).to_ne_bytes();
        assert_eq!(
            jk_continuation_complete_function_pending(handle, bytes.as_ptr(), bytes.len()),
            1
        );
        assert_eq!(
            jk_continuation_poll_function_pending(handle),
            FunctionCallStatus::Ready as u8
        );
        let mut taken = std::ptr::null_mut::<OwnedResult>();
        assert_eq!(
            jk_continuation_take_function_pending_result(
                handle,
                (&mut taken as *mut *mut OwnedResult).cast(),
                std::mem::size_of_val(&taken),
            ),
            FunctionCallStatus::Ready as u8
        );
        assert_eq!(taken, payload);
        assert_eq!(std::ptr::read_unaligned(slot.cast::<usize>()), 0);
        assert_eq!(jk_continuation_cancel(handle), 1);
        jk_continuation_free(handle);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        drop(Box::from_raw(payload));
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}

#[test]
fn function_pending_rejects_invalid_payload_without_consuming_completion() {
    let call = Call::new();
    let short = [0_u8; 4];
    assert_eq!(
        unsafe { jk_continuation_complete_function_pending(call.handle, short.as_ptr(), 4) },
        0
    );
    assert_eq!(
        unsafe { jk_continuation_complete_function_pending(call.handle, std::ptr::null(), 8) },
        0
    );
    assert_eq!(call.complete(42), 1);
    assert_eq!(call.poll(), FunctionCallStatus::Ready as u8);
}

#[test]
fn function_pending_cancellation_rejects_late_completion_and_handoff() {
    let call = Call::new();
    assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
    assert_eq!(unsafe { jk_continuation_cancel(call.handle) }, 1);
    assert_eq!(call.complete(42), 0);
    assert_eq!(call.poll(), FunctionCallStatus::Cancelled as u8);
    assert_eq!(call.publish(), 0);
    assert_eq!(call.probe.hits.load(Ordering::Acquire), 0);
    assert!(unsafe { jk_continuation_suspend_result_pointer(call.handle) }.is_null());
}

#[test]
fn function_pending_unit_and_retired_handle_protocol() {
    let handle = jk_continuation_new(12);
    unsafe {
        assert_eq!(jk_continuation_begin_function_pending(handle), 1);
        assert_eq!(
            jk_continuation_complete_function_pending(handle, std::ptr::null(), 0),
            1
        );
        assert_eq!(
            jk_continuation_poll_function_pending(handle),
            FunctionCallStatus::Ready as u8
        );
        assert_eq!(
            jk_continuation_take_function_pending_result(handle, std::ptr::null_mut(), 0),
            FunctionCallStatus::Ready as u8
        );
        assert_eq!(
            jk_continuation_take_function_pending_result(handle, std::ptr::null_mut(), 0),
            FunctionCallStatus::Failed as u8
        );
        jk_continuation_free(handle);
        assert_eq!(
            jk_continuation_complete_function_pending(handle, std::ptr::null(), 0),
            0
        );
        assert_eq!(jk_continuation_publish_function_pending(handle), 0);
        assert_eq!(
            jk_continuation_take_function_pending_result(handle, std::ptr::null_mut(), 0),
            FunctionCallStatus::Cancelled as u8
        );
        assert_eq!(
            jk_continuation_poll_function_pending(handle),
            FunctionCallStatus::Cancelled as u8
        );
    }
}

#[test]
fn function_pending_scope_close_cancels_an_unpublished_call() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let call = Call::new();
    assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
    scope.close_and_wait();
    assert_eq!(call.poll(), FunctionCallStatus::Cancelled as u8);
    assert_eq!(call.publish(), 0);
    assert_eq!(call.complete(42), 0);
}

#[test]
fn function_pending_take_validates_state_and_moves_exactly_once() {
    let call = Call::new();
    let mut output = 99_u64;
    let pointer = (&mut output as *mut u64).cast();
    unsafe {
        assert_eq!(
            jk_continuation_take_function_pending_result(call.handle, pointer, 8),
            FunctionCallStatus::Failed as u8
        );
        assert_eq!(call.complete(42), 1);
        assert_eq!(
            jk_continuation_take_function_pending_result(call.handle, pointer, 8),
            FunctionCallStatus::Failed as u8
        );
        assert_eq!(call.poll(), FunctionCallStatus::Ready as u8);
        assert_eq!(
            jk_continuation_take_function_pending_result(call.handle, pointer, 4),
            FunctionCallStatus::Failed as u8
        );
        assert_eq!(
            jk_continuation_take_function_pending_result(call.handle, std::ptr::null_mut(), 8),
            FunctionCallStatus::Failed as u8
        );
        assert_eq!(output, 99);
        assert_eq!(
            jk_continuation_take_function_pending_result(call.handle, pointer, 8),
            FunctionCallStatus::Ready as u8
        );
        assert_eq!(output, 42);
        output = 99;
        assert_eq!(
            jk_continuation_take_function_pending_result(call.handle, pointer, 8),
            FunctionCallStatus::Failed as u8
        );
        assert_eq!(output, 99);
    }
}

#[test]
fn function_pending_take_racing_cancellation_transfers_or_drops_once() {
    for _ in 0..64 {
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = Box::into_raw(Box::new(OwnedResult(Arc::clone(&drops))));
        let handle = jk_continuation_new(0);
        unsafe {
            jk_continuation_alloc_suspend_result(handle, std::mem::size_of::<usize>());
            jk_continuation_register_cleanup(
                handle,
                CONTINUATION_SUSPEND_RESULT_STORAGE,
                0,
                drop_owned_result as *mut c_void,
            );
            assert_eq!(jk_continuation_begin_function_pending(handle), 1);
            let bytes = (payload as usize).to_ne_bytes();
            assert_eq!(
                jk_continuation_complete_function_pending(handle, bytes.as_ptr(), bytes.len()),
                1
            );
            assert_eq!(
                jk_continuation_poll_function_pending(handle),
                FunctionCallStatus::Ready as u8
            );
        }
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let address = handle as usize;
        let worker = std::thread::spawn(move || {
            let mut output = 0_usize;
            worker_barrier.wait();
            let status = unsafe {
                jk_continuation_take_function_pending_result(
                    address as *mut Continuation,
                    (&mut output as *mut usize).cast(),
                    std::mem::size_of_val(&output),
                )
            };
            (status, output)
        });
        barrier.wait();
        assert_eq!(unsafe { jk_continuation_cancel(handle) }, 1);
        let (status, output) = worker.join().unwrap();
        if status == FunctionCallStatus::Ready as u8 {
            assert_eq!(output, payload as usize);
            assert_eq!(drops.load(Ordering::Acquire), 0);
            unsafe { drop(Box::from_raw(output as *mut OwnedResult)) };
        } else {
            assert_eq!(status, FunctionCallStatus::Cancelled as u8);
            assert_eq!(output, 0);
        }
        unsafe { jk_continuation_free(handle) };
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}

#[test]
fn function_pending_boundary_publishes_after_the_native_chain_returns() {
    let outer = Call::new();
    let middle = Call::new();
    let leaf = Call::new();
    leaf.probe
        .parent
        .store(middle.handle as usize, Ordering::Release);
    middle
        .probe
        .parent
        .store(outer.handle as usize, Ordering::Release);
    let status = with_function_pending_boundary(|| {
        let nested = with_function_pending_boundary(|| {
            assert_eq!(leaf.poll(), FunctionCallStatus::Pending as u8);
            assert_eq!(leaf.complete(40), 1);
            FunctionCallStatus::Pending
        });
        assert_eq!(nested, FunctionCallStatus::Pending);
        assert_eq!(middle.poll(), FunctionCallStatus::Pending as u8);
        assert_eq!(outer.poll(), FunctionCallStatus::Pending as u8);
        assert_eq!(leaf.publish(), 0, "native caller chain is still active");
        leaf.no_resume();
        middle.no_resume();
        outer.no_resume();
        FunctionCallStatus::Pending
    });
    assert_eq!(status, FunctionCallStatus::Pending);
    outer.resumed(42);
    middle.resumed(41);
    leaf.resumed(40);
}

#[test]
fn function_pending_boundary_returns_before_provider_completion() {
    let call = Call::new();
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
            FunctionCallStatus::Pending
        }),
        FunctionCallStatus::Pending
    );
    call.no_resume();
    assert_eq!(call.complete(42), 1);
    call.resumed(42);
}

#[test]
fn function_pending_boundary_ready_and_unwind_do_not_schedule() {
    let ready = Call::new();
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(ready.complete(42), 1);
            assert_eq!(ready.poll(), FunctionCallStatus::Ready as u8);
            FunctionCallStatus::Ready
        }),
        FunctionCallStatus::Ready
    );
    assert_eq!(ready.probe.hits.load(Ordering::Acquire), 0);

    let pending = Call::new();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_function_pending_boundary(|| {
            assert_eq!(pending.poll(), FunctionCallStatus::Pending as u8);
            panic!("native invocation failed");
        });
    }))
    .is_err());
    assert_eq!(pending.poll(), FunctionCallStatus::Cancelled as u8);
    assert_eq!(pending.probe.hits.load(Ordering::Acquire), 0);
    // The failed boundary must restore TLS for the next invocation.
    assert_eq!(
        with_function_pending_boundary(|| FunctionCallStatus::Ready),
        FunctionCallStatus::Ready
    );
}

#[test]
fn function_pending_boundary_rejects_a_ready_root_with_pending_children() {
    let call = Call::new();
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
            FunctionCallStatus::Ready
        }),
        FunctionCallStatus::Failed
    );
    assert_eq!(call.poll(), FunctionCallStatus::Cancelled as u8);
    assert_eq!(call.probe.hits.load(Ordering::Acquire), 0);
    assert_eq!(
        with_function_pending_boundary(|| FunctionCallStatus::Pending),
        FunctionCallStatus::Failed
    );
}

#[test]
fn function_pending_boundary_handles_cancellation_before_handoff() {
    let call = Call::new();
    let caller = Call::new();
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(call.poll(), FunctionCallStatus::Pending as u8);
            assert_eq!(caller.poll(), FunctionCallStatus::Pending as u8);
            assert_eq!(unsafe { jk_continuation_cancel(call.handle) }, 1);
            FunctionCallStatus::Pending
        }),
        FunctionCallStatus::Cancelled
    );
    assert_eq!(call.complete(42), 0);
    assert_eq!(caller.poll(), FunctionCallStatus::Cancelled as u8);
    assert_eq!(call.probe.hits.load(Ordering::Acquire), 0);
}

unsafe extern "C" fn immediate_pending_provider(
    handle: *mut Continuation,
    operation: u64,
    _arguments: *const u8,
    _arguments_size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    let payload = 40_u64.to_ne_bytes();
    unsafe {
        jk_continuation_complete_suspend_with_payload(
            handle,
            operation,
            payload.as_ptr(),
            payload.len(),
        )
    }
}

unsafe extern "C" fn deferred_pending_provider(
    _handle: *mut Continuation,
    _operation: u64,
    _arguments: *const u8,
    _arguments_size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    1
}

unsafe extern "C" fn resume_final_result(handle: *mut Continuation) {
    unsafe {
        let probe = &*std::ptr::read_unaligned(
            jk_continuation_frame_pointer(handle).cast::<*const Probe>(),
        );
        let sender = probe.result.clone();
        probe.hits.fetch_add(1, Ordering::AcqRel);
        // The generated machine entry writes its final result before calling
        // complete. This test preinitializes that slot with a managed value.
        assert_eq!(jk_continuation_complete(handle), 1);
        sender.send(1).unwrap();
    }
}

#[test]
fn function_pending_operation_transfers_final_result_or_cancels_child_payload() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OPERATION: u64 = 0xfeed_2007;
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                deferred_pending_provider as *mut c_void,
                std::ptr::null_mut(),
            )
        },
        1
    );
    for reject in [false, true] {
        let parent = Call::new();
        let leaf = Call::allocate(resume_final_result);
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = Box::into_raw(Box::new(OwnedResult(Arc::clone(&drops))));
        unsafe {
            let slot = jk_continuation_alloc_result(leaf.handle, std::mem::size_of::<usize>());
            std::ptr::write_unaligned(slot.cast::<*mut OwnedResult>(), payload);
            for (handle, storage) in [
                (leaf.handle, CONTINUATION_RESULT_STORAGE),
                (parent.handle, CONTINUATION_SUSPEND_RESULT_STORAGE),
            ] {
                assert_eq!(
                    jk_continuation_register_cleanup(
                        handle,
                        storage,
                        0,
                        drop_owned_result as *mut c_void,
                    ),
                    1
                );
            }
        }
        assert_eq!(
            with_function_pending_boundary(|| {
                assert_eq!(
                    unsafe {
                        jk_continuation_start_pending_provider(
                            leaf.handle,
                            OPERATION,
                            std::ptr::null(),
                            0,
                            parent.handle,
                        )
                    },
                    FunctionCallStatus::Pending as u8
                );
                FunctionCallStatus::Pending
            }),
            FunctionCallStatus::Pending
        );
        if reject {
            assert_eq!(unsafe { jk_continuation_cancel(parent.handle) }, 1);
            assert_eq!(
                unsafe { jk_continuation_state(leaf.handle) },
                ContinuationState::Cancelled as u8
            );
            assert_eq!(drops.load(Ordering::Acquire), 1);
        }
        let operation_result = 0_u64.to_ne_bytes();
        assert_eq!(
            unsafe {
                jk_continuation_complete_suspend_with_payload(
                    leaf.handle,
                    OPERATION,
                    operation_result.as_ptr(),
                    operation_result.len(),
                )
            },
            if reject { 0 } else { 1 }
        );
        if !reject {
            leaf.resumed(1);
        } else {
            assert_eq!(leaf.probe.hits.load(Ordering::Acquire), 0);
        }
        assert!(unsafe { jk_continuation_result_pointer(leaf.handle) }.is_null());
        if !reject {
            assert_eq!(drops.load(Ordering::Acquire), 0);
            assert_eq!(parent.poll(), FunctionCallStatus::Ready as u8);
            let mut taken = std::ptr::null_mut::<OwnedResult>();
            assert_eq!(
                unsafe {
                    jk_continuation_take_function_pending_result(
                        parent.handle,
                        (&mut taken as *mut *mut OwnedResult).cast(),
                        std::mem::size_of_val(&taken),
                    )
                },
                FunctionCallStatus::Ready as u8
            );
            assert_eq!(taken, payload);
            unsafe { drop(Box::from_raw(taken)) };
        }
        drop(leaf);
        drop(parent);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}

#[test]
fn function_pending_provider_completion_resumes_a_three_level_chain() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OPERATION: u64 = 0xfeed_2001;
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                immediate_pending_provider as *mut c_void,
                std::ptr::null_mut(),
            )
        },
        1
    );
    let outer = Call::new();
    let middle = Call::new();
    let leaf = Call::operation();
    leaf.probe
        .parent
        .store(middle.handle as usize, Ordering::Release);
    middle
        .probe
        .parent
        .store(outer.handle as usize, Ordering::Release);
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(
                unsafe {
                    jk_continuation_start_pending_provider(
                        leaf.handle,
                        OPERATION,
                        std::ptr::null(),
                        0,
                        std::ptr::null_mut(),
                    )
                },
                FunctionCallStatus::Pending as u8
            );
            assert_eq!(middle.poll(), FunctionCallStatus::Pending as u8);
            assert_eq!(outer.poll(), FunctionCallStatus::Pending as u8);
            assert_eq!(leaf.probe.hits.load(Ordering::Acquire), 0);
            middle.no_resume();
            outer.no_resume();
            FunctionCallStatus::Pending
        }),
        FunctionCallStatus::Pending
    );
    outer.resumed(42);
    middle.resumed(41);
    leaf.resumed(40);
}

#[test]
fn function_pending_provider_returns_pending_before_completion() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OPERATION: u64 = 0xfeed_2002;
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                deferred_pending_provider as *mut c_void,
                std::ptr::null_mut(),
            )
        },
        1
    );
    let leaf = Call::operation();
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(
                unsafe {
                    jk_continuation_start_pending_provider(
                        leaf.handle,
                        OPERATION,
                        std::ptr::null(),
                        0,
                        std::ptr::null_mut(),
                    )
                },
                FunctionCallStatus::Pending as u8
            );
            FunctionCallStatus::Pending
        }),
        FunctionCallStatus::Pending
    );
    leaf.no_resume();
    let address = leaf.handle as usize;
    let worker = std::thread::spawn(move || {
        let payload = 42_u64.to_ne_bytes();
        unsafe {
            jk_continuation_complete_suspend_with_payload(
                address as *mut Continuation,
                OPERATION,
                payload.as_ptr(),
                payload.len(),
            )
        }
    });
    assert_eq!(worker.join().unwrap(), 1);
    leaf.resumed(42);
}

#[test]
fn function_pending_timer_completion_waits_for_native_return() {
    let leaf = Call::operation();
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(
                unsafe {
                    jk_continuation_start_pending_timer(
                        leaf.handle,
                        0xfeed_2003,
                        1,
                        std::ptr::null_mut(),
                    )
                },
                FunctionCallStatus::Pending as u8
            );
            // The timer completion may only enqueue a resume; while this
            // native frame is still inside the boundary the callback must
            // not have dispatched the leaf yet.
            let deadline = std::time::Instant::now() + Duration::from_millis(50);
            while std::time::Instant::now() < deadline {
                if leaf.probe.hits.load(Ordering::Acquire) != 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(leaf.probe.hits.load(Ordering::Acquire), 0);
            FunctionCallStatus::Pending
        }),
        FunctionCallStatus::Pending
    );
    leaf.resumed(0);
}

#[test]
fn function_pending_operation_requires_a_boundary_and_known_provider() {
    let leaf = Call::operation();
    assert_eq!(
        unsafe { jk_continuation_start_pending_timer(leaf.handle, 123, 1, std::ptr::null_mut()) },
        FunctionCallStatus::Failed as u8
    );
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(
                unsafe {
                    jk_continuation_start_pending_provider(
                        leaf.handle,
                        0xfeed_2004,
                        std::ptr::null(),
                        0,
                        std::ptr::null_mut(),
                    )
                },
                FunctionCallStatus::Failed as u8
            );
            FunctionCallStatus::Failed
        }),
        FunctionCallStatus::Failed
    );
    assert_eq!(leaf.probe.hits.load(Ordering::Acquire), 0);
}

#[test]
fn function_pending_handoff_lets_the_worker_run_other_work() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OPERATION: u64 = 0xfeed_2006;
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                deferred_pending_provider as *mut c_void,
                std::ptr::null_mut(),
            )
        },
        1
    );
    // A caller activation waiting on a suspending callee whose own suspend
    // operation is still pending: exactly the nested-call machine entry shape.
    let caller = Call::new();
    let leaf = Call::operation();
    leaf.probe
        .parent
        .store(caller.handle as usize, Ordering::Release);
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(
                unsafe {
                    jk_continuation_start_pending_provider(
                        leaf.handle,
                        OPERATION,
                        std::ptr::null(),
                        0,
                        caller.handle,
                    )
                },
                FunctionCallStatus::Pending as u8
            );
            assert_eq!(caller.poll(), FunctionCallStatus::Pending as u8);
            caller.no_resume();
            leaf.no_resume();
            FunctionCallStatus::Pending
        }),
        FunctionCallStatus::Pending
    );
    // Both activations handed off: the caller's native frame has returned, so
    // a worker can immediately run unrelated queued work.
    let (ran, receiver) = mpsc::channel::<()>();
    crate::runtime::task::enqueue_scheduler_callback(Box::new(move || {
        let _ = ran.send(());
    }));
    receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(caller.probe.hits.load(Ordering::Acquire), 0);
    assert_eq!(leaf.probe.hits.load(Ordering::Acquire), 0);
    // The callee completes late on another thread; the caller resumes once.
    let address = leaf.handle as usize;
    let worker = std::thread::spawn(move || {
        let payload = 42_u64.to_ne_bytes();
        unsafe {
            jk_continuation_complete_suspend_with_payload(
                address as *mut Continuation,
                OPERATION,
                payload.as_ptr(),
                payload.len(),
            )
        }
    });
    assert_eq!(worker.join().unwrap(), 1);
    leaf.resumed(42);
    caller.resumed(43);
}

#[test]
fn function_pending_scope_cancels_a_provider_before_native_handoff() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OPERATION: u64 = 0xfeed_2005;
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                deferred_pending_provider as *mut c_void,
                std::ptr::null_mut(),
            )
        },
        1
    );
    let leaf = Call::operation();
    assert_eq!(
        with_function_pending_boundary(|| {
            assert_eq!(
                unsafe {
                    jk_continuation_start_pending_provider(
                        leaf.handle,
                        OPERATION,
                        std::ptr::null(),
                        0,
                        std::ptr::null_mut(),
                    )
                },
                FunctionCallStatus::Pending as u8
            );
            scope.close_and_wait();
            FunctionCallStatus::Pending
        }),
        FunctionCallStatus::Cancelled
    );
    assert_eq!(leaf.probe.hits.load(Ordering::Acquire), 0);
    let payload = 42_u64.to_ne_bytes();
    assert_eq!(
        unsafe {
            jk_continuation_complete_suspend_with_payload(
                leaf.handle,
                OPERATION,
                payload.as_ptr(),
                payload.len(),
            )
        },
        0
    );
}

unsafe extern "C" fn held_pending_provider(
    _handle: *mut Continuation,
    _operation: u64,
    arguments: *const u8,
    _arguments_size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    let gate = unsafe { &*std::ptr::read_unaligned(arguments.cast::<*const Barrier>()) };
    gate.wait();
    gate.wait();
    1
}

#[test]
fn function_pending_cancellation_keeps_provider_start_buffers_live() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OPERATION: u64 = 0xfeed_2006;
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                held_pending_provider as *mut c_void,
                std::ptr::null_mut(),
            )
        },
        1
    );
    let leaf = Call::operation();
    let gate = Arc::new(Barrier::new(2));
    let worker_gate = Arc::clone(&gate);
    let worker_scope = Arc::clone(&scope);
    let address = leaf.handle as usize;
    let worker = std::thread::spawn(move || {
        let _guard = worker_scope.enter();
        with_function_pending_boundary(|| {
            let arguments = (Arc::as_ptr(&worker_gate) as usize).to_ne_bytes();
            assert_eq!(
                unsafe {
                    jk_continuation_start_pending_provider(
                        address as *mut Continuation,
                        OPERATION,
                        arguments.as_ptr(),
                        arguments.len(),
                        std::ptr::null_mut(),
                    )
                },
                FunctionCallStatus::Cancelled as u8
            );
            FunctionCallStatus::Cancelled
        })
    });
    gate.wait();
    assert_eq!(unsafe { jk_continuation_cancel(leaf.handle) }, 1);
    assert!(!unsafe { jk_continuation_suspend_result_pointer(leaf.handle) }.is_null());
    assert!(!unsafe { jk_continuation_suspend_arguments_pointer(leaf.handle) }.is_null());
    gate.wait();
    assert_eq!(worker.join().unwrap(), FunctionCallStatus::Cancelled);
    assert!(unsafe { jk_continuation_suspend_result_pointer(leaf.handle) }.is_null());
    assert!(unsafe { jk_continuation_suspend_arguments_pointer(leaf.handle) }.is_null());
    assert_eq!(leaf.probe.hits.load(Ordering::Acquire), 0);
}

#[test]
fn retired_function_activation_does_not_block_parent_cancellation() {
    let parent = Call::new();
    let previous = Call::operation();
    let successor = Call::new();
    assert!(previous
        .continuation()
        .attach_function_parent(previous.handle, parent.handle));
    assert!(successor
        .continuation()
        .attach_function_parent(successor.handle, parent.handle));
    assert!(previous.continuation().resume());
    unsafe {
        jk_continuation_retire_function_activation(previous.handle, successor.handle);
    }
    assert_eq!(parent.children_count(), 1);
    assert!(parent.continuation().cancel());
    parent.continuation().wait_for_idle();
    assert_eq!(parent.children_count(), 0);
    assert!(parent.continuation().frame_pointer().is_null());
}

#[test]
fn failed_resumed_call_releases_an_unpublished_activation() {
    let parent = Call::new();
    assert!(parent.continuation().cancel());
    let successor = Call::operation();
    assert_eq!(
        unsafe {
            jk_continuation_begin_function_pending_with_parent(successor.handle, parent.handle)
        },
        0
    );
    // The generated resumed entry saved its live values but returned a terminal
    // status before the fresh activation could enter the Pending handoff list.
    unsafe {
        jk_continuation_fail_function_chain(successor.handle, FunctionCallStatus::Cancelled as u8);
    }
    assert!(unsafe { retain_handle(successor.handle) }.is_none());
}
