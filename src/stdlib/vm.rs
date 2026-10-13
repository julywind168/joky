//! Native implementation of joky/vm. Handles are thread-affine and never reused.
use crate::{
    runtime::string::borrow,
    vm::{VmEngine, VmMode},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Type,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    io::Write,
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

thread_local! {
    static VMS: RefCell<HashMap<u64, Rc<async_call::HandleState>>> = RefCell::new(HashMap::new());
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn operation<T>(fallback: T, action: impl FnOnce() -> Result<T, Error>) -> T {
    LAST_ERROR.with(|error| *error.borrow_mut() = None);
    let result = catch_unwind(AssertUnwindSafe(action)).unwrap_or_else(|_| {
        Err(runtime_error(
            ErrorCode::Panic,
            "VM runtime operation panicked",
        ))
    });
    match result {
        Ok(value) => {
            // Nested VM operations may set LAST_ERROR while the action is
            // running. Once the outer operation succeeds, its public status
            // must describe that successful operation rather than an error
            // that was handled by the callee.
            LAST_ERROR.with(|error| *error.borrow_mut() = None);
            value
        }
        Err(message) => {
            LAST_ERROR.with(|error| *error.borrow_mut() = Some(message.to_string()));
            fallback
        }
    }
}

fn lookup(handle: u64) -> Result<Rc<async_call::HandleState>, Error> {
    VMS.with(|vms| vms.borrow().get(&handle).cloned())
        .ok_or_else(|| {
            runtime_error(
                ErrorCode::NotFound,
                format!("Invalid or closed VM handle {handle} on this thread"),
            )
        })
}

#[export_name = "joky_vm_create"]
pub extern "C" fn create() -> u64 {
    create_with_mode(VmMode::Jit)
}

#[export_name = "joky_vm_create_interpreter"]
pub extern "C" fn create_interpreter() -> u64 {
    create_with_mode(VmMode::Interpreter)
}

fn create_with_mode(mode: VmMode) -> u64 {
    operation(0, || {
        let handle = NEXT_HANDLE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| runtime_error(ErrorCode::InvalidState, "VM handles exhausted"))?;
        VMS.with(|vms| {
            vms.borrow_mut()
                .insert(handle, Rc::new(async_call::HandleState::new(mode)))
        });
        Ok(handle)
    })
}

#[export_name = "joky_vm_close"]
pub extern "C" fn close(handle: u64) -> i32 {
    operation(1, || {
        // Drop outside the table borrow; active calls retain their own Rc.
        let removed = VMS.with(|vms| vms.borrow_mut().remove(&handle));
        let removed = removed.ok_or_else(|| {
            runtime_error(
                ErrorCode::NotFound,
                format!("Invalid or closed VM handle {handle} on this thread"),
            )
        })?;
        removed.close();
        Ok(0)
    })
}

/// # Safety
/// `source` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_load_source_v2"]
pub unsafe extern "C" fn load_source(handle: u64, source: *const u8) -> i32 {
    operation(1, || {
        let source = unsafe { borrow(source)? };
        let state = lookup(handle)?;
        let mut engine = state.engine.try_borrow_mut().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        engine.load_source(source)?;
        state.remember(&engine);
        Ok(0)
    })
}

/// # Safety
/// `source` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_reload_source_v2"]
pub unsafe extern "C" fn reload_source(handle: u64, source: *const u8) -> i32 {
    unsafe { load_source(handle, source) }
}

fn register_int32(handle: u64, name: &str, entry: *const u8, arity: usize) -> Result<i32, Error> {
    if entry.is_null() {
        return Err(abi_error("Cannot register a null function entry"));
    }
    let signature = FunctionSignature::new(vec![Type::Int32; arity], Type::Int32);
    let owner = crate::stdlib::module_owner(entry);
    let engine = lookup(handle)?;
    let mut engine = engine
        .engine
        .try_borrow_mut()
        .map_err(|_| runtime_error(ErrorCode::InvalidState, "VM is executing another operation"))?;
    unsafe {
        engine.register_owned_fn(name, signature, entry, owner)?;
    }
    Ok(0)
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes. `entry` must be a synchronous
/// `extern "C" fn() -> i32`; the owning generated module is retained when known.
#[export_name = "joky_vm_register_int32_0_v2"]
pub unsafe extern "C" fn register_int32_0(handle: u64, name: *const u8, entry: *const u8) -> i32 {
    operation(1, || {
        register_int32(handle, unsafe { borrow(name)? }, entry, 0)
    })
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes. `entry` must be a synchronous
/// `extern "C" fn(i32) -> i32`; the owning generated module is retained when known.
#[export_name = "joky_vm_register_int32_1_v2"]
pub unsafe extern "C" fn register_int32_1(handle: u64, name: *const u8, entry: *const u8) -> i32 {
    operation(1, || {
        register_int32(handle, unsafe { borrow(name)? }, entry, 1)
    })
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes. `entry` must be a synchronous
/// `extern "C" fn(i32, i32) -> i32`; the owning generated module is retained when known.
#[export_name = "joky_vm_register_int32_2_v2"]
pub unsafe extern "C" fn register_int32_2(handle: u64, name: *const u8, entry: *const u8) -> i32 {
    operation(1, || {
        register_int32(handle, unsafe { borrow(name)? }, entry, 2)
    })
}

fn call(handle: u64, name: &str, args: &[i32]) -> Result<i32, Error> {
    let engine = lookup(handle)?;
    let engine = engine
        .engine
        .try_borrow()
        .map_err(|_| runtime_error(ErrorCode::InvalidState, "VM is executing another operation"))?;
    match args {
        [] => engine.call_int32_0(name),
        [a] => engine.call_int32_1(name, *a),
        [a, b] => engine.call_int32_2(name, *a, *b),
        _ => unreachable!(),
    }
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_call_int32_0_v2"]
pub unsafe extern "C" fn call_int32_0(handle: u64, name: *const u8) -> i32 {
    operation(0, || call(handle, unsafe { borrow(name)? }, &[]))
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_call_int32_1_v2"]
pub unsafe extern "C" fn call_int32_1(handle: u64, name: *const u8, a: i32) -> i32 {
    operation(0, || call(handle, unsafe { borrow(name)? }, &[a]))
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_call_int32_2_v2"]
pub unsafe extern "C" fn call_int32_2(handle: u64, name: *const u8, a: i32, b: i32) -> i32 {
    operation(0, || call(handle, unsafe { borrow(name)? }, &[a, b]))
}

fn call_float64(handle: u64, name: &str, args: &[f64]) -> Result<f64, Error> {
    let engine = lookup(handle)?;
    let engine = engine
        .engine
        .try_borrow()
        .map_err(|_| runtime_error(ErrorCode::InvalidState, "VM is executing another operation"))?;
    match args {
        [] => engine.call_float64_0(name),
        [a] => engine.call_float64_1(name, *a),
        [a, b] => engine.call_float64_2(name, *a, *b),
        _ => unreachable!(),
    }
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_call_float64_0_v2"]
pub unsafe extern "C" fn call_float64_0(handle: u64, name: *const u8) -> f64 {
    operation(0.0, || call_float64(handle, unsafe { borrow(name)? }, &[]))
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_call_float64_1_v2"]
pub unsafe extern "C" fn call_float64_1(handle: u64, name: *const u8, a: f64) -> f64 {
    operation(0.0, || call_float64(handle, unsafe { borrow(name)? }, &[a]))
}

/// # Safety
/// `name` must be an aligned JokyText descriptor with live immutable UTF-8 bytes for this call, on the handle's thread.
#[export_name = "joky_vm_call_float64_2_v2"]
pub unsafe extern "C" fn call_float64_2(handle: u64, name: *const u8, a: f64, b: f64) -> f64 {
    operation(0.0, || {
        call_float64(handle, unsafe { borrow(name)? }, &[a, b])
    })
}

#[export_name = "joky_vm_error_code"]
pub extern "C" fn error_code() -> i32 {
    LAST_ERROR.with(|error| i32::from(error.borrow().is_some()))
}

#[export_name = "joky_vm_print_error"]
pub extern "C" fn print_error() {
    LAST_ERROR.with(|error| {
        if let Some(message) = error.borrow().as_ref() {
            let _ = writeln!(std::io::stderr().lock(), "joky/vm: {message}");
        }
    });
}

fn runtime_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, code, message)
}

fn abi_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}

#[cfg(test)]
mod tests;

mod async_call;
pub(crate) use async_call::{
    begin_migration, call_async_int32, cancel_migration, domain_schema, prepare_migration,
    publish_domain_code, publish_migration, script_poll, step_migration,
};
