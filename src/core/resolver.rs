//! Built-in import and call resolution over the parsed AST.
//!
//! The resolver owns AST rewriting. Providers such as `stdlib` only supply
//! binding names and signatures; they do not need to know the AST shape.
use crate::{
    parser::ast::{Expr, ExternFnDecl, Item, Module, Param},
    Error, ErrorPhase, FunctionSignature, Type,
};
use std::collections::HashMap;

/// Name and signature information needed to inject an external binding.
#[derive(Clone, Debug)]
pub(crate) struct BindingSpec {
    pub(crate) name: String,
    pub(crate) signature: FunctionSignature,
}

/// Resolve the built-in imports used by a module.
pub(crate) fn resolve(module: &Module, bindings: &[BindingSpec]) -> Result<Module, Error> {
    let mut resolved = module.clone();
    let mut imported = false;
    for path in &module.imports {
        if path != "joky/vm" {
            return Err(resolve_error(
                None,
                format!("Unknown built-in module `{path}`"),
            ));
        }
        if imported {
            return Err(resolve_error(None, "Duplicate import `joky/vm`"));
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
                let span = function.span.clone();
                resolve_calls(&mut function.body, &functions, &span)?;
            }
        }
        for binding in bindings {
            resolved.items.push(Item::ExternFn(ExternFnDecl {
                name: binding.name.clone(),
                params: binding
                    .signature
                    .params
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, ty)| Param {
                        name: format!("arg{index}"),
                        ty,
                    })
                    .collect(),
                ret_ty: binding.signature.ret.clone(),
                suspends: false,
                span: 0..0,
            }));
        }
    }
    resolved.imports.clear();
    Ok(resolved)
}

fn resolve_calls(
    expr: &mut Expr,
    functions: &HashMap<String, (FunctionSignature, bool)>,
    span: &std::ops::Range<usize>,
) -> Result<(), Error> {
    match expr {
        Expr::Call { callee, args } => {
            if callee == "vm.call_int32" {
                if !(2..=4).contains(&args.len()) {
                    return Err(resolve_error(
                        Some(span),
                        "vm.call_int32 expects a handle, function name and zero to two Int32 arguments",
                    ));
                }
                *callee = format!("vm.call_int32_{}", args.len() - 2);
            }
            if callee == "vm.register_int32" {
                if args.len() != 3 {
                    return Err(resolve_error(
                        Some(span),
                        "vm.register_int32 expects a handle, function name and function reference",
                    ));
                }
                let Expr::Ident(name) = &args[2] else {
                    return Err(resolve_error(
                        Some(span),
                        "vm.register_int32 expects a function name as its third argument",
                    ));
                };
                let Some((signature, suspends)) = functions.get(name) else {
                    return Err(resolve_error(
                        Some(span),
                        format!(
                            "vm.register_int32 function `{name}` is not defined in this module"
                        ),
                    ));
                };
                if *suspends
                    || signature.ret != Type::Int32
                    || signature.params.len() > 2
                    || signature.params.iter().any(|ty| *ty != Type::Int32)
                {
                    return Err(resolve_error(
                        Some(span),
                        format!("vm.register_int32 function `{name}` must be a synchronous Int32 function with zero to two Int32 arguments"),
                    ));
                }
                *callee = format!("vm.register_int32_{}", signature.params.len());
                args[2] = Expr::FunctionRef(name.clone());
            }
            for arg in args {
                resolve_calls(arg, functions, span)?;
            }
        }
        Expr::Let { value, .. } => resolve_calls(value, functions, span)?,
        Expr::Block(exprs) | Expr::Scope(exprs) => {
            for expr in exprs {
                resolve_calls(expr, functions, span)?;
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            resolve_calls(lhs, functions, span)?;
            resolve_calls(rhs, functions, span)?;
        }
        _ => {}
    }
    Ok(())
}

fn resolve_error(span: Option<&std::ops::Range<usize>>, message: impl Into<String>) -> Error {
    let error = Error::new(ErrorPhase::Resolve, message);
    span.map_or(error.clone(), |span| error.with_span(span.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_builtin_calls_without_stdlib_ast_logic() {
        let module =
            crate::parser::parse("import joky/vm fn main() { vm.call_int32(1, \"value\", 42) }")
                .unwrap();
        let bindings = vec![BindingSpec {
            name: "vm.call_int32_1".into(),
            signature: FunctionSignature::new(
                vec![Type::Named("vm.Handle".into()), Type::String, Type::Int32],
                Type::Int32,
            ),
        }];
        let resolved = resolve(&module, &bindings).unwrap();
        assert!(resolved.imports.is_empty());
        let Item::FnDef(function) = &resolved.items[0] else {
            panic!("expected function definition");
        };
        let Expr::Block(expressions) = &function.body else {
            panic!("expected function block");
        };
        let Expr::Call { callee, .. } = &expressions[0] else {
            panic!("expected call expression");
        };
        assert_eq!(callee, "vm.call_int32_1");
        assert_eq!(resolved.items.len(), 2);
    }
}
