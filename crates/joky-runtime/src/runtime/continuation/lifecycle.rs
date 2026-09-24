use super::*;

impl Continuation {
    pub(super) fn begin_scope_work(&self) -> bool {
        let mut work = self
            .inner
            .scope_work
            .lock()
            .expect("continuation scope work mutex");
        if work.is_some() {
            return true;
        }
        let weak = Arc::downgrade(&self.inner);
        let Some(scope_work) = self.inner.scope.begin_work(move || {
            if let Some(inner) = weak.upgrade() {
                let continuation = Continuation { inner };
                continuation.cancel();
            }
        }) else {
            return false;
        };
        *work = Some(scope_work);
        true
    }

    pub(super) fn release_scope_work(&self) {
        self.inner
            .scope_work
            .lock()
            .expect("continuation scope work mutex")
            .take();
    }

    /// Ready is not terminal: a provider can have published its result without
    /// yet reserving the next callback. Keep the lease across that handoff gap.
    pub(super) fn release_scope_work_if_idle(&self) {
        let state = self.state();
        if matches!(
            state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            self.release_scope_work();
        }
    }

    pub(super) fn remember_owner_handle(&self, handle: *mut Continuation) {
        if !handle.is_null() {
            let _ = self.inner.owner_handle.compare_exchange(
                0,
                handle as usize,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    /// Reserve one scheduler callback before publishing it to the executor.
    /// The lifecycle gate closes the admission race with `jk_continuation_free`.
    pub(super) fn reserve_callback(&self, handle: *mut Continuation) -> Option<CallbackLease> {
        let _gate = self
            .inner
            .lifecycle_gate
            .lock()
            .expect("continuation lifecycle mutex");
        if (self.inner.free_requested.load(Ordering::Acquire) && !self.internal_wait_active())
            || self.inner.handle_released.load(Ordering::Acquire)
        {
            return None;
        }
        self.remember_owner_handle(handle);
        self.inner
            .callbacks_in_flight
            .fetch_add(1, Ordering::AcqRel);
        Some(CallbackLease {
            inner: Arc::clone(&self.inner),
            previous: 0,
        })
    }

    pub(super) fn release_timer_ticket(&self, ticket: &Arc<TimerTicket>) -> bool {
        let current = {
            let mut current = self
                .inner
                .timer_ticket
                .lock()
                .expect("continuation timer mutex");
            if current
                .as_ref()
                .is_some_and(|value| Arc::ptr_eq(value, ticket))
            {
                let current = current.take();
                // Publish the inactive timer ID while holding the same mutex
                // used by `publish_timer_id`, so a timer that fires before
                // registration returns cannot be republished afterward.
                self.inner.timer_id.store(0, Ordering::Release);
                current
            } else {
                None
            }
        };
        let Some(current) = current else {
            return false;
        };
        if !current.release() {
            return false;
        }
        let previous = self.inner.timer_active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "continuation timer underflow");
        self.inner.wake.notify_all();
        true
    }

    pub(super) fn cancel_active_timer(&self) {
        let (timer_id, ticket) = {
            let mut ticket = self
                .inner
                .timer_ticket
                .lock()
                .expect("continuation timer mutex");
            let ticket = ticket.take();
            let timer_id = self.inner.timer_id.swap(0, Ordering::AcqRel);
            (timer_id, ticket)
        };
        if let Some(ticket) = ticket {
            if ticket.release() {
                let previous = self.inner.timer_active.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "continuation timer underflow");
            }
        }
        if timer_id != 0 {
            crate::runtime::reactor::cancel_timer(timer_id);
        }
        self.inner.wake.notify_all();
    }

    fn publish_timer_id(&self, ticket: &Arc<TimerTicket>, timer_id: u64) -> bool {
        let current = self
            .inner
            .timer_ticket
            .lock()
            .expect("continuation timer mutex");
        if current
            .as_ref()
            .is_some_and(|value| Arc::ptr_eq(value, ticket))
            && !ticket.released.load(Ordering::Acquire)
        {
            self.inner.timer_id.store(timer_id, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub(super) fn timer_cancelled(&self, ticket: Arc<TimerTicket>) {
        if !self.release_timer_ticket(&ticket) {
            return;
        }
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state == ContinuationState::Sleeping {
            *state = ContinuationState::Ready;
        }
        drop(state);
        self.release_scope_work();
        self.inner.wake.notify_all();
    }

    #[cfg(test)]
    pub(super) fn sleep(&self, milliseconds: u64) -> bool {
        self.sleep_with_handle(std::ptr::null_mut(), milliseconds, false)
    }

    /// Start a timer-backed suspending operation.
    pub(super) fn start_suspend(
        &self,
        handle: *mut Continuation,
        operation: u64,
        milliseconds: u64,
        dispatch_machine: bool,
    ) -> bool {
        if !handle.is_null() {
            self.inner.handle.store(handle as usize, Ordering::Release);
        }
        // Zero is the inactive sentinel, while operation id zero is valid.
        // Offset provider operation tokens by one in the continuation header.
        self.inner
            .active_operation
            .store(operation.wrapping_add(1), Ordering::Release);
        if self.sleep_with_handle(handle, milliseconds, dispatch_machine) {
            true
        } else {
            self.inner.active_operation.store(0, Ordering::Release);
            false
        }
    }

    /// Start a provider-backed operation.
    pub(super) fn start_suspend_provider(&self, handle: *mut Continuation, operation: u64) -> bool {
        *self.inner.region.lock().expect("continuation region") = crate::runtime::region::current();
        if !handle.is_null() {
            self.inner.handle.store(handle as usize, Ordering::Release);
        }
        let provider = suspend_providers()
            .lock()
            .expect("suspend provider mutex")
            .get(&(self.inner.scope.id(), operation))
            .copied();
        let Some(provider) = provider else {
            return false;
        };
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Ready
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
        {
            return false;
        }
        *state = ContinuationState::Sleeping;
        if !self.begin_scope_work() {
            *state = ContinuationState::Ready;
            return false;
        }
        self.inner.handler_frame.store(
            crate::runtime::handler::current() as usize,
            Ordering::Release,
        );
        self.inner
            .active_operation
            .store(operation.wrapping_add(1), Ordering::Release);
        self.inner
            .provider_cancel
            .store(provider.cancel as *mut c_void, Ordering::Release);
        drop(state);

        let start: unsafe extern "C" fn(
            *mut Continuation,
            u64,
            *const u8,
            usize,
            *mut u8,
            usize,
        ) -> u8 = unsafe { std::mem::transmute(provider.start) };
        let accepted = unsafe {
            start(
                handle,
                operation,
                self.suspend_argument_pointer(),
                self.suspend_argument_size(),
                self.suspend_result_pointer(),
                self.suspend_result_size(),
            )
        } != 0;
        if !accepted {
            self.inner
                .provider_cancel
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.active_operation.store(0, Ordering::Release);
            let mut state = self.inner.state.lock().expect("continuation state mutex");
            if *state == ContinuationState::Sleeping {
                *state = ContinuationState::Ready;
            }
            self.inner.wake.notify_all();
            self.release_scope_work();
        }
        accepted
    }

    pub(super) fn resuspend_provider(&self, handle: *mut Continuation, operation: u64) -> bool {
        *self.inner.region.lock().expect("continuation region") = crate::runtime::region::current();
        let provider = suspend_providers()
            .lock()
            .expect("suspend provider mutex")
            .get(&(self.inner.scope.id(), operation))
            .copied();
        let Some(provider) = provider else {
            return false;
        };
        if !handle.is_null() {
            self.inner.handle.store(handle as usize, Ordering::Release);
        }
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Resuming
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
        {
            return false;
        }
        *state = ContinuationState::Sleeping;
        self.inner
            .active_operation
            .store(operation.wrapping_add(1), Ordering::Release);
        self.inner
            .provider_cancel
            .store(provider.cancel as *mut c_void, Ordering::Release);
        drop(state);

        let start: unsafe extern "C" fn(
            *mut Continuation,
            u64,
            *const u8,
            usize,
            *mut u8,
            usize,
        ) -> u8 = unsafe { std::mem::transmute(provider.start) };
        let accepted = unsafe {
            start(
                handle,
                operation,
                self.suspend_argument_pointer(),
                self.suspend_argument_size(),
                self.suspend_result_pointer(),
                self.suspend_result_size(),
            )
        } != 0;
        if !accepted {
            self.inner
                .provider_cancel
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.active_operation.store(0, Ordering::Release);
            let mut state = self.inner.state.lock().expect("continuation state mutex");
            if *state == ContinuationState::Sleeping {
                *state = ContinuationState::Resuming;
            }
            self.release_scope_work();
        }
        accepted
    }

    pub(super) fn sleep_with_handle(
        &self,
        handle: *mut Continuation,
        milliseconds: u64,
        dispatch_machine: bool,
    ) -> bool {
        *self.inner.region.lock().expect("continuation region") = crate::runtime::region::current();
        let handle_address = handle as usize;
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Ready {
            return false;
        }
        *state = ContinuationState::Sleeping;
        if !self.begin_scope_work() {
            *state = ContinuationState::Ready;
            return false;
        }
        self.inner.handler_frame.store(
            crate::runtime::handler::current() as usize,
            Ordering::Release,
        );
        self.remember_owner_handle(handle);
        let inner = Arc::clone(&self.inner);
        let ticket = TimerTicket::new();
        *self
            .inner
            .timer_ticket
            .lock()
            .expect("continuation timer mutex") = Some(Arc::clone(&ticket));
        inner.timer_active.fetch_add(1, Ordering::AcqRel);
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(milliseconds))
            .unwrap_or_else(Instant::now);
        let fired_ticket = Arc::clone(&ticket);
        let fired_inner = Arc::clone(&inner);
        let cancel_ticket = Arc::clone(&ticket);
        let cancel_inner = Arc::clone(&inner);
        let timer_id = crate::runtime::reactor::register_timer_with_cancel(
            deadline,
            Box::new(move || {
                let continuation = Continuation {
                    inner: Arc::clone(&fired_inner),
                };
                continuation.timer_fired(
                    handle_address as *mut Continuation,
                    dispatch_machine,
                    fired_ticket,
                );
            }),
            Some(Box::new(move || {
                let continuation = Continuation {
                    inner: cancel_inner,
                };
                continuation.timer_cancelled(cancel_ticket);
            })),
        );
        if !self.publish_timer_id(&ticket, timer_id) {
            // Cancellation or timer expiry won the publication race. The
            // reactor request may already be complete; cancellation is then
            // a no-op, otherwise it releases the still-pending registration.
            crate::runtime::reactor::cancel_timer(timer_id);
        }
        true
    }

    pub(super) fn timer_fired(
        &self,
        handle: *mut Continuation,
        dispatch_machine: bool,
        ticket: Arc<TimerTicket>,
    ) {
        if !self.release_timer_ticket(&ticket) {
            return;
        }
        let ready = {
            let mut state = self.inner.state.lock().expect("continuation state mutex");
            if *state == ContinuationState::Sleeping {
                *state = ContinuationState::Ready;
                true
            } else {
                false
            }
        };
        self.inner.wake.notify_all();

        // Cancellation may have raced the timer timeout. The task token is
        // authoritative even if the continuation was not explicitly cancelled.
        if ready {
            let context = self.inner.task_context.load(Ordering::Acquire);
            if context != 0
                && crate::runtime::task::task_context_is_cancelled(
                    context as *mut crate::runtime::task::TaskContext,
                )
            {
                self.cancel();
            }
        }

        let machine_entry = self.inner.machine_entry.load(Ordering::Acquire);
        let callback = self.inner.resume_callback.load(Ordering::Acquire);
        let should_dispatch = ready
            && dispatch_machine
            && (!machine_entry.is_null() || !callback.is_null())
            && *self.inner.state.lock().expect("continuation state mutex")
                == ContinuationState::Ready;
        if should_dispatch {
            self.enqueue_resume(handle);
        } else {
            self.release_scope_work();
            self.inner.wake.notify_all();
        }
    }

    /// Arm the next timer while a machine entry is currently resuming.
    pub(super) fn resuspend_sleep(&self, handle: *mut Continuation, milliseconds: u64) -> bool {
        {
            let mut state = self.inner.state.lock().expect("continuation state mutex");
            if *state != ContinuationState::Resuming {
                return false;
            }
            if self.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
                drop(state);
                self.finish_cancelled();
                return false;
            }
            *state = ContinuationState::Ready;
        }
        self.sleep_with_handle(handle, milliseconds, true)
    }
}
