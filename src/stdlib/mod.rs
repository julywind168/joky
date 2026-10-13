//! Built-in binding registry and runtime ownership tracking.
pub(crate) mod domain;
mod output;
pub(crate) mod task;
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
    let mut async_spec = BindingSpec::with_symbol(
        "vm.call_async_int32",
        FunctionSignature::new(vec![handle.clone(), Type::String, Type::Int32], Type::Int32),
        "joky_vm_call_async_int32_v2",
    );
    async_spec.suspends = true;
    vec![
        NativeBinding {
            spec: async_spec,
            entry: Some(HostEntry::Cps {
                entry: vm::call_async_int32,
                frame_slots: Some(3),
            }),
            owner: None,
        },
        binding(
            "vm.domain_schema",
            "joky_vm_domain_schema",
            vec![handle.clone()],
            Type::UInt64,
            vm::domain_schema as *const u8,
        ),
        binding(
            "vm.prepare_migration",
            "joky_vm_prepare_migration_v1",
            vec![
                handle.clone(),
                Type::UInt64,
                Type::reference(Type::String, false),
                Type::reference(Type::String, false),
            ],
            Type::Int32,
            vm::prepare_migration as *const u8,
        ),
        binding(
            "vm.begin_migration",
            "joky_vm_begin_migration_v1",
            vec![
                handle.clone(),
                Type::UInt64,
                Type::reference(Type::String, false),
                Type::reference(Type::String, false),
                Type::reference(Type::String, false),
            ],
            Type::Int32,
            vm::begin_migration as *const u8,
        ),
        binding(
            "vm.step_migration",
            "joky_vm_step_migration",
            vec![handle.clone(), Type::UInt64],
            Type::Int32,
            vm::step_migration as *const u8,
        ),
        binding(
            "vm.begin_migration_batch",
            "joky_vm_begin_migration_batch_v1",
            vec![handle.clone(), Type::reference(Type::String, false)],
            Type::Int32,
            vm::begin_migration_batch as *const u8,
        ),
        binding(
            "vm.stage_migration",
            "joky_vm_stage_migration_v1",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                Type::reference(Type::String, false),
            ],
            Type::Int32,
            vm::stage_migration as *const u8,
        ),
        binding(
            "vm.publish_migration",
            "joky_vm_publish_migration",
            vec![handle.clone()],
            Type::Int32,
            vm::publish_migration as *const u8,
        ),
        binding(
            "vm.cancel_migration",
            "joky_vm_cancel_migration",
            vec![handle.clone()],
            Type::Int32,
            vm::cancel_migration as *const u8,
        ),
        binding(
            "vm.publish_domain_code",
            "joky_vm_publish_domain_code",
            vec![handle.clone()],
            Type::Int32,
            vm::publish_domain_code as *const u8,
        ),
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
            "joky_vm_load_source_v2",
            vec![handle.clone(), Type::reference(Type::String, false)],
            Type::Int32,
            vm::load_source as *const u8,
        ),
        binding(
            "vm.reload_source",
            "joky_vm_reload_source_v2",
            vec![handle.clone(), Type::reference(Type::String, false)],
            Type::Int32,
            vm::reload_source as *const u8,
        ),
        binding(
            "vm.register_int32_0",
            "joky_vm_register_int32_0_v2",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                function.clone(),
            ],
            Type::Int32,
            vm::register_int32_0 as *const u8,
        ),
        binding(
            "vm.register_int32_1",
            "joky_vm_register_int32_1_v2",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                function.clone(),
            ],
            Type::Int32,
            vm::register_int32_1 as *const u8,
        ),
        binding(
            "vm.register_int32_2",
            "joky_vm_register_int32_2_v2",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                function,
            ],
            Type::Int32,
            vm::register_int32_2 as *const u8,
        ),
        binding(
            "vm.call_int32_0",
            "joky_vm_call_int32_0_v2",
            vec![handle.clone(), Type::reference(Type::String, false)],
            Type::Int32,
            vm::call_int32_0 as *const u8,
        ),
        binding(
            "vm.call_int32_1",
            "joky_vm_call_int32_1_v2",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                Type::Int32,
            ],
            Type::Int32,
            vm::call_int32_1 as *const u8,
        ),
        binding(
            "vm.call_int32_2",
            "joky_vm_call_int32_2_v2",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                Type::Int32,
                Type::Int32,
            ],
            Type::Int32,
            vm::call_int32_2 as *const u8,
        ),
        binding(
            "vm.call_float64_0",
            "joky_vm_call_float64_0_v2",
            vec![handle.clone(), Type::reference(Type::String, false)],
            Type::Float64,
            vm::call_float64_0 as *const u8,
        ),
        binding(
            "vm.call_float64_1",
            "joky_vm_call_float64_1_v2",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                Type::Float64,
            ],
            Type::Float64,
            vm::call_float64_1 as *const u8,
        ),
        binding(
            "vm.call_float64_2",
            "joky_vm_call_float64_2_v2",
            vec![
                handle.clone(),
                Type::reference(Type::String, false),
                Type::Float64,
                Type::Float64,
            ],
            Type::Float64,
            vm::call_float64_2 as *const u8,
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
    unsafe {
        registry.register_module(
            "joky/internal/output",
            [binding(
                "__joky_write_stdout",
                "joky_io_write_stdout_v2",
                vec![Type::reference(Type::String, false)],
                Type::Int32,
                output::joky_io_write_stdout_v2 as *const u8,
            )],
        )
    }
    .expect("Stdout binding must be valid");
    // These static C ABI functions implement the exact signatures listed above.
    unsafe { registry.register_module("joky/vm", vm_bindings()) }
        .expect("standard library bindings must be valid");
    let tasks = [
        (
            "yield_now",
            vec![],
            task::yield_now as crate::runtime::cps::CpsEntry,
        ),
        (
            "sleep_ms",
            vec![Type::UInt64],
            task::sleep_ms as crate::runtime::cps::CpsEntry,
        ),
    ]
    .into_iter()
    .map(|(name, params, entry)| {
        let frame_slots = params.len();
        let mut spec = BindingSpec::with_symbol(
            format!("joky/task.{name}"),
            FunctionSignature::new(params, Type::Unit),
            format!("joky_task_{name}"),
        );
        spec.suspends = true;
        NativeBinding {
            spec,
            entry: Some(HostEntry::Cps {
                entry,
                frame_slots: Some(frame_slots),
            }),
            owner: None,
        }
    });
    // Static CPS callbacks use the declared frame slots and retain no pointers.
    unsafe { registry.register_module("joky/task", tasks) }.expect("Task bindings must be valid");
    let placeholder = NativeBinding {
        spec: BindingSpec::with_symbol(
            "__domain_module",
            FunctionSignature::new(vec![], crate::runtime::domain::module_type()),
            "__joky_domain_module_placeholder",
        ),
        entry: Some(HostEntry::Direct(
            crate::runtime::domain::module_placeholder as *const u8,
        )),
        owner: None,
    };
    unsafe { registry.register_module("joky/internal/domain_module", [placeholder]) }
        .expect("Domain module binding must be valid");
    let domain = [
        (
            "read",
            "__domain_read_i32",
            vec![Type::String],
            domain::read as crate::runtime::cps::CpsEntry,
        ),
        (
            "write",
            "__domain_write_i32",
            vec![Type::String, Type::Int32],
            domain::write as crate::runtime::cps::CpsEntry,
        ),
    ]
    .into_iter()
    .map(|(name, local, params, entry)| {
        let slots = params.len();
        let mut spec = BindingSpec::with_symbol(
            local,
            FunctionSignature::new(params, Type::Int32),
            format!("__joky_domain_{name}_i32"),
        );
        spec.suspends = true;
        NativeBinding {
            spec,
            entry: Some(HostEntry::Cps {
                entry,
                frame_slots: Some(slots),
            }),
            owner: None,
        }
    });
    unsafe { registry.register_module("joky/internal/domain", domain) }
        .expect("Domain bindings must be valid");
    // Embedded source modules declare private host imports explicitly.
    let mut spec = BindingSpec::with_symbol(
        "__joky_task_yield_now",
        FunctionSignature::new(vec![], Type::Unit),
        "__joky_task_yield_now",
    );
    spec.suspends = true;
    unsafe {
        registry.register_module(
            "joky/internal/task",
            [NativeBinding {
                spec,
                entry: Some(HostEntry::Cps {
                    entry: task::yield_internal,
                    frame_slots: Some(0),
                }),
                owner: None,
            }],
        )
    }
    .expect("Private task binding must be valid");
    registry
}

/// Reuse one immutable registry per thread; raw pointers and Rc owners stay local.
pub(crate) fn module_registry() -> Rc<ModuleRegistry> {
    REGISTRY.with(Rc::clone)
}

pub(crate) use vm::script_poll;
