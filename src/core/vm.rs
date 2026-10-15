//! Checked signatures for the source VM library's private runtime boundary.
use super::resolver::BindingSpec;
use crate::{FunctionSignature, Type};

pub(crate) const NAMES: &[&str] = &[
    "__vm_create",
    "__vm_close",
    "__vm_error_code",
    "__vm_print_error",
    "__vm_domain_schema",
    "__vm_prepare_migration",
    "__vm_begin_migration",
    "__vm_step_migration",
    "__vm_begin_migration_batch",
    "__vm_stage_migration",
    "__vm_publish_migration",
    "__vm_cancel_migration",
    "__vm_module_count",
    "__vm_print_modules",
    "__vm_publish_domain_code",
    "__vm_load_source",
    "__vm_reload_source",
    "__vm_register_int32_0",
    "__vm_register_int32_1",
    "__vm_register_int32_2",
    "__vm_call_int32_0",
    "__vm_call_int32_1",
    "__vm_call_int32_2",
    "__vm_call_float64_0",
    "__vm_call_float64_1",
    "__vm_call_float64_2",
    "__vm_call_async_int32",
];

pub(crate) fn binding(name: &str) -> Option<BindingSpec> {
    let operation = name.strip_prefix("__vm_")?;
    let owner = Type::Resource("vm.Owner".into());
    let text = || Type::reference(Type::String, false);
    let mut params = vec![Type::reference(owner.clone(), false)];
    let mut ret = Type::Int32;
    let suffix = match operation {
        "create" => {
            params = vec![Type::UInt64];
            ret = owner;
            ""
        }
        "error_code" | "print_error" => {
            params.clear();
            if operation == "print_error" {
                ret = Type::Unit;
            }
            ""
        }
        "close"
        | "publish_migration"
        | "cancel_migration"
        | "module_count"
        | "print_modules"
        | "publish_domain_code" => "",
        "domain_schema" => {
            ret = Type::UInt64;
            ""
        }
        "step_migration" => {
            params.push(Type::UInt64);
            ""
        }
        "prepare_migration" => {
            params.extend([Type::UInt64, text(), text()]);
            "_v1"
        }
        "begin_migration" => {
            params.extend([Type::UInt64, text(), text(), text()]);
            "_v1"
        }
        "begin_migration_batch" => {
            params.push(text());
            "_v1"
        }
        "stage_migration" => {
            params.extend([text(), text()]);
            "_v1"
        }
        "load_source" | "reload_source" => {
            params.push(text());
            "_v2"
        }
        "register_int32_0" | "register_int32_1" | "register_int32_2" => {
            params.extend([text(), Type::Named("vm.Function".into())]);
            "_v2"
        }
        "call_int32_0" | "call_int32_1" | "call_int32_2" | "call_float64_0" | "call_float64_1"
        | "call_float64_2" => {
            if operation.starts_with("call_float64") {
                ret = Type::Float64;
            }
            params.push(text());
            let arity = operation.as_bytes().last().unwrap() - b'0';
            params.extend(std::iter::repeat_n(ret.clone(), arity as usize));
            "_v2"
        }
        "call_async_int32" => {
            params.extend([Type::String, Type::Int32]);
            "_v2"
        }
        _ => return None,
    };
    let symbol = match operation {
        "create" => "vm_create".into(),
        "error_code" | "print_error" => format!("joky_vm_{operation}"),
        _ => format!("__joky_vm_managed_{operation}{suffix}"),
    };
    let mut spec = BindingSpec::with_symbol(name, FunctionSignature::new(params, ret), symbol);
    spec.suspends = operation == "call_async_int32";
    Some(spec)
}
