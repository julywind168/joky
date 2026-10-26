//! Synchronous UTF-8 stdout boundary shared by all execution backends.
use crate::{Error, ErrorCode, ErrorPhase};
use std::io::Write;

pub(super) fn binding() -> crate::modules::NativeBinding {
    use crate::runtime::parallel::{Host, HostCode};
    let host = Host {
        name: "__joky_write_stdout".into(),
        signature: crate::FunctionSignature::new(
            vec![crate::Type::reference(crate::Type::String, false)],
            crate::Type::Int32,
        ),
        code: HostCode::TextView(joky_io_write_stdout_v2),
        _owner: std::sync::Arc::new(()),
    };
    let binding = host.binding();
    crate::modules::NativeBinding {
        spec: crate::core::resolver::BindingSpec::with_symbol(
            host.name,
            host.signature,
            "joky_io_write_stdout_v2",
        ),
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
    let result = std::panic::catch_unwind(|| {
        let text = crate::runtime::string::borrow(text)?;
        std::io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(|error| {
                Error::new_with_code(ErrorPhase::Runtime, ErrorCode::IoError, error.to_string())
            })
    })
    .unwrap_or_else(|_| {
        Err(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::Panic,
            "Stdout write panicked",
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
    }
}
