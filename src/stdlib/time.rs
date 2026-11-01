//! Scalar clock boundaries. Instants use the same clock domain as task timers.
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Clock,
    Ticks,
    UnixMicros,
}
impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Clock => "clock",
            Self::Ticks => "ticks",
            Self::UnixMicros => "unix_micros",
        }
    }
    pub(crate) fn signature(self) -> FunctionSignature {
        FunctionSignature::new(
            if matches!(self, Self::Ticks) {
                vec![Type::UInt64]
            } else {
                vec![]
            },
            if matches!(self, Self::UnixMicros) {
                Type::Int64
            } else {
                Type::UInt64
            },
        )
    }
    pub(crate) fn entry(self) -> *const u8 {
        match self {
            Self::Clock => joky_time_clock as *const u8,
            Self::Ticks => joky_time_ticks as *const u8,
            Self::UnixMicros => joky_time_unix_micros as *const u8,
        }
    }
}
pub(super) fn bindings() -> Vec<crate::modules::NativeBinding> {
    [Operation::Clock, Operation::Ticks, Operation::UnixMicros]
        .into_iter()
        .map(|op| {
            let host = crate::runtime::parallel::Host {
                name: format!("__joky_time_{}", op.name()),
                signature: op.signature(),
                code: crate::runtime::parallel::HostCode::Time(op),
                _owner: std::sync::Arc::new(()),
            };
            let binding = host.binding();
            crate::modules::NativeBinding {
                spec: crate::core::resolver::BindingSpec::with_symbol(
                    host.name,
                    host.signature,
                    format!("joky_time_{}", op.name()),
                ),
                entry: Some(binding.entry),
                owner: binding.owner,
            }
        })
        .collect()
}
fn result<T: Default>(result: Result<T, Error>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            crate::runtime::error::record_error(error);
            T::default()
        }
    }
}
#[no_mangle]
pub extern "C" fn joky_time_clock() -> u64 {
    result(crate::runtime::executor::time_clock())
}
#[no_mangle]
pub extern "C" fn joky_time_ticks(clock: u64) -> u64 {
    result(crate::runtime::executor::time_ticks(clock))
}
#[no_mangle]
pub extern "C" fn joky_time_unix_micros() -> i64 {
    let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(value) => value.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    };
    result(i64::try_from(nanos.div_euclid(1000)).map_err(|_| {
        Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::InvalidState,
            "Wall clock exceeds the timestamp range",
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        bindings::{HostAbiKind, HostEntry},
        parallel::Host,
    };

    #[test]
    fn time_points_parallel_evidence_requires_exact_scalar_entry() {
        for native in bindings() {
            let binding = native.host().unwrap();
            assert!(Host::from_binding(&binding).is_some());
            let mut changed = binding.clone();
            changed.owner = None;
            assert!(Host::from_binding(&changed).is_none());
            let mut changed = binding.clone();
            changed.abi = HostAbiKind::TextViewV2;
            assert!(Host::from_binding(&changed).is_none());
            let mut changed = binding.clone();
            changed.signature.ret = Type::Unit;
            assert!(Host::from_binding(&changed).is_none());
            let mut changed = binding;
            changed.entry = HostEntry::Direct(std::ptr::null());
            assert!(Host::from_binding(&changed).is_none());
        }
    }
}
