//! Built-in binding registry and runtime ownership tracking.
mod vm;

use crate::{
    resolver::{BindingRegistry, BindingSpec},
    FunctionSignature, Type,
};
use std::{
    any::Any,
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
    sync::OnceLock,
};

thread_local! {
    static MODULE_OWNERS: RefCell<HashMap<usize, Weak<dyn Any>>> = RefCell::new(HashMap::new());
    static BINDINGS: RefCell<Option<Vec<Binding>>> = const { RefCell::new(None) };
}

pub(crate) fn track_module(module: &Rc<crate::jit::CompiledModule>) {
    let owner: Rc<dyn Any> = module.clone();
    MODULE_OWNERS.with(|owners| {
        let mut owners = owners.borrow_mut();
        owners.retain(|_, owner| owner.strong_count() != 0);
        for function in module.exports.values() {
            owners.insert(function.entry as usize, Rc::downgrade(&owner));
        }
    });
}

pub(crate) fn module_owner(entry: *const u8) -> Option<Rc<dyn Any>> {
    MODULE_OWNERS.with(|owners| owners.borrow().get(&(entry as usize))?.upgrade())
}

#[derive(Clone)]
pub(crate) struct Binding {
    pub name: &'static str,
    pub symbol: &'static str,
    pub signature: FunctionSignature,
    pub entry: *const u8,
}

fn binding_specs() -> &'static [BindingSpec] {
    static SPECS: OnceLock<Vec<BindingSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        all_bindings()
            .into_iter()
            .map(|binding| BindingSpec {
                name: binding.name.into(),
                signature: binding.signature,
            })
            .collect()
    })
}

/// The single source of the `joky/vm` binding surface: resolution injects every
/// row as an extern declaration, and `binding` looks rows up by name.
fn all_bindings() -> Vec<Binding> {
    let handle = Type::Named("vm.Handle".into());
    let function = Type::Named("vm.Function".into());
    vec![
        Binding {
            name: "vm.create",
            symbol: "joky_vm_create",
            signature: FunctionSignature::new(vec![], handle.clone()),
            entry: vm::create as *const u8,
        },
        Binding {
            name: "vm.create_interpreter",
            symbol: "joky_vm_create_interpreter",
            signature: FunctionSignature::new(vec![], handle.clone()),
            entry: vm::create_interpreter as *const u8,
        },
        Binding {
            name: "vm.close",
            symbol: "joky_vm_close",
            signature: FunctionSignature::new(vec![handle.clone()], Type::Int32),
            entry: vm::close as *const u8,
        },
        Binding {
            name: "vm.load_source",
            symbol: "joky_vm_load_source",
            signature: FunctionSignature::new(vec![handle.clone(), Type::String], Type::Int32),
            entry: vm::load_source as *const u8,
        },
        Binding {
            name: "vm.reload_source",
            symbol: "joky_vm_reload_source",
            signature: FunctionSignature::new(vec![handle.clone(), Type::String], Type::Int32),
            entry: vm::reload_source as *const u8,
        },
        Binding {
            name: "vm.register_int32_0",
            symbol: "joky_vm_register_int32_0",
            signature: FunctionSignature::new(
                vec![handle.clone(), Type::String, function.clone()],
                Type::Int32,
            ),
            entry: vm::register_int32_0 as *const u8,
        },
        Binding {
            name: "vm.register_int32_1",
            symbol: "joky_vm_register_int32_1",
            signature: FunctionSignature::new(
                vec![handle.clone(), Type::String, function.clone()],
                Type::Int32,
            ),
            entry: vm::register_int32_1 as *const u8,
        },
        Binding {
            name: "vm.register_int32_2",
            symbol: "joky_vm_register_int32_2",
            signature: FunctionSignature::new(
                vec![handle.clone(), Type::String, function],
                Type::Int32,
            ),
            entry: vm::register_int32_2 as *const u8,
        },
        Binding {
            name: "vm.call_int32_0",
            symbol: "joky_vm_call_int32_0",
            signature: FunctionSignature::new(vec![handle.clone(), Type::String], Type::Int32),
            entry: vm::call_int32_0 as *const u8,
        },
        Binding {
            name: "vm.call_int32_1",
            symbol: "joky_vm_call_int32_1",
            signature: FunctionSignature::new(
                vec![handle.clone(), Type::String, Type::Int32],
                Type::Int32,
            ),
            entry: vm::call_int32_1 as *const u8,
        },
        Binding {
            name: "vm.call_int32_2",
            symbol: "joky_vm_call_int32_2",
            signature: FunctionSignature::new(
                vec![handle.clone(), Type::String, Type::Int32, Type::Int32],
                Type::Int32,
            ),
            entry: vm::call_int32_2 as *const u8,
        },
        Binding {
            name: "vm.error_code",
            symbol: "joky_vm_error_code",
            signature: FunctionSignature::new(vec![], Type::Int32),
            entry: vm::error_code as *const u8,
        },
        Binding {
            name: "vm.print_error",
            symbol: "joky_vm_print_error",
            signature: FunctionSignature::new(vec![], Type::Unit),
            entry: vm::print_error as *const u8,
        },
    ]
}

pub(crate) fn binding(name: &str) -> Option<Binding> {
    BINDINGS.with(|bindings| {
        let mut bindings = bindings.borrow_mut();
        let entries = bindings.get_or_insert_with(all_bindings);
        entries.iter().find(|binding| binding.name == name).cloned()
    })
}

/// Provide the standard library modules consumed by the core resolver.
pub(crate) fn binding_registry() -> &'static BindingRegistry {
    static REGISTRY: OnceLock<BindingRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = BindingRegistry::new();
        registry.register_module("joky/vm", binding_specs().iter().cloned());
        registry
    })
}
