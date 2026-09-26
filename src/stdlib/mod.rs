//! Built-in module resolution and the single native binding registry.
mod vm;

use crate::{
    parser::ast::{Expr, ExternFnDecl, Item, Module, Param},
    FunctionSignature, Type,
};

pub(crate) struct Binding {
    pub name: &'static str,
    pub symbol: &'static str,
    pub signature: FunctionSignature,
    pub entry: *const u8,
}

const VM_FUNCTIONS: &[&str] = &[
    "vm.create",
    "vm.close",
    "vm.load_source",
    "vm.reload_source",
    "vm.call_int32_0",
    "vm.call_int32_1",
    "vm.call_int32_2",
    "vm.error_code",
    "vm.print_error",
];

pub(crate) fn binding(name: &str) -> Option<Binding> {
    let handle = Type::Named("vm.Handle".into());
    let (name, symbol, params, ret, entry) = match name {
        "vm.create" => (
            "vm.create",
            "joky_vm_create",
            vec![],
            handle,
            vm::create as *const u8,
        ),
        "vm.close" => (
            "vm.close",
            "joky_vm_close",
            vec![handle],
            Type::Int32,
            vm::close as *const u8,
        ),
        "vm.load_source" => (
            "vm.load_source",
            "joky_vm_load_source",
            vec![handle, Type::String],
            Type::Int32,
            vm::load_source as *const u8,
        ),
        "vm.reload_source" => (
            "vm.reload_source",
            "joky_vm_reload_source",
            vec![handle, Type::String],
            Type::Int32,
            vm::reload_source as *const u8,
        ),
        "vm.call_int32_0" => (
            "vm.call_int32_0",
            "joky_vm_call_int32_0",
            vec![handle, Type::String],
            Type::Int32,
            vm::call_int32_0 as *const u8,
        ),
        "vm.call_int32_1" => (
            "vm.call_int32_1",
            "joky_vm_call_int32_1",
            vec![handle, Type::String, Type::Int32],
            Type::Int32,
            vm::call_int32_1 as *const u8,
        ),
        "vm.call_int32_2" => (
            "vm.call_int32_2",
            "joky_vm_call_int32_2",
            vec![handle, Type::String, Type::Int32, Type::Int32],
            Type::Int32,
            vm::call_int32_2 as *const u8,
        ),
        "vm.error_code" => (
            "vm.error_code",
            "joky_vm_error_code",
            vec![],
            Type::Int32,
            vm::error_code as *const u8,
        ),
        "vm.print_error" => (
            "vm.print_error",
            "joky_vm_print_error",
            vec![],
            Type::Unit,
            vm::print_error as *const u8,
        ),
        _ => return None,
    };
    Some(Binding {
        name,
        symbol,
        signature: FunctionSignature::new(params, ret),
        entry,
    })
}

/// Resolve only built-in imports; no filesystem or package search is performed.
pub(crate) fn resolve(module: &Module) -> Result<Module, String> {
    let mut resolved = module.clone();
    let mut imported = false;
    for path in &module.imports {
        if path != "joky/vm" {
            return Err(format!("Unknown built-in module `{path}`"));
        }
        if imported {
            return Err("Duplicate import `joky/vm`".into());
        }
        imported = true;
    }
    if imported {
        for item in &mut resolved.items {
            if let Item::FnDef(function) = item {
                resolve_calls(&mut function.body)?;
            }
        }
        for name in VM_FUNCTIONS {
            let binding = binding(name).unwrap();
            resolved.items.push(Item::ExternFn(ExternFnDecl {
                name: binding.name.into(),
                params: binding
                    .signature
                    .params
                    .into_iter()
                    .enumerate()
                    .map(|(index, ty)| Param {
                        name: format!("arg{index}"),
                        ty,
                    })
                    .collect(),
                ret_ty: binding.signature.ret,
                suspends: false,
            }));
        }
    }
    resolved.imports.clear();
    Ok(resolved)
}

fn resolve_calls(expr: &mut Expr) -> Result<(), String> {
    match expr {
        Expr::Call { callee, args } => {
            if callee == "vm.call_int32" {
                if !(2..=4).contains(&args.len()) {
                    return Err("vm.call_int32 expects a handle, function name and zero to two Int32 arguments".into());
                }
                *callee = format!("vm.call_int32_{}", args.len() - 2);
            }
            for arg in args {
                resolve_calls(arg)?;
            }
        }
        Expr::Let { value, .. } => resolve_calls(value)?,
        Expr::Block(exprs) | Expr::Scope(exprs) => {
            for expr in exprs {
                resolve_calls(expr)?;
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            resolve_calls(lhs)?;
            resolve_calls(rhs)?;
        }
        _ => {}
    }
    Ok(())
}
