//! Internal scheduler wait protocol. No operation id or user handler is involved.
use super::*;
use crate::runtime::abi::FunctionCallStatus;

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_start_task_wait(
    handle: *mut Continuation,
    group: *mut crate::runtime::task::TaskGroup,
    race: usize,
    parent: *mut Continuation,
) -> u8 {
    let Some(value) = (unsafe { retain_handle(handle) }) else {
        return FunctionCallStatus::Cancelled as u8;
    };
    if group.is_null()
        || !value.attach_function_parent(handle, parent)
        || !value.withhold_operation_resume(parent)
    {
        return FunctionCallStatus::Cancelled as u8;
    }
    let group = unsafe { Arc::clone(&(*group).inner) };
    {
        let mut state = value.inner.state.lock().expect("continuation state mutex");
        if !value.begin_scope_work() {
            drop(state);
            return value.finish_operation_start(handle, false) as u8;
        }
        value.inner.task_wait_active.store(true, Ordering::Release);
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
            .task_wait_group
            .lock()
            .expect("task wait group mutex") = Some(Arc::clone(&group));
    }
    group.register_waiter(value.clone(), race != 0);
    // Cancellation cannot tear down this frame before the group drains.
    value
        .inner
        .native_operation_active
        .store(false, Ordering::Release);
    function_calls::defer_pending_operation(handle);
    if value.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
        crate::runtime::task::enqueue_scheduler_callback(Box::new(move || group.cancel_all()));
    }
    FunctionCallStatus::Pending as u8
}

impl Continuation {
    pub(crate) fn complete_task_wait(&self) {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if self
            .inner
            .task_wait_group
            .lock()
            .expect("task wait group mutex")
            .take()
            .is_none()
        {
            return;
        }
        *state = ContinuationState::Ready;
        drop(state);
        self.enqueue_resume(self.inner.handle.load(Ordering::Acquire) as *mut Continuation);
    }
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_own_handler(
    handle: *mut Continuation,
    frame: *mut crate::runtime::handler::HandlerFrame,
) -> u8 {
    let Some(value) = (unsafe { retain_handle(handle) }) else {
        return 0;
    };
    let mut frames = value
        .inner
        .owned_handlers
        .lock()
        .expect("owned handlers mutex");
    if !frames.contains(&(frame as usize)) {
        frames.push(frame as usize);
    }
    value
        .inner
        .handler_frame
        .store(frame as usize, Ordering::Release);
    1
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_exit_handler(handle: *mut Continuation) -> u8 {
    let Some(value) = (unsafe { retain_handle(handle) }) else {
        return 0;
    };
    let frame = value
        .inner
        .owned_handlers
        .lock()
        .expect("owned handlers mutex")
        .pop();
    if let Some(frame) = frame {
        unsafe {
            crate::runtime::handler::jk_handler_frame_exit(frame as *mut _);
            value.inner.handler_frame.store(
                crate::runtime::handler::current() as usize,
                Ordering::Release,
            );
            crate::runtime::handler::jk_handler_frame_free(frame as *mut _);
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_task_wait_defers_cancellation_until_native_handoff() {
        unsafe extern "C" fn unexpected_resume(_: *mut Continuation) {
            panic!("cancelled wait must not resume user code");
        }
        for pending in [true, false] {
            let handle = jk_continuation_new(1);
            let value = unsafe { retain_handle(handle) }.unwrap();
            let mut group = crate::runtime::task::TaskGroup::new();
            unsafe {
                jk_continuation_alloc_frame(handle, 8);
                jk_continuation_set_resume_callback(handle, unexpected_resume as *mut c_void);
            }
            function_calls::with_function_pending_boundary(|| {
                assert_eq!(
                    unsafe {
                        jk_continuation_start_task_wait(handle, &mut group, 0, std::ptr::null_mut())
                    },
                    FunctionCallStatus::Pending as u8
                );
                assert!(value.cancel());
                assert!(
                    !value.frame_pointer().is_null(),
                    "native frame still owns the saved storage"
                );
                if pending {
                    FunctionCallStatus::Pending
                } else {
                    FunctionCallStatus::Cancelled
                }
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !value.frame_pointer().is_null() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "cancellation did not finish after native handoff"
                );
                std::thread::yield_now();
            }
            unsafe {
                jk_continuation_free(handle);
            }
        }
    }

    #[test]
    fn cancelling_a_parent_retains_its_frame_until_child_tasks_drain() {
        use crate::runtime::task::{TaskContext, TaskGroup};
        use std::sync::mpsc;

        struct Gate {
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        unsafe extern "C-unwind" fn task(context: *mut TaskContext) {
            let gate = unsafe { &*((*context).captures.cast::<Gate>()) };
            gate.entered.send(()).unwrap();
            gate.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        unsafe extern "C" fn unexpected_resume(_: *mut Continuation) {
            panic!("cancelled task wait must not execute user code");
        }
        let (entered, entry) = mpsc::channel();
        let (release, exit) = mpsc::channel();
        let gate = Gate {
            entered,
            release: Mutex::new(exit),
        };
        let mut context: TaskContext = unsafe { std::mem::zeroed() };
        context.captures = (&gate as *const Gate).cast_mut().cast();
        let mut group = TaskGroup::new();
        unsafe {
            group.spawn(task, &mut context);
        }
        entry.recv_timeout(Duration::from_secs(5)).unwrap();
        let parent = jk_continuation_new(1);
        let child = jk_continuation_new(1);
        unsafe {
            jk_continuation_alloc_frame(parent, 8);
            jk_continuation_alloc_frame(child, 8);
            jk_continuation_set_resume_callback(parent, unexpected_resume as *mut c_void);
            jk_continuation_set_resume_callback(child, unexpected_resume as *mut c_void);
        }
        let parent_value = unsafe { retain_handle(parent) }.unwrap();
        let child_value = unsafe { retain_handle(child) }.unwrap();
        let status = function_calls::with_function_pending_boundary(|| {
            assert!(parent_value.begin_function_pending(parent));
            assert_eq!(
                unsafe { jk_continuation_start_task_wait(child, &mut group, 0, parent) },
                FunctionCallStatus::Pending as u8
            );
            assert_eq!(
                unsafe { jk_continuation_poll_function_pending(parent) },
                FunctionCallStatus::Pending as u8
            );
            FunctionCallStatus::Pending
        });
        assert_eq!(status, FunctionCallStatus::Pending);
        unsafe {
            jk_continuation_free(parent);
        }
        assert!(
            !parent_value.frame_pointer().is_null(),
            "parent frame must survive pending child cancellation"
        );
        assert!(!parent_value.inner.handle_released.load(Ordering::Acquire));
        assert!(!child_value.frame_pointer().is_null());
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !parent_value.inner.handle_released.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "parent cancellation did not finish after child drain"
            );
            std::thread::yield_now();
        }
        assert!(parent_value.frame_pointer().is_null());
        assert!(child_value.frame_pointer().is_null());
        unsafe {
            jk_continuation_free(child);
        }
        drop(group);
    }
}
