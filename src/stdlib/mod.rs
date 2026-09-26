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
    all_bindings()
        .into_iter()
        .find(|binding| binding.name == name)
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
        for binding in all_bindings() {
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
