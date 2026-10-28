//! Compiler-owned runtime contracts and external adapter selection.
use super::resolver::BindingSpec;
use crate::{
    parser::ast::{ExternFnDecl, Item},
    runtime::{byte_request::ByteIoKind, domain, tcp},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, TypeRegistry,
};

/// Fixed runtime operations are recognized once, before backend selection.
/// Registered VM operations use host entries; typed calls and other families need adapters.
#[derive(Clone, Debug)]
pub(crate) enum Builtin {
    Vm(Box<BindingSpec>),
    VmCall,
    VmSource,
    VmMigration(super::vm::MigrationOperation),
    Tcp(tcp::Operation),
    Dns,
    Fs(crate::runtime::fs::Operation),
    Domain(domain::Operation),
}

impl Builtin {
    pub(crate) fn lookup(name: &str) -> Option<Self> {
        if name == "__vm_call" {
            Some(Self::VmCall)
        } else if name == "__vm_source" {
            Some(Self::VmSource)
        } else if let Some(op) = super::vm::MigrationOperation::lookup(name) {
            Some(Self::VmMigration(op))
        } else if let Some(op) = tcp::operation(name) {
            Some(Self::Tcp(op))
        } else if let Some(op) = crate::runtime::fs::operation(name) {
            Some(Self::Fs(op))
        } else if crate::runtime::dns::is_lookup(name) {
            Some(Self::Dns)
        } else if let Some(op) = domain::operation(name) {
            Some(Self::Domain(op))
        } else {
            super::vm::binding(name).map(|spec| Self::Vm(Box::new(spec)))
        }
    }

    pub(crate) fn needs_adapter(&self) -> bool {
        !matches!(self, Self::Vm(_))
    }

    pub(crate) fn validate(
        &self,
        signature: &FunctionSignature,
        suspends: bool,
        c_abi: bool,
    ) -> Result<(), Error> {
        match self {
            Self::VmCall | Self::VmSource | Self::VmMigration(_) => {
                let expected = if let Self::VmMigration(op) = self {
                    op.signature()
                } else if matches!(self, Self::VmSource) {
                    super::vm::source_signature()
                } else {
                    super::vm::call_signature()
                };
                if c_abi || !suspends || *signature != expected {
                    return Err(Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError,
                        "VM call requires its exact compiler-owned signature and suspension convention"));
                }
                Ok(())
            }
            Self::Vm(spec) => {
                if c_abi || spec.signature != *signature || spec.suspends != suspends {
                    return Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        "VM runtime adapter requires its registered signature and suspension convention",
                    ));
                }
                Ok(())
            }
            _ if c_abi => Err(super::ffi::error(
                Span::empty(0),
                "TCP runtime names require checked extern declarations",
            )),
            Self::Tcp(op) => op.validate(signature, suspends),
            Self::Dns => crate::runtime::dns::validate(signature, suspends),
            Self::Fs(op) => op.validate(signature, suspends),
            Self::Domain(op) => op.validate(signature, suspends),
        }
    }

    pub(crate) fn validate_declaration(&self, item: &Item) -> Result<(), Error> {
        let Item::ExternFn(f) = item else {
            let Item::FnDef(f) = item else { unreachable!() };
            return Err(match self {
                Self::Vm(_) | Self::VmCall | Self::VmSource | Self::VmMigration(_) => {
                    Error::new_with_code(
                        ErrorPhase::TypeCheck,
                        ErrorCode::ReservedName,
                        "VM runtime names require checked extern declarations",
                    )
                    .with_span(f.span)
                }
                _ => super::ffi::error(
                    f.span,
                    "TCP runtime names require checked extern declarations",
                ),
            });
        };
        self.validate_extern(f)
    }

    pub(crate) fn validate_extern(&self, f: &ExternFnDecl) -> Result<(), Error> {
        self.validate(
            &FunctionSignature::new(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                f.ret_ty.clone(),
            ),
            f.suspends,
            f.c_abi,
        )
        .map_err(|e| e.with_span(f.span))
    }

    /// Extra result lanes beyond the parameter portion of an adapter frame.
    pub(crate) fn result_slots(&self) -> usize {
        match self {
            Self::Domain(op) if op.status_payload() => 2,
            Self::Domain(domain::Operation::OutboxMessageBytes) | Self::Dns => 1,
            Self::Tcp(op) if op.is_read() => 1,
            Self::Fs(op) if op.returns_bytes() => 1,
            _ => 0,
        }
    }
}

/// External registration ABI is independent of the compiler-owned contract.
/// For example, __vm_create is both a VM builtin and a resource factory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternAbi {
    Scalar,
    TextView,
    TextFfi,
    ResourceFactory,
    ByteIo(ByteIoKind),
    C,
    Borrowing,
}

impl ExternAbi {
    pub(crate) fn classify(
        f: &ExternFnDecl,
        types: &TypeRegistry,
        builtin: Option<&Builtin>,
    ) -> Self {
        if let Some(kind) = types.resource_io.get(&f.name) {
            Self::ByteIo(*kind)
        } else if types.resource_factories.contains_key(&f.name) {
            Self::ResourceFactory
        } else if types.text_ffi.contains_key(&f.name) {
            Self::TextFfi
        } else if f.c_abi {
            Self::C
        } else if builtin.is_none() && super::ffi::is_borrowing_import(f) {
            Self::Borrowing
        } else if !builtin.is_some_and(Builtin::needs_adapter)
            && FunctionSignature::new(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                f.ret_ty.clone(),
            )
            .has_text()
        {
            Self::TextView
        } else {
            Self::Scalar
        }
    }

    pub(crate) fn validate(
        self,
        f: &ExternFnDecl,
        signature: &FunctionSignature,
        nominals: &std::collections::HashMap<String, crate::Type>,
        types: &TypeRegistry,
    ) -> Result<(), Error> {
        if self.is_resource()
            && (f.suspends != types.resource_async.contains(&f.name)
                || f.c_abi
                || types.resource_factories.get(&f.name) != Some(signature))
        {
            return Err(super::ffi::error(
                f.span,
                "Resource factory requires its registered signature and suspension convention",
            ));
        }
        if self == ExternAbi::TextFfi {
            crate::core::ffi::validate_text_signature(signature)?;
            if f.suspends || f.c_abi || types.text_ffi.get(&f.name) != Some(signature) {
                return Err(super::ffi::error(
                    f.span,
                    "Text FFI registration requires a matching synchronous extern fn declaration",
                ));
            }
        }
        if self == ExternAbi::Borrowing {
            super::ffi::validate_borrowing_signature(signature, nominals, types, f.span)?;
        } else if f.c_abi {
            super::ffi::validate_signature(signature, f.suspends, nominals, types, f.span)?;
        }
        Ok(())
    }

    pub(crate) fn is_resource(self) -> bool {
        matches!(self, Self::ResourceFactory | Self::ByteIo(_))
    }

    pub(crate) fn byte_io(self) -> Option<ByteIoKind> {
        match self {
            Self::ByteIo(kind) => Some(kind),
            _ => None,
        }
    }

    pub(crate) fn has_text(self) -> bool {
        matches!(self, Self::TextView | Self::TextFfi)
    }
}
