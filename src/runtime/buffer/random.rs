//! OS entropy over exclusively borrowed, initialized byte storage.
use super::{located, Buffer, Error, BYTE_CHUNK};
use crate::runtime::parallel::pool::check_cancelled;

impl Buffer {
    /// The caller holds an exclusive loan on this live byte owner or slice.
    /// The payload is never retained or exposed to callbacks.
    pub(crate) fn fill_random(&mut self) -> Result<bool, Error> {
        self.bytes()?;
        // The checked byte layout and exclusive source loan establish this view.
        let bytes = unsafe { std::slice::from_raw_parts_mut(self.data.as_ptr(), self.len) };
        for chunk in bytes.chunks_mut(BYTE_CHUNK) {
            check_cancelled()?;
            if entropy(chunk).is_err() {
                // Ordinary initialized zeros, not a secure-erasure guarantee.
                bytes.fill(0);
                return Ok(false);
            }
        }
        check_cancelled()?;
        Ok(true)
    }
}
fn entropy(bytes: &mut [u8]) -> Result<(), getrandom::Error> {
    #[cfg(test)]
    if let Some(result) = testing::fill(bytes) {
        return result;
    }
    getrandom::fill(bytes)
}

// Compiler-owned ABI: live exclusive UInt8 buffer/slice, followed by source span.
// No pointer escapes this synchronous operation. Return 1 on full success and 0
// on entropy failure; layout/cancellation failures use the runtime error channel.
#[export_name = "__joky_buffer_random_fill"]
pub(super) unsafe extern "C" fn fill(
    destination: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> u8 {
    located(
        (*destination).fill_random().map(u8::from),
        source,
        start,
        end,
    )
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    }
    pub(crate) struct Guard(Option<(usize, usize)>);
    impl Guard {
        /// Succeed for this many chunks, then partially write and fail.
        pub(crate) fn new(successes: usize) -> Self {
            Self(STATE.with(|state| state.replace(Some((successes, 0)))))
        }
        pub(crate) fn calls(&self) -> usize {
            STATE.with(|state| state.get().unwrap().1)
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            STATE.with(|state| state.set(self.0));
        }
    }
    pub(super) fn fill(bytes: &mut [u8]) -> Option<Result<(), getrandom::Error>> {
        STATE.with(|state| {
            let (successes, calls) = state.get()?;
            state.set(Some((successes, calls + 1)));
            if calls >= successes {
                bytes[..1].fill(0xa5);
                Some(Err(getrandom::Error::UNSUPPORTED))
            } else {
                bytes.fill(0x5a);
                Some(Ok(()))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::arena::{Arena, LIVE};

    #[test]
    fn random_partial_entropy_is_cleared_and_layouts_are_checked() {
        let mut arena = Arena::default();
        let mut bytes = Buffer::copy_bytes(&mut arena, &vec![7; BYTE_CHUNK + 1]).unwrap();
        let guard = testing::Guard::new(1);
        unsafe {
            assert!(!bytes.as_mut().fill_random().unwrap());
            assert!(bytes.as_ref().bytes().unwrap().iter().all(|&b| b == 0));
        }
        assert_eq!(guard.calls(), 2);
        let mut wrong = Buffer::allocate(&mut arena, 1, 8, 8).unwrap();
        let mut empty = Buffer::copy_bytes(&mut arena, &[]).unwrap();
        unsafe {
            assert!(wrong.as_mut().fill_random().is_err());
            assert!(empty.as_mut().fill_random().unwrap());
        }
        assert_eq!(guard.calls(), 2);
        drop(arena);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
