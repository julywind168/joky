//! Built-in binding registry and runtime ownership tracking.
mod vm;

use crate::{
    core::resolver::BindingSpec,
    modules::{ModuleRegistry, NativeBinding},
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

pub(crate) fn track_module(module: &Rc<crate::backends::jit::CompiledModule>) {
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

fn binding(
    name: &str,
    symbol: &str,
    params: Vec<Type>,
    ret: Type,
    entry: *const u8,
) -> NativeBinding {
    NativeBinding {
        spec: BindingSpec::with_symbol(name, FunctionSignature::new(params, ret), symbol),
        entry: Some(HostEntry::Direct(entry)),
        owner: None,
    }
}

/// The single source of the `joky/vm` binding surface: resolution injects every
/// row as an extern declaration and native linking consumes the same metadata.
fn vm_bindings() -> Vec<NativeBinding> {
    let handle = Type::Named("vm.Handle".into());
    let function = Type::Named("vm.Function".into());
    vec![
        binding(
            "vm.create",
            "joky_vm_create",
            vec![],
            handle.clone(),
            vm::create as *const u8,
        ),
        binding(
            "vm.create_interpreter",
            "joky_vm_create_interpreter",
            vec![],
            handle.clone(),
            vm::create_interpreter as *const u8,
        ),
        binding(
            "vm.close",
            "joky_vm_close",
            vec![handle.clone()],
            Type::Int32,
            vm::close as *const u8,
        ),
        binding(
            "vm.load_source",
            "joky_vm_load_source",
            vec![handle.clone(), Type::String],
            Type::Int32,
            vm::load_source as *const u8,
        ),
        binding(
            "vm.reload_source",
            "joky_vm_reload_source",
            vec![handle.clone(), Type::String],
            Type::Int32,
            vm::reload_source as *const u8,
        ),
        binding(
            "vm.register_int32_0",
            "joky_vm_register_int32_0",
            vec![handle.clone(), Type::String, function.clone()],
            Type::Int32,
            vm::register_int32_0 as *const u8,
        ),
        binding(
            "vm.register_int32_1",
            "joky_vm_register_int32_1",
            vec![handle.clone(), Type::String, function.clone()],
            Type::Int32,
            vm::register_int32_1 as *const u8,
        ),
        binding(
            "vm.register_int32_2",
            "joky_vm_register_int32_2",
            vec![handle.clone(), Type::String, function],
            Type::Int32,
            vm::register_int32_2 as *const u8,
        ),
        binding(
            "vm.call_int32_0",
            "joky_vm_call_int32_0",
            vec![handle.clone(), Type::String],
            Type::Int32,
            vm::call_int32_0 as *const u8,
        ),
        binding(
            "vm.call_int32_1",
            "joky_vm_call_int32_1",
            vec![handle.clone(), Type::String, Type::Int32],
            Type::Int32,
            vm::call_int32_1 as *const u8,
        ),
        binding(
            "vm.call_int32_2",
            "joky_vm_call_int32_2",
            vec![handle.clone(), Type::String, Type::Int32, Type::Int32],
            Type::Int32,
            vm::call_int32_2 as *const u8,
        ),
        binding(
            "vm.error_code",
            "joky_vm_error_code",
            vec![],
            Type::Int32,
            vm::error_code as *const u8,
        ),
        binding(
            "vm.print_error",
            "joky_vm_print_error",
            vec![],
            Type::Unit,
            vm::print_error as *const u8,
        ),
    ]
}

fn build_registry() -> ModuleRegistry {
    let mut registry = ModuleRegistry::default();
    // These static C ABI functions implement the exact signatures listed above.
    unsafe { registry.register_module("joky/vm", vm_bindings()) }
        .expect("standard library bindings must be valid");
    registry
}

/// Reuse one immutable registry per thread; raw pointers and Rc owners stay local.
pub(crate) fn module_registry() -> Rc<ModuleRegistry> {
    REGISTRY.with(Rc::clone)
}
