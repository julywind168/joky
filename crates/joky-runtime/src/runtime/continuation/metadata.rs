use super::*;

impl Continuation {
    pub(super) fn set_spill(&self, pointer: *mut u8, size: usize) -> bool {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            return false;
        }
        self.inner.spill_pointer.store(pointer, Ordering::Release);
        self.inner.spill_size.store(size, Ordering::Release);
        true
    }

    pub(super) fn allocate_spill(&self, size: usize) -> *mut u8 {
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
            .spill_storage
            .lock()
            .expect("continuation spill mutex");
        if storage.is_some() {
            return std::ptr::null_mut();
        }
        let mut bytes = vec![0u8; size].into_boxed_slice();
        let pointer = bytes.as_mut_ptr();
        self.inner.spill_pointer.store(pointer, Ordering::Release);
        self.inner.spill_size.store(size, Ordering::Release);
        *storage = Some(bytes);
        drop(state);
        pointer
    }

    pub(super) fn release_spill(&self) -> bool {
        let mut storage = self
            .inner
            .spill_storage
            .lock()
            .expect("continuation spill mutex");
        let released = storage.take().is_some();
        if released {
            self.inner
                .spill_pointer
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner.spill_size.store(0, Ordering::Release);
        }
        released
    }

    pub(super) fn set_resume_entry(&self, entry: usize) -> bool {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            return false;
        }
        self.inner.resume_entry.store(entry, Ordering::Release);
        self.inner.machine_entry.store(
            registered_machine_entry(self.inner.scope.code_scope_id(), entry)
                .unwrap_or(std::ptr::null_mut()),
            Ordering::Release,
        );
        true
    }

    pub(super) fn set_root_group(&self, group: usize) {
        self.inner.root_group.store(group, Ordering::Release);
    }

    pub(super) fn root_group(&self) -> usize {
        self.inner.root_group.load(Ordering::Acquire)
    }

    pub(super) fn spill_pointer(&self) -> *mut u8 {
        self.inner.spill_pointer.load(Ordering::Acquire)
    }

    pub(super) fn spill_size(&self) -> usize {
        self.inner.spill_size.load(Ordering::Acquire)
    }

    pub(super) fn resume_entry(&self) -> usize {
        self.inner.resume_entry.load(Ordering::Acquire)
    }
}
