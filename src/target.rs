//! Validated target configuration for AOT compilation.

/// Only the compiler's own target is supported until target-aware layout,
/// foreign bindings, runtime archives and linking are implemented together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AotTarget {
    triple: &'static str,
    pointer_width: u8,
}

impl AotTarget {
    pub fn native() -> Self {
        Self {
            triple: env!("JOKY_TARGET"),
            pointer_width: usize::BITS as u8,
        }
    }

    pub fn from_triple(triple: &str) -> Result<Self, String> {
        let native = Self::native();
        if triple == native.triple {
            Ok(native)
        } else {
            Err(format!(
                "unsupported AOT target '{triple}'; this compiler supports only '{}'; cross-compilation is not yet supported",
                native.triple
            ))
        }
    }

    pub fn triple(self) -> &'static str {
        self.triple
    }

    pub fn pointer_width(self) -> u8 {
        self.pointer_width
    }

    pub fn cpu(self) -> &'static str {
        "native"
    }

    pub(crate) fn isa_builder(self) -> Result<cranelift_codegen::isa::Builder, String> {
        cranelift_native::builder().map_err(|error| error.to_string())
    }
}

impl Default for AotTarget {
    fn default() -> Self {
        Self::native()
    }
}
