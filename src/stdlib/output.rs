//! Synchronous UTF-8 standard output boundaries shared by all execution backends.
use crate::{Error, ErrorCode, ErrorPhase};
use std::io::Write;

pub(super) fn binding() -> crate::modules::NativeBinding {
    output_binding(false)
}

pub(super) fn stderr_binding() -> crate::modules::NativeBinding {
    output_binding(true)
}

fn output_binding(stderr: bool) -> crate::modules::NativeBinding {
    use crate::runtime::parallel::{Host, HostCode};
    let (name, symbol, entry) = if stderr {
        (
            crate::core::builtins::STDERR_NAME,
            crate::core::builtins::STDERR_SYMBOL,
            joky_io_write_stderr_v2 as unsafe extern "C" fn(*const u8) -> i32,
        )
    } else {
        (
            crate::core::builtins::STDOUT_NAME,
            crate::core::builtins::STDOUT_SYMBOL,
            joky_io_write_stdout_v2 as unsafe extern "C" fn(*const u8) -> i32,
        )
    };
    let host = Host {
        name: name.into(),
        signature: crate::core::builtins::stdout_signature(),
        code: HostCode::TextView(entry),
        _owner: std::sync::Arc::new(()),
    };
    let binding = host.binding();
    crate::modules::NativeBinding {
        spec: crate::core::resolver::BindingSpec::with_symbol(host.name, host.signature, symbol),
        entry: Some(binding.entry),
        owner: binding.owner,
    }
}

/// # Safety
/// `text` is a live immutable TextViewV2 descriptor and UTF-8 payload for this call.
/// Calls may run concurrently: the descriptor is borrowed only until return,
/// stdout is locked for the entire write, and failures use worker-local runtime
/// error state. No VM, local registry, callback or input pointer is retained.
#[no_mangle]
pub unsafe extern "C" fn joky_io_write_stdout_v2(text: *const u8) -> i32 {
    write(text, false)
}

/// Write and flush stderr using the same borrowed TextViewV2 contract.
/// # Safety
/// `text` must remain a live immutable descriptor and UTF-8 payload until return.
#[no_mangle]
pub unsafe extern "C" fn joky_io_write_stderr_v2(text: *const u8) -> i32 {
    write(text, true)
}

unsafe fn write(text: *const u8, stderr: bool) -> i32 {
    let result = std::panic::catch_unwind(|| {
        let text = crate::runtime::string::borrow(text)?;
        fn emit(mut output: impl Write, text: &str, stderr: bool) -> std::io::Result<()> {
            // Acquire the real stream before the capture lock, including when
            // an embedding caller already holds its reentrant stdio lock.
            crate::runtime::fs::write_output(stderr, text, || {
                output.write_all(text.as_bytes())?;
                output.flush()
            })
        }
        let result = if stderr {
            emit(std::io::stderr().lock(), text, true)
        } else {
            emit(std::io::stdout().lock(), text, false)
        };
        result.map_err(|error| {
            Error::new_with_code(ErrorPhase::Runtime, ErrorCode::IoError, error.to_string())
        })
    })
    .unwrap_or_else(|_| {
        Err(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::Panic,
            "Output write panicked",
        ))
    });
    match result {
        Ok(()) => 0,
        Err(error) => {
            crate::runtime::error::record_error(error);
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        bindings::{HostAbiKind, HostEntry},
        parallel::Host,
    };
    #[test]
    fn parallel_stdout_evidence_checks_binding_identity_and_abi() {
        let original = binding().host().unwrap();
        assert!(Host::from_binding(&original).is_some());
        let mut changed = original.clone();
        changed.owner = None;
        assert!(Host::from_binding(&changed).is_none());
        let mut changed = original.clone();
        changed.abi = HostAbiKind::Scalar;
        assert!(Host::from_binding(&changed).is_none());
        let mut changed = original.clone();
        changed.signature.ret = crate::Type::Unit;
        assert!(Host::from_binding(&changed).is_none());
        let mut changed = original;
        changed.entry = HostEntry::Direct(std::ptr::null());
        assert!(Host::from_binding(&changed).is_none());

        unsafe extern "C" fn impostor(_: *const u8) -> i32 {
            0
        }
        let mut host = Host::from_binding(&binding().host().unwrap()).unwrap();
        host.code = crate::runtime::parallel::HostCode::TextView(impostor);
        assert!(Host::from_binding(&host.binding()).is_none());
        // A valid generic host certificate cannot be renamed into the builtin.
        host.name = "user_output".into();
        let fake = host.binding();
        assert!(Host::from_binding(&fake).is_some());
        let mut renamed = binding();
        renamed.entry = Some(fake.entry);
        renamed.owner = fake.owner;
        assert!(unsafe {
            crate::modules::ModuleRegistry::default().register_module("fake", [renamed])
        }
        .is_err());
        let mut symbol = binding();
        symbol.spec.symbol = Some("user_output".into());
        assert!(unsafe {
            crate::modules::ModuleRegistry::default().register_module("fake", [symbol])
        }
        .is_err());
        let mut stderr = stderr_binding().host().unwrap();
        assert!(Host::from_binding(&stderr).is_some());
        stderr.entry = HostEntry::Direct(joky_io_write_stdout_v2 as *const u8);
        assert!(Host::from_binding(&stderr).is_none());
        let mut renamed = stderr_binding();
        renamed.spec.symbol = Some(crate::core::builtins::STDOUT_SYMBOL.into());
        assert!(unsafe {
            crate::modules::ModuleRegistry::default().register_module("fake", [renamed])
        }
        .is_err());
    }

    #[test]
    fn parallel_stdout_declarations_require_the_fixed_synchronous_abi() {
        use crate::core::builtins::{stdout_signature, Builtin, STDOUT_NAME};
        let builtin = Builtin::lookup(STDOUT_NAME).unwrap();
        let signature = stdout_signature();
        assert!(!builtin.needs_adapter());
        assert!(builtin.validate(&signature, false, false).is_ok());
        assert!(builtin.validate(&signature, true, false).is_err());
        assert!(builtin.validate(&signature, false, true).is_err());
        for ty in [
            crate::Type::String,
            crate::Type::reference(crate::Type::String, true),
            crate::Type::Ptr,
        ] {
            let bad = crate::FunctionSignature::new(vec![ty], crate::Type::Int32);
            assert!(builtin.validate(&bad, false, false).is_err());
        }
        let mut vm = crate::vm::VmEngine::new();
        vm.set_prelude_enabled(false);
        for source in [
            "extern fn __joky_write_stdout(text: &String) @suspends -> Int32",
            "extern fn __joky_write_stdout(text: &mut String) -> Int32",
            "extern fn __joky_write_stdout(text: &String)",
            "fn __joky_write_stdout(text: &String) -> Int32 { 0 }",
        ] {
            assert!(vm.load_source(source).is_err(), "{source}");
        }
    }
}
