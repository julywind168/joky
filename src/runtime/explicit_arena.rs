//! Explicit regions are ordinary owning resources with task-shared accounting.
use super::{
    arena::{Arena, ExternalCharge},
    buffer::Buffer,
    resource::OwnedResource,
};
use crate::{Error, ErrorCode, ErrorPhase, SourceId, Span};
use std::{alloc::Layout, ffi::c_void, ptr::NonNull};

struct Region {
    arena: Arena,
    _charge: ExternalCharge,
}

fn allocation_error() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::ResourceLimit,
        "Explicit arena allocation failed",
    )
}

unsafe extern "C" fn release(token: *mut c_void) {
    std::ptr::drop_in_place(token.cast::<Region>());
    std::alloc::dealloc(token.cast(), Layout::new::<Region>());
}

pub(crate) fn new(parent: &mut Arena) -> Result<NonNull<Buffer>, Error> {
    let charge = parent.reserve_external(std::mem::size_of::<Region>())?;
    let pointer = NonNull::new(unsafe { std::alloc::alloc(Layout::new::<Region>()) })
        .ok_or_else(allocation_error)?;
    unsafe {
        pointer.cast::<Region>().as_ptr().write(Region {
            arena: parent.child(),
            _charge: charge,
        });
    }
    let resource = match OwnedResource::from_token(pointer.cast(), release) {
        Ok(resource) => resource,
        Err(error) => {
            unsafe { release(pointer.as_ptr().cast()) };
            return Err(error);
        }
    };
    Buffer::external(parent, resource)
}

/// # Safety
/// Owner is a live shared borrow of an explicit arena on its serialized task
/// thread. Size and alignment describe checked non-owning storage, and the
/// returned allocation must never be accessed after the owner is released.
pub(crate) unsafe fn allocate(
    owner: *mut Buffer,
    size: usize,
    align: usize,
) -> Result<NonNull<u8>, Error> {
    let owner = owner.as_mut().ok_or_else(allocation_error)?;
    let lease = owner.resource_mut()?.lease()?;
    (*lease.token().cast::<Region>())
        .arena
        .allocate(size, align)
}

fn located<T>(result: Result<NonNull<T>, Error>, source: u64, start: usize, end: usize) -> *mut T {
    match result {
        Ok(pointer) => pointer.as_ptr(),
        Err(error) => {
            super::error::record_error(
                error.with_span(Span::new(start, end).with_source(SourceId::from_raw(source))),
            );
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// Parent points to an exclusively accessible live task arena on its serialized
/// execution thread. Generated code must register cleanup for the returned owner.
#[export_name = "__joky_explicit_arena_new"]
pub(crate) unsafe extern "C" fn new_native(
    parent: *mut Arena,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    located(new(&mut *parent), source, start, end)
}

/// # Safety
/// Owner is a live explicit-arena resource borrowed on its serialized task
/// thread. Size and alignment describe checked non-owning storage, and no view
/// of the returned allocation may outlive the resource owner.
#[export_name = "__joky_explicit_arena_alloc"]
pub(crate) unsafe extern "C" fn allocate_native(
    owner: *mut Buffer,
    size: usize,
    align: usize,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located(allocate(owner, size, align), source, start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::arena::{Budget, LIMIT, LIVE};

    struct Limit(usize);
    impl Limit {
        fn set(bytes: usize) -> Self {
            Self(LIMIT.with(|limit| limit.replace(bytes)))
        }
    }
    impl Drop for Limit {
        fn drop(&mut self) {
            LIMIT.with(|limit| limit.set(self.0));
        }
    }

    #[test]
    fn explicit_arena_release_reclaims_storage_without_moving_task_values() {
        let _limit = Limit::set(6144);
        let mut parent = Arena::default();
        let stable = parent.allocate(8, 8).unwrap().cast::<u64>();
        unsafe { stable.as_ptr().write(42) };
        let initial = parent.bytes();
        for _ in 0..32 {
            let owner = new(&mut parent).unwrap();
            unsafe {
                let pointer = allocate(owner.as_ptr(), 4096, 64).unwrap();
                assert_eq!(pointer.as_ptr() as usize % 64, 0);
                pointer.as_ptr().write(7);
                Buffer::release(&mut parent, owner.as_ptr()).unwrap();
                assert_eq!(stable.as_ptr().read(), 42);
            }
            assert_eq!(parent.bytes(), initial);
            LIVE.with(|live| assert_eq!(live.get(), 1));
        }
        drop(parent);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }

    #[test]
    fn explicit_arenas_share_the_task_limit_with_parent_and_external_storage() {
        let _limit = Limit::set(8192);
        let mut parent = Arena::default();
        let first = new(&mut parent).unwrap();
        let second = new(&mut parent).unwrap();
        unsafe {
            allocate(first.as_ptr(), 4096, 8).unwrap();
            assert!(allocate(second.as_ptr(), 4096, 8).is_err());
            assert!(parent.allocate(4096, 8).is_err());
            assert!(parent.reserve_external(4096).is_err());
            Buffer::release(&mut parent, first.as_ptr()).unwrap();
            allocate(second.as_ptr(), 4096, 8).unwrap();
            Buffer::release(&mut parent, second.as_ptr()).unwrap();
        }
        assert_eq!(parent.bytes(), 0);
        drop(parent);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }

    #[test]
    fn explicit_arena_failed_creation_and_release_refund_shared_budgets() {
        let mut parent = Arena::default();
        {
            let _limit = Limit::set(std::mem::size_of::<Region>());
            assert!(new(&mut parent).is_err());
            assert_eq!(parent.bytes(), 0);
            let pointer = parent.allocate(1, 1).unwrap();
            unsafe { parent.release(pointer) };
        }
        let budget = std::sync::Arc::new(Budget {
            bytes: (0).into(),
            limit: (8192).into(),
            ..Default::default()
        });
        let mut parent = Arena::with_budget(budget.clone(), None);
        let owner = new(&mut parent).unwrap();
        unsafe {
            let before = budget.bytes.get();
            budget.limit.set(before + 128);
            assert!(allocate(owner.as_ptr(), 256, 8).is_err());
            assert_eq!(budget.bytes.get(), before);
            budget.limit.set(8192);
            allocate(owner.as_ptr(), 4096, 8).unwrap();
            assert!(budget.bytes.get() > 4096);
            Buffer::release(&mut parent, owner.as_ptr()).unwrap();
        }
        assert_eq!(parent.bytes(), 0);
        assert_eq!(budget.bytes.get(), 0);
        drop(parent);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }

    #[test]
    fn explicit_arena_preserves_invocation_metadata_and_sibling_account_limits() {
        let budget = std::sync::Arc::new(Budget {
            bytes: (0).into(),
            limit: (32768).into(),
            ..Default::default()
        });
        let mut parent = Arena::with_budget(budget.clone(), Some(8192));
        parent.credit_imported();
        let mut metadata_owner = Buffer::allocate(&mut parent, 0, 0, 1).unwrap();
        unsafe { metadata_owner.as_mut().reserve_metadata(4096).unwrap() };
        let first = new(&mut parent).unwrap();
        let second = new(&mut parent).unwrap();
        unsafe {
            assert!(allocate(first.as_ptr(), 4096, 8).is_err());
            allocate(first.as_ptr(), 2048, 8).unwrap();
            assert!(allocate(second.as_ptr(), 2048, 8).is_err());
            Buffer::release(&mut parent, first.as_ptr()).unwrap();
            allocate(second.as_ptr(), 2048, 8).unwrap();
            Buffer::release(&mut parent, second.as_ptr()).unwrap();
            Buffer::release(&mut parent, metadata_owner.as_ptr()).unwrap();
        }
        assert_eq!(parent.bytes(), 0);
        assert_eq!(budget.bytes.get(), 0);
        drop(parent);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
