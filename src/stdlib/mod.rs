//! Built-in binding registry and runtime ownership tracking.
mod vm;

use crate::{
    modules::{ModuleRegistry, NativeBinding},
    resolver::BindingSpec,
    runtime::bindings::HostEntry,
    FunctionSignature, Type,
};
use std::{
    any::Any,
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
};

thread_local! {
    static MODULE_OWNERS: RefCell<HashMap<usize, Weak<dyn Any>>> = RefCell::new(HashMap::new());
    static REGISTRY: Rc<ModuleRegistry> = Rc::new(build_registry());
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

struct Binding {
    pub name: &'static str,
    pub symbol: &'static str,
    pub signature: FunctionSignature,
    pub entry: *const u8,
}

/// The single source of the `joky/vm` binding surface: resolution injects every
/// row as an extern declaration and native linking consumes the same metadata.
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

fn build_registry() -> ModuleRegistry {
    let mut registry = ModuleRegistry::default();
    // These static C ABI functions implement the exact signatures listed above.
    unsafe {
        registry.register_module(
            "joky/vm",
            all_bindings().into_iter().map(|binding| NativeBinding {
                spec: BindingSpec::with_symbol(binding.name, binding.signature, binding.symbol),
                entry: Some(HostEntry::Direct(binding.entry)),
                owner: None,
            }),
        )
    }
    .expect("standard library bindings must be valid");
    registry
}

/// Reuse one immutable registry per thread; raw pointers and Rc owners stay local.
pub(crate) fn module_registry() -> Rc<ModuleRegistry> {
    REGISTRY.with(Rc::clone)
}
