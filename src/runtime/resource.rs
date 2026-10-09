//! Versioned external resource ownership, independent of language containers.
use super::{arena::Arena, buffer::Buffer};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{ffi::c_void, ptr::NonNull};

pub type ResourceCreateV1 = unsafe extern "C" fn(u64, *mut *mut c_void) -> i32;
pub type ResourceReleaseV1 = unsafe extern "C" fn(*mut c_void);

/// Synchronous factory for a registered resource type (UInt64 -> Resource).
/// Callbacks run on the task thread and must not unwind. Every nonnull output
/// transfers exactly one release obligation, including on failure. Successful
/// calls return zero and a nonnull token. release must be infallible and must not
/// access Joky storage. The descriptor and callback code outlive all resources.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ResourceFactoryV1 {
    pub abi_version: u32,
    pub create: Option<ResourceCreateV1>,
    pub release: Option<ResourceReleaseV1>,
}
impl ResourceFactoryV1 {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.abi_version != 1 || self.create.is_none() || self.release.is_none() {
            return Err(error("Invalid resource factory v1 descriptor"));
        }
        Ok(())
    }
}
pub(crate) fn validate_name(name: &str) -> Result<(), Error> {
    if name.starts_with("__joky_")
        || name.is_empty()
        || !name
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    {
        return Err(error("Invalid resource factory name"));
    }
    Ok(())
}
fn error(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}
pub(crate) struct OwnedResource {
    token: NonNull<c_void>,
    release: ResourceReleaseV1,
}
impl Drop for OwnedResource {
    fn drop(&mut self) {
        // SAFETY: Adoption owns exactly one callback obligation; no copies escape.
        unsafe {
            (self.release)(self.token.as_ptr());
        }
    }
}
pub(crate) fn symbol(name: &str) -> String {
    format!("__joky_resource_factory_v1_{name}")
}

/// # Safety
/// Factory callbacks obey ResourceFactoryV1 and arena is exclusively live.
pub(crate) unsafe fn invoke(
    arena: &mut Arena,
    factory: &ResourceFactoryV1,
    input: u64,
) -> Result<NonNull<Buffer>, Error> {
    factory.validate()?;
    let mut token = std::ptr::null_mut();
    let result = super::error::checked(|| factory.create.unwrap()(input, &mut token));
    let resource = NonNull::new(token).map(|token| OwnedResource {
        token,
        release: factory.release.unwrap(),
    });
    if result? != 0 {
        return Err(error("Resource factory failed"));
    }
    let resource = resource.ok_or_else(|| error("Resource factory returned a null owner"))?;
    Buffer::external(arena, resource)
}

#[export_name = "__joky_resource_create_v1"]
unsafe extern "C" fn create(
    arena: *mut Arena,
    factory: *const ResourceFactoryV1,
    input: u64,
) -> *mut Buffer {
    match invoke(&mut *arena, &*factory, input) {
        Ok(owner) => owner.as_ptr(),
        Err(error) => {
            super::error::record_error(error);
            std::ptr::null_mut()
        }
    }
}
pub(crate) fn symbols() -> [(&'static str, *const u8); 1] {
    [("__joky_resource_create_v1", create as *const u8)]
}
