//! Uniquely owned streaming states, released by ordinary resource cleanup.
use super::{
    bounds_error, check_cancelled, located, mac, Buffer, Digest, Error, HmacSha256, Mac, Sha256,
    BYTE_CHUNK,
};
use crate::runtime::{
    arena::{Arena, ExternalCharge},
    resource::{OwnedResource, ResourceReleaseV1},
};
use std::{alloc::Layout, ffi::c_void, ptr::NonNull};

enum State {
    Sha(Sha256),
    Hmac(HmacSha256),
}
// Thread-local accounting must never follow a resource to another worker.
enum Charge {
    Local {
        _charge: ExternalCharge,
    },
    Shared {
        _charge: crate::runtime::parallel::Lease,
    },
}
struct Stream {
    state: Option<State>,
    charge: Charge,
}
unsafe extern "C" fn release(token: *mut c_void) {
    std::ptr::drop_in_place(token.cast::<Stream>());
    std::alloc::dealloc(token.cast(), Layout::new::<Stream>());
}
pub(crate) fn transferable_type(name: &str) -> bool {
    matches!(name, "crypto.Sha256Owner" | "crypto.HmacSha256Owner")
}
/// # Safety
/// The resource owns this live token and has no active operation. Validate the
/// release callback before inspecting the private layout, as for TCP owners.
pub(crate) unsafe fn transferable(token: NonNull<c_void>, callback: ResourceReleaseV1) -> bool {
    std::ptr::fn_addr_eq(callback, release as ResourceReleaseV1)
        && matches!(
            (*token.cast::<Stream>().as_ptr()).charge,
            Charge::Shared { .. }
        )
}
pub(crate) fn stream_new(
    arena: &mut Arena,
    key: Option<&Buffer>,
) -> Result<NonNull<Buffer>, Error> {
    check_cancelled()?;
    let state = match key {
        Some(key) => State::Hmac(mac(key.bytes()?, &[])?),
        None => State::Sha(Sha256::new()),
    };
    let bytes = std::mem::size_of::<Stream>() + OwnedResource::event_bytes();
    let charge = if let Some(root) = arena.parallel_root() {
        Charge::Shared {
            _charge: root.accounts().reserve(bytes)?,
        }
    } else {
        Charge::Local {
            _charge: arena.reserve_external(bytes)?,
        }
    };
    let pointer =
        NonNull::new(unsafe { std::alloc::alloc(Layout::new::<Stream>()) }).ok_or_else(|| {
            Error::new_with_code(
                crate::ErrorPhase::Runtime,
                crate::ErrorCode::ResourceLimit,
                "Cryptographic state allocation failed",
            )
        })?;
    unsafe {
        pointer.cast::<Stream>().as_ptr().write(Stream {
            state: Some(state),
            charge,
        })
    };
    let resource = match OwnedResource::from_token(pointer.cast(), release) {
        Ok(resource) => resource,
        Err(error) => {
            unsafe { release(pointer.as_ptr().cast()) };
            return Err(error);
        }
    };
    Buffer::external(arena, resource)
}
fn with_state<R>(
    owner: &mut Buffer,
    hmac: bool,
    f: impl FnOnce(&mut Option<State>) -> Result<R, Error>,
) -> Result<R, Error> {
    let resource = owner.resource_mut()?;
    resource.control_token(release)?;
    let lease = resource.lease()?;
    // The private callback proves the layout; the lease serializes access.
    let state = unsafe { &mut (*lease.token().cast::<Stream>()).state };
    if !matches!(
        (&*state, hmac),
        (Some(State::Sha(_)), false) | (Some(State::Hmac(_)), true)
    ) {
        return Err(bounds_error(
            "Cryptographic state has finished or has the wrong algorithm",
        ));
    }
    check_cancelled()?;
    f(state)
}
pub(crate) fn stream_update(owner: &mut Buffer, input: &Buffer, hmac: bool) -> Result<(), Error> {
    let input = input.bytes()?;
    with_state(owner, hmac, |state| {
        for chunk in input.chunks(BYTE_CHUNK) {
            check_cancelled()?;
            match state.as_mut().unwrap() {
                State::Sha(state) => Digest::update(state, chunk),
                State::Hmac(state) => Mac::update(state, chunk),
            }
        }
        check_cancelled()
    })
}
pub(crate) fn stream_finish(
    owner: &mut Buffer,
    destination: &mut Buffer,
    hmac: bool,
) -> Result<(), Error> {
    destination.crypto_output()?;
    let result: [u8; 32] = with_state(owner, hmac, |state| {
        Ok(match state.take().unwrap() {
            State::Sha(state) => state.finalize().into(),
            State::Hmac(state) => state.finalize().into_bytes().into(),
        })
    })?;
    unsafe { std::ptr::copy_nonoverlapping(result.as_ptr(), destination.data.as_ptr(), 32) };
    Ok(())
}
pub(crate) fn stream_verify(owner: &mut Buffer, tag: &Buffer) -> Result<bool, Error> {
    let tag = tag.bytes()?;
    with_state(owner, true, |state| {
        let State::Hmac(state) = state.take().unwrap() else {
            unreachable!()
        };
        // The library rejects non-full-length tags and compares equal lengths
        // in constant time. Invalid tags still consume the streaming state.
        Ok(state.verify_slice(tag).is_ok())
    })
}

/// # Safety
/// Arena is exclusively live on its task thread. Generated code registers
/// cleanup for the returned resource before any subsequent fallible operation.
#[export_name = "__joky_buffer_sha256_new"]
pub(crate) unsafe extern "C" fn sha256_new(
    arena: *mut Arena,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    located(
        stream_new(&mut *arena, None).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}
/// # Safety
/// As for sha256_new; key is a live shared byte view borrowed only for this call.
#[export_name = "__joky_buffer_hmac_sha256_new"]
pub(crate) unsafe extern "C" fn hmac_sha256_new(
    arena: *mut Arena,
    key: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    located(
        stream_new(&mut *arena, Some(&*key)).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}
macro_rules! update {
    ($name:ident, $symbol:literal, $hmac:literal) => {
        /// # Safety
        /// Owner is an exclusive live streaming resource; input is a shared
        /// byte view borrowed for this call only, disjoint from the state.
        #[export_name = $symbol]
        pub(crate) unsafe extern "C" fn $name(
            owner: *mut Buffer,
            input: *const Buffer,
            source: u64,
            start: usize,
            end: usize,
        ) {
            located(
                stream_update(&mut *owner, &*input, $hmac),
                source,
                start,
                end,
            )
        }
    };
}
macro_rules! finish {
    ($name:ident, $symbol:literal, $hmac:literal) => {
        /// # Safety
        /// Owner is an exclusively borrowed live resource. The consuming
        /// source wrapper retains cleanup until completion.
        /// Destination is a disjoint exclusive initialized byte view.
        #[export_name = $symbol]
        pub(crate) unsafe extern "C" fn $name(
            owner: *mut Buffer,
            destination: *mut Buffer,
            source: u64,
            start: usize,
            end: usize,
        ) {
            located(
                stream_finish(&mut *owner, &mut *destination, $hmac),
                source,
                start,
                end,
            )
        }
    };
}
update!(sha256_update, "__joky_buffer_sha256_update", false);
update!(hmac_sha256_update, "__joky_buffer_hmac_sha256_update", true);
finish!(sha256_finish, "__joky_buffer_sha256_finish", false);
finish!(hmac_sha256_finish, "__joky_buffer_hmac_sha256_finish", true);
/// # Safety
/// Owner is an exclusively borrowed live HMAC resource. The consuming source
/// wrapper retains cleanup until completion. Tag is
/// a shared initialized byte view borrowed only for the duration of this call.
#[export_name = "__joky_buffer_hmac_sha256_verify"]
pub(crate) unsafe extern "C" fn hmac_sha256_verify(
    owner: *mut Buffer,
    tag: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> u8 {
    located(
        stream_verify(&mut *owner, &*tag).map(u8::from),
        source,
        start,
        end,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_stream_state_validation_and_local_accounting() {
        let mut arena = Arena::default();
        let input = Buffer::copy_bytes(&mut arena, b"abc").unwrap();
        let invalid = Buffer::allocate(&mut arena, 0, 8, 8).unwrap();
        let mut output = Buffer::copy_bytes(&mut arena, &[7; 32]).unwrap();
        let mut short = Buffer::copy_bytes(&mut arena, &[7; 31]).unwrap();
        let baseline = arena.bytes();
        for _ in 0..8 {
            let mut owner = stream_new(&mut arena, None).unwrap();
            assert!(arena.bytes() >= baseline + std::mem::size_of::<Stream>());
            unsafe {
                assert!(!owner.as_ref().resource_ref().unwrap().parallel_safe());
                assert!(stream_update(owner.as_mut(), input.as_ref(), true).is_err());
                assert!(stream_update(owner.as_mut(), invalid.as_ref(), false).is_err());
                stream_update(owner.as_mut(), input.as_ref(), false).unwrap();
                assert!(stream_finish(owner.as_mut(), short.as_mut(), false).is_err());
                assert_eq!(short.as_ref().bytes().unwrap(), &[7; 31]);
                stream_finish(owner.as_mut(), output.as_mut(), false).unwrap();
                assert_eq!(
                    output.as_ref().bytes().unwrap(),
                    super::super::digest(b"abc").unwrap()
                );
                assert!(stream_update(owner.as_mut(), input.as_ref(), false).is_err());
                assert!(stream_finish(owner.as_mut(), output.as_mut(), false).is_err());
                Buffer::release(&mut arena, owner.as_ptr()).unwrap();
                let mut owner = stream_new(&mut arena, Some(input.as_ref())).unwrap();
                assert!(!stream_verify(owner.as_mut(), short.as_ref()).unwrap());
                assert!(stream_verify(owner.as_mut(), output.as_ref()).is_err());
                Buffer::release(&mut arena, owner.as_ptr()).unwrap();
                assert!(stream_update(output.as_mut(), input.as_ref(), false).is_err());
            }
            assert_eq!(arena.bytes(), baseline);
        }
    }

    #[test]
    fn crypto_stream_partial_creation_refunds_state_and_shared_leases() {
        use crate::runtime::{
            arena::LIMIT,
            parallel::{Storage, TaskBudget},
        };
        struct Restore(usize);
        impl Drop for Restore {
            fn drop(&mut self) {
                LIMIT.with(|n| n.set(self.0));
            }
        }
        let mut arena = Arena::default();
        {
            // State allocation fits, but its Buffer control block does not.
            let _restore =
                Restore(LIMIT.with(|n| {
                    n.replace(std::mem::size_of::<Stream>() + OwnedResource::event_bytes())
                }));
            assert!(stream_new(&mut arena, None).is_err());
            assert_eq!(arena.bytes(), 0);
        }
        let budget = TaskBudget::new(8192);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let mut arena = Arena::with_storage(root.clone());
        for _ in 0..8 {
            let owner = stream_new(&mut arena, None).unwrap();
            unsafe {
                assert!(owner.as_ref().resource_ref().unwrap().parallel_safe());
                let lease = owner.as_ref().resource_ref().unwrap().lease().unwrap();
                assert!(!owner.as_ref().resource_ref().unwrap().parallel_safe());
                drop(lease);
                Buffer::release(&mut arena, owner.as_ptr()).unwrap();
            }
        }
        drop(arena);
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}
