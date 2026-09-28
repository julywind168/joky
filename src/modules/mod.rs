//! Module bindings joining core metadata to optional native implementations.
//!
//! This composition layer is independent of the standard library and backends.
//! A registry is built before use and shared immutably for compilation/reload.
use crate::{
    core::resolver::{BindingRegistry, BindingSpec},
    parser::ast::{Item, Module},
    runtime::bindings::{HostBinding, HostEntry},
    Error, ErrorCode, ErrorPhase, Type, TypeRegistry,
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
                    HostEntry::Direct(ptr) if ptr.is_null() => {
                        return Err(abi_error(format!(
                            "Binding `{}` has a null entry",
                            spec.name
                        )));
                    }
                    HostEntry::Cps {
                        frame_slots: Some(slots),
                        ..
                    } if *slots < spec.signature.params.len()
                        || *slots >= i32::MAX as usize / 8 =>
                    {
                        return Err(abi_error(format!(
                            "Binding `{}` has an invalid CPS frame size",
                            spec.name
                        )));
                    }
                    _ => {}
                }
            }
            for ty in spec
                .signature
                .params
                .iter()
                .chain(std::iter::once(&spec.signature.ret))
            {
                if ty.is_aggregate() || matches!(ty, Type::Parameter(_) | Type::Applied(_)) {
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

#[cfg(test)]
mod tests;
