use super::*;

pub(super) enum ResumptionPayload {
    Raw(Vec<u8>),
    /// A managed String is retained by the handler frame. The handle only
    /// receives its flattened pointer/length ABI and duplicates the pointer
    /// before returning it to generated code.
    String {
        pointer: u64,
        length: u64,
    },
    /// A flattened aggregate that owns the managed pointer words listed in
    /// `managed_offsets`. The offsets are byte offsets into `bytes`.
    Managed {
        bytes: Vec<u8>,
        managed_offsets: Vec<usize>,
    },
}

impl ResumptionPayload {
    pub(super) fn flattened(&self) -> Vec<u8> {
        match self {
            Self::Raw(bytes) => bytes.clone(),
            Self::String { pointer, length } => pointer
                .to_ne_bytes()
                .into_iter()
                .chain(length.to_ne_bytes())
                .collect(),
            Self::Managed { bytes, .. } => bytes.clone(),
        }
    }

    pub(super) fn release(self) {
        match self {
            Self::String { pointer, .. } => {
                if pointer != 0 {
                    jk_drop(pointer as usize as *mut u8);
                }
            }
            Self::Managed {
                bytes,
                managed_offsets,
            } => {
                for offset in managed_offsets {
                    let Some(pointer_bytes) = bytes.get(offset..offset.saturating_add(8)) else {
                        continue;
                    };
                    let Ok(pointer_bytes) = <[u8; 8]>::try_from(pointer_bytes) else {
                        continue;
                    };
                    let pointer = u64::from_ne_bytes(pointer_bytes);
                    if pointer != 0 {
                        jk_drop(pointer as usize as *mut u8);
                    }
                }
            }
            Self::Raw(_) => {}
        }
    }

    pub(super) fn take_flattened(self) -> Vec<u8> {
        let bytes = self.flattened();
        // The flattened bytes now own the references formerly held by this
        // payload. They are released when the resumed value is dropped.
        std::mem::forget(self);
        bytes
    }
}
pub(super) struct HandlerEnv {
    pub(super) captures: Vec<u8>,
    pub(super) slots: Vec<OwnedHandlerEnvSlot>,
}

pub(super) struct OwnedHandlerEnvSlot {
    pub(super) offset: usize,
    pub(super) ownership: HandlerEnvOwnership,
    pub(super) drop_callback: usize,
}

impl Drop for HandlerEnv {
    fn drop(&mut self) {
        for slot in &self.slots {
            if !matches!(
                slot.ownership,
                HandlerEnvOwnership::Owned | HandlerEnvOwnership::Shared
            ) || slot.drop_callback == 0
                || slot.offset > self.captures.len()
                || self.captures.len() - slot.offset < std::mem::size_of::<usize>()
            {
                continue;
            }
            let Some(pointer_bytes) = self
                .captures
                .get(slot.offset..slot.offset + std::mem::size_of::<usize>())
            else {
                continue;
            };
            let Ok(pointer_bytes) = <[u8; std::mem::size_of::<usize>()]>::try_from(pointer_bytes)
            else {
                continue;
            };
            let pointer = usize::from_ne_bytes(pointer_bytes);
            if pointer == 0 {
                continue;
            }
            // SAFETY: registration validates that an owned slot has a
            // non-null C ABI drop callback.
            let drop: unsafe extern "C" fn(*mut u8) =
                unsafe { std::mem::transmute(slot.drop_callback) };
            unsafe { drop(pointer as *mut u8) };
        }
    }
}

impl HandlerEnv {
    /// Transfer a managed capture when a handler returns that capture (or an
    /// aggregate containing it) as its result. The result buffer becomes the
    /// new owner, so the environment slot must be cleared before the frame is
    /// released. Pointer equality is intentional here: unique values cannot
    /// be aliased except when they are being moved out. Shared captures are
    /// retained independently and are therefore released by the environment
    /// even when the handler returns an equal pointer.
    pub(super) fn transfer_owned_result(&mut self, result: &[u8]) {
        for slot in &self.slots {
            if slot.ownership != HandlerEnvOwnership::Owned
                || slot.offset > self.captures.len()
                || self.captures.len() - slot.offset < std::mem::size_of::<usize>()
            {
                continue;
            }
            let Some(pointer_bytes) = self
                .captures
                .get(slot.offset..slot.offset + std::mem::size_of::<usize>())
            else {
                continue;
            };
            let Ok(pointer_bytes) = <[u8; std::mem::size_of::<usize>()]>::try_from(pointer_bytes)
            else {
                continue;
            };
            let pointer = usize::from_ne_bytes(pointer_bytes);
            if pointer == 0
                || !result
                    .as_chunks::<{ std::mem::size_of::<usize>() }>()
                    .0
                    .contains(&pointer.to_ne_bytes())
            {
                continue;
            }
            if let Some(pointer_bytes) = self
                .captures
                .get_mut(slot.offset..slot.offset + std::mem::size_of::<usize>())
            {
                pointer_bytes.fill(0);
            }
        }
    }
}

pub(super) fn release_managed_bytes(bytes: &[u8], slots: &[(usize, usize)]) {
    for (offset, callback) in slots {
        let Some(pointer_bytes) = bytes.get(*offset..offset.saturating_add(8)) else {
            continue;
        };
        let Ok(pointer_bytes) = <[u8; 8]>::try_from(pointer_bytes) else {
            continue;
        };
        let pointer = u64::from_ne_bytes(pointer_bytes) as usize as *mut u8;
        if pointer.is_null() {
            continue;
        }
        if *callback == 0 {
            jk_drop(pointer);
        } else {
            // SAFETY: generated class drop glue uses the same C ABI as the
            // runtime managed drop entry point and remains linked for the
            // entire compiled program.
            let drop: unsafe extern "C" fn(*mut u8) = unsafe { std::mem::transmute(*callback) };
            unsafe { drop(pointer) };
        }
    }
}

pub(super) struct HandlerThunkContext {
    pub(super) thunk: usize,
    pub(super) environment: HandlerEnv,
    pub(super) result_capacity: usize,
    pub(super) registered_payload: bool,
    pub(super) result_slots: Vec<(usize, usize)>,
}
