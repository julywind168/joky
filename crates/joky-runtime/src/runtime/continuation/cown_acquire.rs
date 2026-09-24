//! Cown acquisition as an internal Pending operation. Notifications only make
//! the activation runnable; it retries the entire set on its own task context.
use super::*;
use crate::runtime::abi::FunctionCallStatus;
use crate::runtime::cown::Waiter;
use crate::runtime::managed::{cown_lease, try_cown_acquire_many};

pub(super) struct CownAcquisition {
    handles: Vec<usize>,
    registration: Option<(usize, Arc<Waiter>)>,
    condition: Option<Arc<Waiter>>,
}

impl Drop for CownAcquisition {
    fn drop(&mut self) {
        if let Some(waiter) = self.condition.take() {
            for &cown in &self.handles {
                unsafe { cown_lease(cown as *mut u8).unregister_condition(&waiter) };
            }
        }
        if let Some((cown, waiter)) = self.registration.take() {
            // The continuation's scope work and region outlive deregistration.
            unsafe { cown_lease(cown as *mut u8).unregister(&waiter, true) };
        }
    }
}

impl Continuation {
    pub(super) fn internal_wait_active(&self) -> bool {
        self.inner.task_wait_active.load(Ordering::Acquire)
            || self.inner.cown_acquire_active.load(Ordering::Acquire)
    }

    pub(super) fn cancel_cown_acquisition(&self) {
        let acquisition = self
            .inner
            .cown_acquisition
            .lock()
            .expect("Cown acquisition")
            .take();
        self.inner
            .cown_acquire_active
            .store(false, Ordering::Release);
        drop(acquisition);
    }

    fn notify_cown_acquisition(&self, identity: usize) {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Sleeping {
            return;
        }
        let matches = self
            .inner
            .cown_acquisition
            .lock()
            .expect("Cown acquisition")
            .as_ref()
            .is_some_and(|acquisition| {
                acquisition
                    .registration
                    .as_ref()
                    .is_some_and(|(_, w)| Arc::as_ptr(w) as usize == identity)
                    || acquisition
                        .condition
                        .as_ref()
                        .is_some_and(|w| Arc::as_ptr(w) as usize == identity)
            });
        if !matches {
            return;
        }
        *state = ContinuationState::Ready;
        drop(state);
        self.enqueue_resume(self.inner.handle.load(Ordering::Acquire) as *mut Continuation);
    }

    fn arm_cown_acquisition(
        &self,
        handle: *mut Continuation,
        blocked: usize,
    ) -> FunctionCallStatus {
        let weak = Arc::downgrade(&self.inner);
        let waiter = Waiter::for_continuation(move |identity| {
            if let Some(inner) = weak.upgrade() {
                Continuation { inner }.notify_cown_acquisition(identity);
            }
        });
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if !self.begin_scope_work() {
            drop(state);
            return self.finish_operation_start(handle, false);
        }
        self.inner
            .cown_acquire_active
            .store(true, Ordering::Release);
        self.inner.task_wait_active.store(false, Ordering::Release);
        *state = ContinuationState::Sleeping;
        self.remember_owner_handle(handle);
        self.inner.handler_frame.store(
            crate::runtime::handler::current() as usize,
            Ordering::Release,
        );
        self.inner.handle.store(handle as usize, Ordering::Release);
        self.inner.active_operation.store(0, Ordering::Release);
        self.inner
            .cown_acquisition
            .lock()
            .expect("Cown acquisition")
            .as_mut()
            .expect("active Cown acquisition")
            .registration = Some((blocked, Arc::clone(&waiter)));
        // Publish identity before queue insertion, under the state lock. Release
        // drops its queue lock before notification, so there is no lock inversion.
        let queued = unsafe { cown_lease(blocked as *mut u8).register(&waiter) };
        drop(state);
        if !queued {
            waiter.notify();
        }
        // Even cancellation during start must cross the native handoff before
        // reclaiming saved locals. The publication/cancellation path owns it.
        self.inner
            .native_operation_active
            .store(false, Ordering::Release);
        function_calls::defer_pending_operation(handle);
        FunctionCallStatus::Pending
    }

    /// Runs with machine_entry_active and the owning task's dynamic context.
    /// A successful attempt enters the body immediately on this same stack.
    pub(super) fn resume_cown_acquisition(&self, handle: *mut Continuation) -> bool {
        if !self.inner.cown_acquire_active.load(Ordering::Acquire) {
            return true;
        }
        let (handles, registration, condition) = {
            let mut acquisition = self
                .inner
                .cown_acquisition
                .lock()
                .expect("Cown acquisition");
            let Some(acquisition) = acquisition.as_mut() else {
                return false;
            };
            (
                acquisition
                    .handles
                    .iter()
                    .map(|&h| h as *mut u8)
                    .collect::<Vec<_>>(),
                acquisition.registration.take(),
                acquisition.condition.take(),
            )
        };
        if let Some(waiter) = condition {
            for &cown in &handles {
                unsafe { cown_lease(cown).unregister_condition(&waiter) };
            }
        }
        let previous_blocker = registration.as_ref().map(|(cown, _)| *cown);
        if let Some((cown, waiter)) = registration {
            unsafe { cown_lease(cown as *mut u8).unregister(&waiter, false) };
        }
        match try_cown_acquire_many(&handles) {
            Ok(()) => {
                self.cancel_cown_acquisition(); // No registered waiter or held capability remains here.
                true
            }
            Err(blocked) => {
                // The selected Cown may remain free when an earlier member
                // now blocks this batch. Pass its notification on as well.
                if let Some(previous) = previous_blocker {
                    unsafe { cown_lease(previous as *mut u8).notify_if_available() };
                }
                let parent =
                    self.inner.function_parent.load(Ordering::Acquire) as *mut Continuation;
                let status = if blocked.is_null() {
                    FunctionCallStatus::Failed
                } else if !self.withhold_operation_resume(parent) {
                    // Cancellation can arrive after this waiter was selected.
                    unsafe { cown_lease(blocked).notify_if_available() };
                    FunctionCallStatus::Cancelled
                } else {
                    self.arm_cown_acquisition(handle, blocked as usize)
                };
                if status != FunctionCallStatus::Pending {
                    unsafe { jk_continuation_fail_function_chain(handle, status as u8) };
                }
                false
            }
        }
    }
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_start_cown_acquire(
    handle: *mut Continuation,
    cowns: *const *mut u8,
    count: usize,
    parent: *mut Continuation,
) -> u8 {
    let Some(value) = (unsafe { retain_handle(handle) }) else {
        return FunctionCallStatus::Cancelled as u8;
    };
    if cowns.is_null() || count == 0 {
        return FunctionCallStatus::Failed as u8;
    }
    if !value.attach_function_parent(handle, parent) || !value.withhold_operation_resume(parent) {
        return FunctionCallStatus::Cancelled as u8;
    }
    // The generated fast attempt validated these handles. Keep their region
    // capabilities across Pending, but never acquire a lease on the start path.
    let handles = unsafe { std::slice::from_raw_parts(cowns, count) };
    let blocked = handles
        .iter()
        .copied()
        .find(|&h| unsafe { cown_lease(h).is_leased() })
        .unwrap_or(handles[0]);
    *value
        .inner
        .cown_acquisition
        .lock()
        .expect("Cown acquisition") = Some(CownAcquisition {
        handles: handles.iter().map(|&h| h as usize).collect(),
        registration: None,
        condition: None,
    });
    value.arm_cown_acquisition(handle, blocked as usize) as u8
}

/// The guard has just evaluated false while holding every lease. Publish all
/// registrations before releasing any lease, then suspend through the normal
/// Pending handoff. A later completed when wakes us; resume reacquires the set.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_start_cown_wait(
    handle: *mut Continuation,
    cowns: *const *mut u8,
    count: usize,
    parent: *mut Continuation,
) -> u8 {
    let Some(value) = (unsafe { retain_handle(handle) }) else {
        return FunctionCallStatus::Cancelled as u8;
    };
    if cowns.is_null() || count == 0 {
        return FunctionCallStatus::Failed as u8;
    }
    if !value.attach_function_parent(handle, parent) || !value.withhold_operation_resume(parent) {
        return FunctionCallStatus::Cancelled as u8;
    }
    let handles = unsafe { std::slice::from_raw_parts(cowns, count) };
    let weak = Arc::downgrade(&value.inner);
    let waiter = Waiter::for_continuation(move |identity| {
        if let Some(inner) = weak.upgrade() {
            Continuation { inner }.notify_cown_acquisition(identity);
        }
    });
    let mut state = value.inner.state.lock().expect("continuation state mutex");
    if !value.begin_scope_work() {
        drop(state);
        return value.finish_operation_start(handle, false) as u8;
    }
    value
        .inner
        .cown_acquire_active
        .store(true, Ordering::Release);
    value.inner.task_wait_active.store(false, Ordering::Release);
    *state = ContinuationState::Sleeping;
    value.remember_owner_handle(handle);
    value.inner.handler_frame.store(
        crate::runtime::handler::current() as usize,
        Ordering::Release,
    );
    value.inner.handle.store(handle as usize, Ordering::Release);
    value.inner.active_operation.store(0, Ordering::Release);
    *value
        .inner
        .cown_acquisition
        .lock()
        .expect("Cown acquisition") = Some(CownAcquisition {
        handles: handles.iter().map(|&h| h as usize).collect(),
        registration: None,
        condition: Some(Arc::clone(&waiter)),
    });
    for &cown in handles {
        unsafe { cown_lease(cown).register_condition(&waiter) };
    }
    for &cown in handles {
        crate::runtime::managed::release_cown_for_condition(cown);
    }
    drop(state);
    value
        .inner
        .native_operation_active
        .store(false, Ordering::Release);
    function_calls::defer_pending_operation(handle);
    FunctionCallStatus::Pending as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::managed::*;
    use std::sync::mpsc;

    unsafe extern "C" fn release_and_complete(handle: *mut Continuation) {
        let value = unsafe { retain_handle(handle) }.unwrap();
        let frame = value.frame_pointer().cast::<usize>();
        let count = unsafe { *frame };
        for index in 0..count {
            jk_cown_release(unsafe { *frame.add(index + 1) } as *mut u8);
        }
        assert!(value.complete());
    }

    unsafe extern "C" fn must_not_resume(_: *mut Continuation) {
        panic!("cancelled Cown acquisition entered its body");
    }

    fn new_cown() -> *mut u8 {
        jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None)
    }

    fn activation(
        handles: &[*mut u8],
        entry: unsafe extern "C" fn(*mut Continuation),
    ) -> (*mut Continuation, Continuation) {
        let handle = jk_continuation_new(1);
        let value = unsafe { retain_handle(handle) }.unwrap();
        unsafe {
            let frame =
                jk_continuation_alloc_frame(handle, (handles.len() + 1) * 8).cast::<usize>();
            *frame = handles.len();
            for (index, &cown) in handles.iter().enumerate() {
                *frame.add(index + 1) = cown as usize;
            }
            jk_continuation_set_machine_entry(handle, entry as *mut c_void);
        }
        (handle, value)
    }

    fn start(handle: *mut Continuation, handles: &[*mut u8]) -> FunctionCallStatus {
        assert_eq!(
            unsafe { jk_cown_try_acquire_many(handles.as_ptr(), handles.len()) },
            FunctionCallStatus::Pending as u8
        );
        assert_eq!(
            unsafe {
                jk_continuation_start_cown_acquire(
                    handle,
                    handles.as_ptr(),
                    handles.len(),
                    std::ptr::null_mut(),
                )
            },
            FunctionCallStatus::Pending as u8
        );
        FunctionCallStatus::Pending
    }

    fn until(ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < deadline, "Cown Pending did not finish");
            std::thread::yield_now();
        }
    }

    #[test]
    fn condition_wait_cancellation_unregisters_every_cown_before_teardown() {
        for (before_handoff, notify_early) in [(false, false), (true, false), (true, true)] {
            let scope = RuntimeScope::new();
            let _scope = scope.enter();
            let cowns = [new_cown(), new_cown()];
            assert!(try_cown_acquire_many(&cowns).is_ok());
            let (handle, value) = activation(&cowns, must_not_resume);
            with_function_pending_boundary(|| {
                assert_eq!(
                    unsafe {
                        jk_continuation_start_cown_wait(
                            handle,
                            cowns.as_ptr(),
                            cowns.len(),
                            std::ptr::null_mut(),
                        )
                    },
                    FunctionCallStatus::Pending as u8
                );
                for &cown in &cowns {
                    assert!(!unsafe { cown_lease(cown).is_leased() });
                    assert_eq!(unsafe { cown_lease(cown).condition_waiter_count() }, 1);
                }
                if notify_early {
                    assert!(try_cown_acquire_many(&cowns[..1]).is_ok());
                    jk_cown_release(cowns[0]);
                }
                if before_handoff {
                    assert!(value.cancel());
                }
                FunctionCallStatus::Pending
            });
            if !before_handoff {
                value.cancel();
            }
            until(|| value.state() == ContinuationState::Cancelled);
            for &cown in &cowns {
                assert_eq!(unsafe { cown_lease(cown).condition_waiter_count() }, 0);
            }
            assert!(try_cown_acquire_many(&cowns).is_ok());
            for &cown in &cowns {
                jk_cown_release(cown);
            }
            unsafe { jk_continuation_free(handle) };
            scope.close_and_wait();
        }
    }

    #[test]
    fn condition_notification_before_handoff_reacquires_the_whole_set() {
        let scope = RuntimeScope::new();
        let _scope = scope.enter();
        let cowns = [new_cown(), new_cown()];
        assert!(try_cown_acquire_many(&cowns).is_ok());
        let (handle, value) = activation(&cowns, release_and_complete);
        with_function_pending_boundary(|| {
            assert_eq!(
                unsafe {
                    jk_continuation_start_cown_wait(
                        handle,
                        cowns.as_ptr(),
                        cowns.len(),
                        std::ptr::null_mut(),
                    )
                },
                FunctionCallStatus::Pending as u8
            );
            assert!(try_cown_acquire_many(&cowns).is_ok());
            jk_cown_release(cowns[0]);
            FunctionCallStatus::Pending
        });
        until(|| cown_waiter_count(cowns[1]) == 1);
        assert!(!unsafe { cown_lease(cowns[0]).is_leased() });
        jk_cown_release(cowns[1]);
        until(|| value.state() == ContinuationState::Completed);
        for &cown in &cowns {
            assert_eq!(unsafe { cown_lease(cown).condition_waiter_count() }, 0);
        }
        unsafe { jk_continuation_free(handle) };
        scope.close_and_wait();
    }

    #[test]
    fn pending_cown_releases_worker_and_resumes_once() {
        // Also run this alone with one worker: all suspended acquisitions
        // must leave the scheduler free to execute unrelated work.
        let scope = RuntimeScope::new();
        let _scope = scope.enter();
        let cown = new_cown();
        assert!(!jk_cown_acquire(cown).is_null());
        use crate::runtime::task::{TaskContext, TaskGroup, TaskState};
        struct Capture {
            cown: usize,
            started: mpsc::Sender<(usize, Continuation)>,
        }
        unsafe extern "C-unwind" fn wait_on_cown(context: *mut TaskContext) {
            let capture = unsafe { &*((*context).captures.cast::<Capture>()) };
            let cown = capture.cown as *mut u8;
            let (handle, value) = activation(&[cown], release_and_complete);
            let status = start(handle, &[cown]);
            capture.started.send((handle as usize, value)).unwrap();
            jk_task_function_status(status as u8);
        }
        let (started, activations) = mpsc::channel();
        let capture = Capture {
            cown: cown as usize,
            started,
        };
        let mut contexts = (0..12)
            .map(|_| {
                let mut context: TaskContext = unsafe { std::mem::zeroed() };
                context.captures = (&capture as *const Capture).cast_mut().cast();
                context
            })
            .collect::<Vec<_>>();
        let group = TaskGroup::new();
        let tasks = contexts
            .iter_mut()
            .map(|context| unsafe { group.spawn(wait_on_cown, context) })
            .collect::<Vec<_>>();
        let activations = (0..12)
            .map(|_| activations.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect::<Vec<_>>();
        until(|| {
            contexts
                .iter()
                .all(|context| context.continuation_pending.load(Ordering::Acquire))
        });
        let (sender, receiver) = mpsc::channel();
        crate::runtime::task::enqueue_scheduler_callback(Box::new(move || {
            sender.send(()).unwrap()
        }));
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(cown_waiter_count(cown), 12);
        jk_cown_release(cown);
        for task in tasks {
            assert_eq!(group.join(task), Some(TaskState::Completed));
        }
        group.close();
        for (handle, value) in activations {
            until(|| value.state() == ContinuationState::Completed);
            unsafe { jk_continuation_free(handle as *mut Continuation) };
        }
        assert_eq!(cown_waiter_count(cown), 0);
        scope.close_and_wait();
    }

    #[test]
    fn cancelled_cown_wait_removes_queue_before_region_closes() {
        for (before_handoff, release_early) in [(false, false), (true, false), (true, true)] {
            let scope = RuntimeScope::new();
            let _scope = scope.enter();
            let cown = new_cown();
            assert!(!jk_cown_acquire(cown).is_null());
            let (handle, value) = activation(&[cown], must_not_resume);
            with_function_pending_boundary(|| {
                let status = start(handle, &[cown]);
                if release_early {
                    jk_cown_release(cown);
                }
                if before_handoff {
                    assert!(value.cancel());
                    assert!(!value.frame_pointer().is_null());
                }
                status
            });
            if !before_handoff {
                value.cancel();
            }
            until(|| value.state() == ContinuationState::Cancelled);
            assert_eq!(cown_waiter_count(cown), 0);
            if !release_early {
                jk_cown_release(cown);
            }
            unsafe { jk_continuation_free(handle) };
            scope.close_and_wait();
        }
    }

    #[test]
    fn resumed_multi_cown_rearms_on_a_different_blocker() {
        let scope = RuntimeScope::new();
        let _scope = scope.enter();
        let first = new_cown();
        let second = new_cown();
        assert!(!jk_cown_acquire(first).is_null());
        let (handle, value) = activation(&[first, second], release_and_complete);
        with_function_pending_boundary(|| {
            let status = start(handle, &[first, second]);
            assert!(!jk_cown_acquire(second).is_null());
            jk_cown_release(first);
            status
        });
        until(|| cown_waiter_count(second) == 1);
        // Reacquisition rolled back the first lease before rearming.
        assert!(!jk_cown_acquire(first).is_null());
        jk_cown_release(first);
        jk_cown_release(second);
        until(|| value.state() == ContinuationState::Completed);
        unsafe { jk_continuation_free(handle) };
        scope.close_and_wait();
    }

    #[test]
    fn changing_blocker_does_not_strand_the_previously_selected_cown() {
        let scope = RuntimeScope::new();
        let _scope = scope.enter();
        let first = new_cown();
        let second = new_cown();
        assert!(!jk_cown_acquire(second).is_null());
        let (batch_handle, batch) = activation(&[first, second], release_and_complete);
        let (single_handle, single) = activation(&[second], release_and_complete);
        with_function_pending_boundary(|| start(batch_handle, &[first, second]));
        with_function_pending_boundary(|| start(single_handle, &[second]));
        assert!(!jk_cown_acquire(first).is_null());
        jk_cown_release(second);
        until(|| single.state() == ContinuationState::Completed);
        until(|| cown_waiter_count(first) == 1);
        jk_cown_release(first);
        until(|| batch.state() == ContinuationState::Completed);
        unsafe {
            jk_continuation_free(single_handle);
            jk_continuation_free(batch_handle);
        }
        scope.close_and_wait();
    }

    #[test]
    fn pending_multi_cown_releases_partial_set_and_handles_early_notification() {
        let scope = RuntimeScope::new();
        let _scope = scope.enter();
        let first = new_cown();
        let second = new_cown();
        let handles = [second, first];
        assert!(!jk_cown_acquire(second).is_null());
        let (handle, value) = activation(&handles, release_and_complete);
        with_function_pending_boundary(|| {
            let status = start(handle, &handles);
            // The failed batch must leave its earlier member available.
            assert!(!jk_cown_acquire(first).is_null());
            jk_cown_release(first);
            jk_cown_release(second);
            assert_eq!(value.state(), ContinuationState::Ready);
            assert!(!value.frame_pointer().is_null());
            status
        });
        until(|| value.state() == ContinuationState::Completed);
        unsafe { jk_continuation_free(handle) };
        scope.close_and_wait();
    }
}
