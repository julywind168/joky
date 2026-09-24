use super::*;

impl Continuation {
    pub(super) fn allocate_frame(&self, size: usize) -> *mut u8 {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) || size == 0
        {
            return std::ptr::null_mut();
        }
        let mut storage = self
            .inner
            .frame_storage
            .lock()
            .expect("continuation frame mutex");
        if storage.is_some() {
            return std::ptr::null_mut();
        }
        let mut bytes = vec![0_u8; size].into_boxed_slice();
        let pointer = bytes.as_mut_ptr();
        self.inner.frame_pointer.store(pointer, Ordering::Release);
        self.inner.frame_size.store(size, Ordering::Release);
        *storage = Some(bytes);
        drop(state);
        pointer
    }

    pub(super) fn release_frame(&self) -> bool {
        let mut storage = self
            .inner
            .frame_storage
            .lock()
            .expect("continuation frame mutex");
        let released = storage.take().is_some();
        if released {
            self.inner
                .frame_pointer
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.frame_size.store(0, Ordering::Release);
            self.inner.program_counter.store(0, Ordering::Release);
        }
        released
    }

    pub(super) fn register_cleanup(
        &self,
        storage: u8,
        offset: usize,
        callback: *mut c_void,
        region: bool,
    ) -> bool {
        if callback.is_null()
            || !matches!(
                storage,
                CONTINUATION_FRAME_STORAGE
                    | CONTINUATION_SPILL_STORAGE
                    | CONTINUATION_RESULT_STORAGE
                    | CONTINUATION_SUSPEND_RESULT_STORAGE
                    | CONTINUATION_SUSPEND_ARGUMENT_STORAGE
                    | CONTINUATION_FAILURE_STORAGE
            )
        {
            return false;
        }
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            return false;
        }
        let mut entries = self
            .inner
            .cleanup_entries
            .lock()
            .expect("continuation cleanup mutex");
        // A machine continuation reuses its handle when it suspends again.
        // Refresh the descriptor for a frame slot instead of accumulating a
        // second armed drop for the same word.
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.storage == storage && entry.offset == offset)
        {
            *entry = ContinuationCleanup {
                storage,
                offset,
                callback: callback as usize,
                region,
                armed: true,
            };
            return true;
        }
        entries.push(ContinuationCleanup {
            storage,
            offset,
            callback: callback as usize,
            region,
            armed: true,
        });
        true
    }

    pub(super) fn disarm_cleanup(&self, storage: u8, offset: usize) -> bool {
        let mut entries = self
            .inner
            .cleanup_entries
            .lock()
            .expect("continuation cleanup mutex");
        let mut disarmed = false;
        for entry in entries
            .iter_mut()
            .filter(|entry| entry.armed && entry.storage == storage && entry.offset == offset)
        {
            entry.armed = false;
            disarmed = true;
        }
        disarmed
    }

    pub(super) fn cleanup_owned_storage(&self, include_result: bool) {
        let entries = std::mem::take(
            &mut *self
                .inner
                .cleanup_entries
                .lock()
                .expect("continuation cleanup mutex"),
        );
        self.invoke_cleanup_entries(entries.into_iter().filter(|entry| {
            entry.armed && (include_result || entry.storage != CONTINUATION_RESULT_STORAGE)
        }));
        let frames = std::mem::take(
            &mut *self
                .inner
                .owned_handlers
                .lock()
                .expect("owned handlers mutex"),
        );
        for frame in frames.into_iter().rev() {
            unsafe {
                crate::runtime::handler::jk_handler_frame_exit(frame as *mut _);
                crate::runtime::handler::jk_handler_frame_free(frame as *mut _);
            }
        }
    }

    /// Run and remove cleanup entries for one operation-owned storage area,
    /// leaving frame/spill/final-result registrations in place.
    pub(super) fn cleanup_storage(&self, storage: u8) {
        let mut selected = Vec::new();
        let mut entries = self
            .inner
            .cleanup_entries
            .lock()
            .expect("continuation cleanup mutex");
        let mut retained = Vec::with_capacity(entries.len());
        for entry in entries.drain(..) {
            if entry.storage == storage {
                if entry.armed {
                    selected.push(entry);
                }
            } else {
                retained.push(entry);
            }
        }
        *entries = retained;
        drop(entries);
        self.invoke_cleanup_entries(selected);
    }

    pub(super) fn invoke_cleanup_entries(
        &self,
        entries: impl IntoIterator<Item = ContinuationCleanup>,
    ) {
        for entry in entries {
            let base = match entry.storage {
                CONTINUATION_FRAME_STORAGE => self.frame_pointer(),
                CONTINUATION_SPILL_STORAGE => self.spill_pointer(),
                CONTINUATION_RESULT_STORAGE => self.result_pointer(),
                CONTINUATION_SUSPEND_RESULT_STORAGE => self.suspend_result_pointer(),
                CONTINUATION_SUSPEND_ARGUMENT_STORAGE => self.suspend_argument_pointer(),
                CONTINUATION_FAILURE_STORAGE => self.failure_pointer(),
                _ => std::ptr::null_mut(),
            };
            if base.is_null() {
                continue;
            }
            let callback: unsafe extern "C" fn(*mut c_void) =
                unsafe { std::mem::transmute(entry.callback) };
            if entry.region {
                // SAFETY: aggregate region callbacks receive the start of a
                // live flattened ABI value and interpret its active tag.
                unsafe { callback(base.add(entry.offset).cast()) };
            } else {
                // SAFETY: codegen registers an ABI pointer-sized component
                // inside a live storage allocation.
                let value = unsafe {
                    std::ptr::read_unaligned(base.add(entry.offset).cast::<*mut c_void>())
                };
                if !value.is_null() {
                    unsafe { callback(value) };
                }
            }
        }
    }

    pub(super) fn frame_pointer(&self) -> *mut u8 {
        self.inner.frame_pointer.load(Ordering::Acquire)
    }

    pub(super) fn frame_size(&self) -> usize {
        self.inner.frame_size.load(Ordering::Acquire)
    }

    pub(super) fn set_program_counter(&self, program_counter: usize) -> bool {
        *self.inner.region.lock().expect("continuation region") = crate::runtime::region::current();
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) || self.frame_pointer().is_null()
        {
            return false;
        }
        self.inner
            .program_counter
            .store(program_counter, Ordering::Release);
        true
    }

    pub(super) fn program_counter(&self) -> usize {
        self.inner.program_counter.load(Ordering::Acquire)
    }

    pub(super) fn allocate_result(&self, size: usize) -> *mut u8 {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) || size == 0
        {
            return std::ptr::null_mut();
        }
        let mut storage = self
            .inner
            .result_storage
            .lock()
            .expect("continuation result mutex");
        if storage.is_some() {
            return std::ptr::null_mut();
        }
        let mut bytes = vec![0_u8; size].into_boxed_slice();
        let pointer = bytes.as_mut_ptr();
        self.inner.result_pointer.store(pointer, Ordering::Release);
        self.inner.result_size.store(size, Ordering::Release);
        *storage = Some(bytes);
        drop(state);
        pointer
    }

    pub(super) fn release_result(&self) -> bool {
        let mut storage = self
            .inner
            .result_storage
            .lock()
            .expect("continuation result mutex");
        let released = storage.take().is_some();
        if released {
            self.inner
                .result_pointer
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.result_size.store(0, Ordering::Release);
        }
        released
    }

    pub(super) fn result_pointer(&self) -> *mut u8 {
        self.inner.result_pointer.load(Ordering::Acquire)
    }

    pub(super) fn result_size(&self) -> usize {
        self.inner.result_size.load(Ordering::Acquire)
    }

    pub(super) fn allocate_suspend_result(&self, size: usize) -> *mut u8 {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) || size == 0
        {
            return std::ptr::null_mut();
        }
        let mut storage = self
            .inner
            .suspend_result_storage
            .lock()
            .expect("continuation suspend result mutex");
        if storage.is_some() {
            return std::ptr::null_mut();
        }
        let mut bytes = vec![0_u8; size].into_boxed_slice();
        let pointer = bytes.as_mut_ptr();
        self.inner
            .suspend_result_pointer
            .store(pointer, Ordering::Release);
        self.inner
            .suspend_result_size
            .store(size, Ordering::Release);
        *storage = Some(bytes);
        drop(state);
        pointer
    }

    pub(super) fn release_suspend_result(&self) -> bool {
        let mut storage = self
            .inner
            .suspend_result_storage
            .lock()
            .expect("continuation suspend result mutex");
        let released = storage.take().is_some();
        if released {
            self.inner
                .suspend_result_pointer
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.suspend_result_size.store(0, Ordering::Release);
        }
        released
    }

    pub(super) fn suspend_result_pointer(&self) -> *mut u8 {
        self.inner.suspend_result_pointer.load(Ordering::Acquire)
    }

    pub(super) fn suspend_result_size(&self) -> usize {
        self.inner.suspend_result_size.load(Ordering::Acquire)
    }

    pub(super) fn allocate_failure(&self, size: usize) -> *mut u8 {
        if size == 0 {
            return std::ptr::null_mut();
        }
        let _state = self.inner.state.lock().expect("continuation state mutex");
        let mut storage = self
            .inner
            .failure_storage
            .lock()
            .expect("continuation failure mutex");
        if storage.is_some() {
            return std::ptr::null_mut();
        }
        let mut bytes = vec![0_u8; size].into_boxed_slice();
        let pointer = bytes.as_mut_ptr();
        self.inner.failure_pointer.store(pointer, Ordering::Release);
        self.inner.failure_size.store(size, Ordering::Release);
        *storage = Some(bytes);
        pointer
    }

    pub(super) fn release_failure(&self) -> bool {
        let mut storage = self
            .inner
            .failure_storage
            .lock()
            .expect("continuation failure mutex");
        let released = storage.take().is_some();
        if released {
            self.inner
                .failure_pointer
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.failure_size.store(0, Ordering::Release);
        }
        released
    }

    pub(super) fn failure_pointer(&self) -> *mut u8 {
        self.inner.failure_pointer.load(Ordering::Acquire)
    }
    pub(super) fn failure_size(&self) -> usize {
        self.inner.failure_size.load(Ordering::Acquire)
    }

    pub(super) fn allocate_suspend_arguments(&self, payload: *const u8, size: usize) -> *mut u8 {
        if payload.is_null() && size != 0 {
            return std::ptr::null_mut();
        }
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            return std::ptr::null_mut();
        }
        let mut storage = self
            .inner
            .suspend_argument_storage
            .lock()
            .expect("continuation suspend argument mutex");
        if storage.is_some() {
            return std::ptr::null_mut();
        }
        if size == 0 {
            self.inner
                .suspend_argument_pointer
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.suspend_argument_size.store(0, Ordering::Release);
            return std::ptr::null_mut();
        }
        let mut bytes = vec![0_u8; size].into_boxed_slice();
        // SAFETY: the caller checked the null/size pair and promises a live
        // payload buffer for the duration of this ABI call.
        unsafe { std::ptr::copy_nonoverlapping(payload, bytes.as_mut_ptr(), size) };
        let pointer = bytes.as_mut_ptr();
        self.inner
            .suspend_argument_pointer
            .store(pointer, Ordering::Release);
        self.inner
            .suspend_argument_size
            .store(size, Ordering::Release);
        *storage = Some(bytes);
        drop(state);
        pointer
    }

    pub(super) fn release_suspend_arguments(&self) -> bool {
        let mut storage = self
            .inner
            .suspend_argument_storage
            .lock()
            .expect("continuation suspend argument mutex");
        let released = storage.take().is_some();
        if released {
            self.inner
                .suspend_argument_pointer
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.suspend_argument_size.store(0, Ordering::Release);
        }
        released
    }

    pub(super) fn suspend_argument_pointer(&self) -> *mut u8 {
        self.inner.suspend_argument_pointer.load(Ordering::Acquire)
    }

    pub(super) fn suspend_argument_size(&self) -> usize {
        self.inner.suspend_argument_size.load(Ordering::Acquire)
    }

    pub(super) fn clear_suspend_cleanups(&self) -> bool {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            return false;
        }
        let mut entries = self
            .inner
            .cleanup_entries
            .lock()
            .expect("continuation cleanup mutex");
        let before = entries.len();
        entries.retain(|entry| {
            !matches!(
                entry.storage,
                CONTINUATION_SUSPEND_RESULT_STORAGE | CONTINUATION_SUSPEND_ARGUMENT_STORAGE
            )
        });
        entries.len() != before
    }
}
