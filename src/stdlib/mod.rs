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

/// Implementations of the signatures shared with standalone source validation.
fn vm_bindings() -> Vec<NativeBinding> {
    crate::core::vm::NAMES
        .iter()
        .map(|&name| {
            let entry = match name {
                "__vm_create" => HostEntry::ResourceFactory(Rc::new(vm::managed::FACTORY)),
                "__vm_close" => HostEntry::Direct(vm::managed::close as *const u8),
                "__vm_error_code" => HostEntry::Direct(vm::managed::error_code as *const u8),
                "__vm_print_error" => HostEntry::Direct(vm::managed::print_error as *const u8),
                "__vm_domain_schema" => HostEntry::Direct(vm::managed::domain_schema as *const u8),
                "__vm_prepare_migration" => {
                    HostEntry::Direct(vm::managed::prepare_migration as *const u8)
                }
                "__vm_begin_migration" => {
                    HostEntry::Direct(vm::managed::begin_migration as *const u8)
                }
                "__vm_step_migration" => {
                    HostEntry::Direct(vm::managed::step_migration as *const u8)
                }
                "__vm_begin_migration_batch" => {
                    HostEntry::Direct(vm::managed::begin_migration_batch as *const u8)
                }
                "__vm_stage_migration" => {
                    HostEntry::Direct(vm::managed::stage_migration as *const u8)
                }
                "__vm_publish_migration" => {
                    HostEntry::Direct(vm::managed::publish_migration as *const u8)
                }
                "__vm_cancel_migration" => {
                    HostEntry::Direct(vm::managed::cancel_migration as *const u8)
                }
                "__vm_module_count" => HostEntry::Direct(vm::managed::module_count as *const u8),
                "__vm_print_modules" => HostEntry::Direct(vm::managed::print_modules as *const u8),
                "__vm_publish_domain_code" => {
                    HostEntry::Direct(vm::managed::publish_domain_code as *const u8)
                }
                "__vm_load_source" => HostEntry::Direct(vm::managed::load_source as *const u8),
                "__vm_reload_source" => HostEntry::Direct(vm::managed::reload_source as *const u8),
                "__vm_register_int32_0" => {
                    HostEntry::Direct(vm::managed::register_int32_0 as *const u8)
                }
                "__vm_register_int32_1" => {
                    HostEntry::Direct(vm::managed::register_int32_1 as *const u8)
                }
                "__vm_register_int32_2" => {
                    HostEntry::Direct(vm::managed::register_int32_2 as *const u8)
                }
                "__vm_call_int32_0" => HostEntry::Direct(vm::managed::call_int32_0 as *const u8),
                "__vm_call_int32_1" => HostEntry::Direct(vm::managed::call_int32_1 as *const u8),
                "__vm_call_int32_2" => HostEntry::Direct(vm::managed::call_int32_2 as *const u8),
                "__vm_call_float64_0" => {
                    HostEntry::Direct(vm::managed::call_float64_0 as *const u8)
                }
                "__vm_call_float64_1" => {
                    HostEntry::Direct(vm::managed::call_float64_1 as *const u8)
                }
                "__vm_call_float64_2" => {
                    HostEntry::Direct(vm::managed::call_float64_2 as *const u8)
                }
                _ => unreachable!("checked VM binding"),
            };
            NativeBinding {
                spec: crate::core::vm::binding(name).unwrap(),
                entry: Some(entry),
                owner: None,
            }
        })
        .collect()
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
    unsafe { registry.register_module("joky/internal/vm", vm_bindings()) }
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

pub(crate) use vm::{call_poll, call_poll_native};
