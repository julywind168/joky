//! Module bindings joining core metadata to optional native implementations.
//!
//! This composition layer is independent of the standard library and backends.
//! A registry is built before use and shared immutably for compilation/reload.
use crate::{
    core::resolver::{BindingRegistry, BindingSpec},
    parser::ast::{Item, Module},
    runtime::bindings::{HostAbiKind, HostBinding, HostEntry},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Type, TypeRegistry,
};
use std::{any::Any, collections::HashMap, rc::Rc};

/// One binding's authoritative declaration and optional in-process entry.
/// An absent entry is useful for AOT imports resolved by the external linker.
#[derive(Clone)]
pub(crate) struct NativeBinding {
    pub(crate) spec: BindingSpec,
    pub(crate) entry: Option<HostEntry>,
    pub(crate) owner: Option<Rc<dyn Any>>,
}

impl NativeBinding {
    pub(crate) fn host(&self) -> Option<HostBinding> {
        Some(HostBinding {
            abi: if self.spec.signature.has_text() {
                HostAbiKind::TextViewV2
            } else {
                HostAbiKind::Scalar
            },
            signature: self.spec.signature.clone(),
            entry: self.entry.clone()?,
            owner: self.owner.clone(),
        })
    }
}

#[derive(Clone, Default)]
pub(crate) struct ModuleRegistry {
    native: HashMap<String, NativeBinding>,
    types: TypeRegistry,
}

impl ModuleRegistry {
    /// Atomically register declarations and their implementations.
    ///
    /// # Safety
    /// Each supplied entry must obey its signature and direct/CPS ABI. Its owner
    /// must keep it and its dependencies alive; ownerless entries must be static.
    /// Entries must not unwind across generated code. String views follow the
    /// borrowed ABI in `runtime::string` and must outlive all retaining callers.
    pub(crate) unsafe fn register_module(
        &mut self,
        path: impl Into<String>,
        bindings: impl IntoIterator<Item = NativeBinding>,
    ) -> Result<(), Error> {
        let path = path.into();
        let bindings: Vec<_> = bindings.into_iter().collect();
        // `types.hosts` is the single authoritative binding registry.
        let mut types = self.types.clone();
        let mut metadata = types.hosts.clone();
        metadata.register_module(path.clone(), bindings.iter().map(|b| b.spec.clone()))?;
        for binding in &bindings {
            let spec = &binding.spec;
            if let Some(entry) = &binding.entry {
                if spec.suspends != matches!(entry, HostEntry::Cps { .. }) {
                    return Err(abi_error(format!(
                        "Binding `{}` has an incompatible direct/CPS ABI",
                        spec.name
                    )));
                }
                match entry {
                    HostEntry::C(_) | HostEntry::BorrowingCps(_) | HostEntry::TextFfi(_) => {
                        return Err(abi_error(
                            "C and borrowing imports require their dedicated VM registration",
                        ))
                    }
                    HostEntry::Direct(ptr) if ptr.is_null() => {
                        return Err(abi_error(format!(
                            "Binding `{}` has a null entry",
                            spec.name
                        )));
                    }
                    HostEntry::Cps {
                        frame_slots: Some(slots),
                        ..
                    } if !frame_slots_valid(*slots, spec.signature.params.len()) => {
                        return Err(abi_error(format!(
                            "Binding `{}` has an invalid CPS frame size",
                            spec.name
                        )));
                    }
                    _ => {}
                }
            }
            let mut wire = spec.signature.clone();
            if let Some(
                builtin @ (crate::core::builtins::Builtin::Vm(_)
                | crate::core::builtins::Builtin::Stdout
                | crate::core::builtins::Builtin::Stderr
                | crate::core::builtins::Builtin::TryStderr),
            ) = crate::core::builtins::Builtin::lookup(&spec.name)
            {
                builtin.validate(&spec.signature, spec.suspends, false)?;
                let output_symbol = match builtin {
                    crate::core::builtins::Builtin::Stdout => {
                        Some(crate::core::builtins::STDOUT_SYMBOL)
                    }
                    crate::core::builtins::Builtin::Stderr => {
                        Some(crate::core::builtins::STDERR_SYMBOL)
                    }
                    crate::core::builtins::Builtin::TryStderr => {
                        Some(crate::core::builtins::TRY_STDERR_SYMBOL)
                    }
                    _ => None,
                };
                if output_symbol.is_some()
                    && (spec.symbol.as_deref() != output_symbol
                        || binding.entry.is_some()
                            && !binding
                                .host()
                                .as_ref()
                                .and_then(crate::runtime::parallel::Host::from_binding)
                                .is_some_and(|host| host.is_output() && host.name == spec.name))
                {
                    return Err(abi_error(
                        "Output binding requires its checked TextViewV2 entry",
                    ));
                }
                for ty in &mut wire.params {
                    if let Some(r) = ty.reference_type().filter(|r| !r.mutable) {
                        if let Type::Resource(name) = &r.target {
                            types.resource_type(name)?;
                            *ty = Type::Named(name.to_string());
                        }
                    }
                }
            }
            if matches!(binding.entry, Some(HostEntry::ResourceFactory(_))) {
                let Type::Resource(name) = &spec.signature.ret else {
                    return Err(abi_error(
                        "Resource factory must return a registered resource",
                    ));
                };
                types.resource_type(name)?;
                if spec.signature.params != [Type::UInt64] || spec.suspends {
                    return Err(abi_error("Resource factory requires UInt64 -> Resource"));
                }
                types
                    .resource_factories
                    .insert(spec.name.clone(), spec.signature.clone());
            }
            validate_text_host_types(&mut types, &wire, spec.suspends)?;
            for name in spec
                .signature
                .params
                .iter()
                .chain(std::iter::once(&spec.signature.ret))
                .filter_map(|ty| match ty {
                    Type::Named(name) => Some(name),
                    _ => None,
                })
            {
                let short = name.rsplit('.').next().unwrap();
                let exports = types.module_types.entry(path.clone()).or_default();
                if exports.get(short).is_some_and(|old| old != name) {
                    return Err(abi_error(format!(
                        "Module '{path}' exports conflicting types named '{short}'"
                    )));
                }
                exports.insert(short.into(), name.clone());
            }
        }
        types.hosts = metadata;
        self.types = types;
        for binding in bindings {
            self.native.insert(binding.spec.name.clone(), binding);
        }
        Ok(())
    }

    pub(crate) fn metadata(&self) -> &BindingRegistry {
        &self.types.hosts
    }

    pub(crate) fn types(&self) -> &TypeRegistry {
        &self.types
    }

    pub(crate) fn binding(&self, name: &str) -> Option<&NativeBinding> {
        self.native.get(name)
    }

    /// Merge generated native CPS layouts with caller-supplied AOT layouts.
    pub(crate) fn import_layouts(
        &self,
        ast: &Module,
        explicit: &HashMap<String, usize>,
    ) -> Result<HashMap<String, usize>, Error> {
        let mut layouts = explicit.clone();
        for item in &ast.items {
            let Item::ExternFn(f) = item else { continue };
            let Some(NativeBinding {
                entry:
                    Some(HostEntry::Cps {
                        frame_slots: Some(slots),
                        ..
                    }),
                ..
            }) = self.binding(&f.name)
            else {
                continue;
            };
            if layouts.get(&f.name).is_some_and(|count| count != slots) {
                return Err(
                    abi_error(format!("Conflicting CPS frame size for `{}`", f.name))
                        .with_span(f.span),
                );
            }
            layouts.insert(f.name.clone(), *slots);
        }
        Ok(layouts)
    }
}

fn abi_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}

/// Host-ABI constraints shared by module registration and per-engine host
/// functions: signatures are scalar-only, and opaque named types register on
/// first use. Callers keep their own entry-point and conflict diagnostics.
pub(crate) fn validate_host_types(
    types: &mut TypeRegistry,
    signature: &FunctionSignature,
) -> Result<(), Error> {
    for ty in signature
        .params
        .iter()
        .chain(std::iter::once(&signature.ret))
    {
        if ty.contains_dyn() {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Dyn views cannot cross a host ABI",
            ));
        }
        if *ty == Type::Rune {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Rune cannot cross an unchecked host ABI; use UInt32 and Rune.from_u32",
            ));
        }
        if ty.is_aggregate()
            || matches!(
                ty,
                Type::Ptr
                    | Type::Reference(_)
                    | Type::Buffer(_)
                    | Type::BufferElement(_)
                    | Type::BufferSlice(_)
                    | Type::BufferIterator(_)
                    | Type::BufferPairIterator(_)
                    | Type::BufferEntry(_)
                    | Type::Parameter(_)
                    | Type::Applied(_)
            )
        {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Aggregates and type parameters are not supported by the host ABI",
            ));
        }
        if let Type::Named(name) = ty {
            if !types.contains(name) {
                types.register_opaque(name.clone())?;
            }
        }
    }
    Ok(())
}

/// A CPS frame must cover every parameter and stay within the slot encoding.
pub(crate) fn frame_slots_valid(slots: usize, params: usize) -> bool {
    slots >= params && slots < i32::MAX as usize / 8
}

#[cfg(test)]
mod tests;

/// Explicit text descriptor signatures have a separately checked wire representation.
pub(crate) fn validate_text_host_types(
    types: &mut TypeRegistry,
    signature: &FunctionSignature,
    suspends: bool,
) -> Result<(), Error> {
    let mut wire = signature.clone();
    for ty in &mut wire.params {
        if ty
            .reference_type()
            .is_some_and(|r| !r.mutable && r.target == Type::String)
            && !suspends
        {
            *ty = Type::String;
        }
    }
    validate_host_types(types, &wire)
}
