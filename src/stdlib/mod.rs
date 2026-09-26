//! Built-in module resolution and the single native binding registry.
mod vm;

use crate::{
    parser::ast::{Expr, ExternFnDecl, Item, Module, Param},
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

pub(crate) struct Binding {
    pub name: &'static str,
    pub symbol: &'static str,
    pub signature: FunctionSignature,
    pub entry: *const u8,
}

const VM_FUNCTIONS: &[&str] = &[
    "vm.create",
    "vm.create_interpreter",
    "vm.close",
    "vm.load_source",
    "vm.reload_source",
    "vm.register_int32_0",
    "vm.register_int32_1",
    "vm.register_int32_2",
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
        "vm.create_interpreter" => (
            "vm.create_interpreter",
            "joky_vm_create_interpreter",
            vec![],
            handle,
            vm::create_interpreter as *const u8,
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
        "vm.register_int32_0" | "vm.register_int32_1" | "vm.register_int32_2" => {
            let arity = name.rsplit('_').next().unwrap().parse::<usize>().unwrap();
            let registered_name = match arity {
                0 => "vm.register_int32_0",
                1 => "vm.register_int32_1",
                _ => "vm.register_int32_2",
            };
            (
                registered_name,
                match arity {
                    0 => "joky_vm_register_int32_0",
                    1 => "joky_vm_register_int32_1",
                    _ => "joky_vm_register_int32_2",
                },
                vec![handle, Type::String, Type::Named("vm.Function".into())],
                Type::Int32,
                match arity {
                    0 => vm::register_int32_0 as *const u8,
                    1 => vm::register_int32_1 as *const u8,
                    _ => vm::register_int32_2 as *const u8,
                },
            )
        }
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
        let functions: HashMap<_, _> = resolved
            .items
            .iter()
            .filter_map(|item| match item {
                Item::FnDef(function) => Some((
                    function.name.clone(),
                    (
                        FunctionSignature::new(
                            function
                                .params
                                .iter()
                                .map(|param| param.ty.clone())
                                .collect(),
                            function.ret_ty.clone(),
                        ),
                        function.suspends,
                    ),
                )),
                Item::ExternFn(_) => None,
            })
            .collect();
        for item in &mut resolved.items {
            if let Item::FnDef(function) = item {
                resolve_calls(&mut function.body, &functions)?;
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

fn resolve_calls(
    expr: &mut Expr,
    functions: &HashMap<String, (FunctionSignature, bool)>,
) -> Result<(), String> {
    match expr {
        Expr::Call { callee, args } => {
            if callee == "vm.call_int32" {
                if !(2..=4).contains(&args.len()) {
                    return Err("vm.call_int32 expects a handle, function name and zero to two Int32 arguments".into());
                }
                *callee = format!("vm.call_int32_{}", args.len() - 2);
            }
            if callee == "vm.register_int32" {
                if args.len() != 3 {
                    return Err(
                        "vm.register_int32 expects a handle, function name and function reference"
                            .into(),
                    );
                }
                let Expr::Ident(name) = &args[2] else {
                    return Err(
                        "vm.register_int32 expects a function name as its third argument".into(),
                    );
                };
                let Some((signature, suspends)) = functions.get(name) else {
                    return Err(format!(
                        "vm.register_int32 function `{name}` is not defined in this module"
                    ));
                };
                if *suspends
                    || signature.ret != Type::Int32
                    || signature.params.len() > 2
                    || signature.params.iter().any(|ty| *ty != Type::Int32)
                {
                    return Err(format!("vm.register_int32 function `{name}` must be a synchronous Int32 function with zero to two Int32 arguments"));
                }
                *callee = format!("vm.register_int32_{}", signature.params.len());
                args[2] = Expr::FunctionRef(name.clone());
            }
            for arg in args {
                resolve_calls(arg, functions)?;
            }
        }
        Expr::Let { value, .. } => resolve_calls(value, functions)?,
        Expr::Block(exprs) | Expr::Scope(exprs) => {
            for expr in exprs {
                resolve_calls(expr, functions)?;
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            resolve_calls(lhs, functions)?;
            resolve_calls(rhs, functions)?;
        }
        _ => {}
    }
    Ok(())
}
