//! Resource ownership and non-retaining adapters for the source VM library.
use super::*;
use crate::runtime::{buffer::Buffer, resource::ResourceFactoryV1};
use std::{cell::Cell, ffi::c_void};

struct Owner {
    handle: Cell<Option<u64>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            shutdown(handle);
        }
    }
}
fn shutdown(handle: u64) {
    let state = VMS.with(|vms| vms.borrow_mut().remove(&handle));
    if let Some(state) = state {
        state.close();
    }
}
unsafe extern "C" fn create(mode: u64, output: *mut *mut c_void) -> i32 {
    let handle = super::create_with_mode(if mode == 0 {
        VmMode::Jit
    } else {
        VmMode::Interpreter
    });
    if handle == 0 {
        return 1;
    }
    *output = Box::into_raw(Box::new(Owner {
        handle: Cell::new(Some(handle)),
    }))
    .cast();
    0
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Owner>()));
}
#[export_name = "__joky_resource_factory_v1_vm_create"]
pub(crate) static FACTORY: ResourceFactoryV1 = ResourceFactoryV1 {
    abi_version: 1,
    create: Some(create),
    release: Some(release),
};
/// # Safety
/// bits points to a live VM resource for this synchronous access. No lease escapes.
pub(super) unsafe fn handle(bits: u64) -> Result<u64, Error> {
    let resource = (bits as *mut Buffer)
        .as_mut()
        .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "Null VM resource"))?;
    let lease = resource.resource_mut()?.lease()?;
    (&*lease.token().cast::<Owner>())
        .handle
        .get()
        .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "VM resource is closed"))
}
#[export_name = "__joky_vm_managed_close"]
pub(crate) unsafe extern "C" fn close(bits: u64) -> i32 {
    operation(1, || {
        let resource = (bits as *mut Buffer)
            .as_mut()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "Null VM resource"))?;
        let lease = resource.resource_mut()?.lease()?;
        let handle = (&*lease.token().cast::<Owner>()).handle.take();
        drop(lease);
        if let Some(handle) = handle {
            shutdown(handle);
        }
        Ok(0)
    })
}
pub(crate) use super::{error_code, print_error};
// The short lease ends before entering the VM, permitting reentrant calls and reload.
macro_rules! adapters {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty = $fallback:expr, $symbol:literal;)*) => {$ (
        /// # Safety
        /// The resource, text descriptors and function entries obey the VM ABI.
        #[export_name = $symbol]
        pub(crate) unsafe extern "C" fn $name(owner: u64, $($arg: $ty),*) -> $ret {
            match handle(owner) {
                Ok(id) => super::$name(id, $($arg),*),
                Err(error) => operation($fallback, || Err(error)),
            }
        }
    )*};
}
adapters! {
    domain_schema() -> u64 = 0, "__joky_vm_managed_domain_schema";
    prepare_migration(schema: u64, source: *const u8, method: *const u8) -> i32 = 1, "__joky_vm_managed_prepare_migration_v1";
    begin_migration(schema: u64, source: *const u8, start: *const u8, advance: *const u8) -> i32 = 1, "__joky_vm_managed_begin_migration_v1";
    step_migration(budget: u64) -> i32 = -1, "__joky_vm_managed_step_migration";
    begin_migration_batch(source: *const u8) -> i32 = 1, "__joky_vm_managed_begin_migration_batch_v1";
    stage_migration(start: *const u8, advance: *const u8) -> i32 = 1, "__joky_vm_managed_stage_migration_v1";
    publish_migration() -> i32 = 1, "__joky_vm_managed_publish_migration";
    cancel_migration() -> i32 = 1, "__joky_vm_managed_cancel_migration";
    module_count() -> i32 = -1, "__joky_vm_managed_module_count";
    print_modules() -> i32 = -1, "__joky_vm_managed_print_modules";
    publish_domain_code() -> i32 = -1, "__joky_vm_managed_publish_domain_code";
    load_source(source: *const u8) -> i32 = 1, "__joky_vm_managed_load_source_v2";
    reload_source(source: *const u8) -> i32 = 1, "__joky_vm_managed_reload_source_v2";
    register_int32_0(name: *const u8, function: *const u8) -> i32 = 1, "__joky_vm_managed_register_int32_0_v2";
    register_int32_1(name: *const u8, function: *const u8) -> i32 = 1, "__joky_vm_managed_register_int32_1_v2";
    register_int32_2(name: *const u8, function: *const u8) -> i32 = 1, "__joky_vm_managed_register_int32_2_v2";
    call_int32_0(name: *const u8) -> i32 = 0, "__joky_vm_managed_call_int32_0_v2";
    call_int32_1(name: *const u8, arg0: i32) -> i32 = 0, "__joky_vm_managed_call_int32_1_v2";
    call_int32_2(name: *const u8, arg0: i32, arg1: i32) -> i32 = 0, "__joky_vm_managed_call_int32_2_v2";
    call_float64_0(name: *const u8) -> f64 = 0.0, "__joky_vm_managed_call_float64_0_v2";
    call_float64_1(name: *const u8, arg0: f64) -> f64 = 0.0, "__joky_vm_managed_call_float64_1_v2";
    call_float64_2(name: *const u8, arg0: f64, arg1: f64) -> f64 = 0.0, "__joky_vm_managed_call_float64_2_v2";
}
