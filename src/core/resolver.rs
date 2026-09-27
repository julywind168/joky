//! Built-in import and call resolution over the parsed AST.
//!
//! The resolver owns AST rewriting. Providers such as `stdlib` only supply
//! binding names and signatures; they do not need to know the AST shape.
use crate::{
    parser::ast::{Expr, ExternFnDecl, Item, Module, Param},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type,
};
use std::collections::{HashMap, HashSet};

/// Name and signature information needed to inject an external binding.
#[derive(Clone, Debug)]
pub(crate) struct BindingSpec {
    pub(crate) name: String,
    pub(crate) signature: FunctionSignature,
}

/// Supplies binding declarations for imported modules.
pub(crate) trait ModuleProvider {
    fn bindings(&self, path: &str) -> Option<&[BindingSpec]>;
}

/// In-memory module-to-binding registry used by the resolver.
#[derive(Clone, Debug, Default)]
pub(crate) struct BindingRegistry {
    modules: HashMap<String, Vec<BindingSpec>>,
}

impl BindingRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register_module(
        &mut self,
        path: impl Into<String>,
        bindings: impl IntoIterator<Item = BindingSpec>,
    ) {
        self.modules
            .insert(path.into(), bindings.into_iter().collect());
    }

    pub(crate) fn module(&self, path: &str) -> Option<&[BindingSpec]> {
        self.modules.get(path).map(Vec::as_slice)
    }
}

impl ModuleProvider for BindingRegistry {
    fn bindings(&self, path: &str) -> Option<&[BindingSpec]> {
        self.module(path)
    }
}

/// Resolve imported modules and rewrite their built-in call forms.
pub(crate) fn resolve<P: ModuleProvider>(module: &Module, provider: &P) -> Result<Module, Error> {
    let mut resolved = module.clone();
    let mut imported = false;
    let mut imported_paths = HashSet::new();
    let mut imported_bindings = Vec::new();
    for import in &module.imports {
        let Some(bindings) = provider.bindings(&import.path) else {
            return Err(resolve_error_code(
                Some(&import.span),
                ErrorCode::InvalidImport,
                format!("Unknown built-in module `{}`", import.path),
            ));
        };
        if !imported_paths.insert(import.path.as_str()) {
            return Err(resolve_error_code(
                Some(&import.span),
                ErrorCode::InvalidImport,
                format!("Duplicate import `{}`", import.path),
            ));
        }
        imported = true;
        imported_bindings.extend(bindings.iter().map(|binding| (binding, import.span)));
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
        let mut names = HashSet::new();
        for (binding, import_span) in imported_bindings {
            if !names.insert(binding.name.as_str()) {
                return Err(resolve_error_code(
                    Some(&import_span),
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate imported binding `{}`", binding.name),
                ));
            }
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
                        span: Span::empty(0),
                    })
                    .collect(),
                ret_ty: binding.signature.ret.clone(),
                suspends: false,
                span: Span::empty(0),
            }));
        }
    }
    resolved.imports.clear();
    Ok(resolved)
}

fn resolve_calls(
    expr: &mut Expr,
    functions: &HashMap<String, (FunctionSignature, bool)>,
) -> Result<(), Error> {
    match expr {
        Expr::Call { callee, args, span } => {
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
                let Expr::Ident {
                    name,
                    span: function_span,
                } = &args[2]
                else {
                    return Err(resolve_error(
                        Some(span),
                        "vm.register_int32 expects a function name as its third argument",
                    ));
                };
                let Some((signature, suspends)) = functions.get(name) else {
                    return Err(resolve_error(
                        Some(function_span),
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
                        Some(function_span),
                        format!("vm.register_int32 function `{name}` must be a synchronous Int32 function with zero to two Int32 arguments"),
                    ));
                }
                *callee = format!("vm.register_int32_{}", signature.params.len());
                args[2] = Expr::FunctionRef {
                    name: name.clone(),
                    span: *function_span,
                };
            }
            for arg in args {
                resolve_calls(arg, functions)?;
            }
        }
        Expr::Let { value, .. } => resolve_calls(value, functions)?,
        Expr::Block { exprs, .. }
        | Expr::Scope {
            branches: exprs, ..
        } => {
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

fn resolve_error(span: Option<&Span>, message: impl Into<String>) -> Error {
    let error = Error::new_with_code(ErrorPhase::Resolve, ErrorCode::InvalidCall, message);
    span.map_or(error.clone(), |span| error.with_span(*span))
}

fn resolve_error_code(span: Option<&Span>, code: ErrorCode, message: impl Into<String>) -> Error {
    let error = Error::new_with_code(ErrorPhase::Resolve, code, message);
    span.map_or(error.clone(), |span| error.with_span(*span))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_builtin_calls_without_stdlib_ast_logic() {
        let module =
            crate::parser::parse("import joky/vm fn main() { vm.call_int32(1, \"value\", 42) }")
                .unwrap();
        let mut registry = BindingRegistry::new();
        registry.register_module(
            "joky/vm",
            [BindingSpec {
                name: "vm.call_int32_1".into(),
                signature: FunctionSignature::new(
                    vec![Type::Named("vm.Handle".into()), Type::String, Type::Int32],
                    Type::Int32,
                ),
            }],
        );
        let resolved = resolve(&module, &registry).unwrap();
        assert!(resolved.imports.is_empty());
        let Item::FnDef(function) = &resolved.items[0] else {
            panic!("expected function definition");
        };
        let Expr::Block {
            exprs: expressions, ..
        } = &function.body
        else {
            panic!("expected function block");
        };
        let Expr::Call { callee, .. } = &expressions[0] else {
            panic!("expected call expression");
        };
        assert_eq!(callee, "vm.call_int32_1");
        assert_eq!(resolved.items.len(), 2);
    }

    #[test]
    fn reports_builtin_call_errors_at_the_call_expression() {
        let source = "import joky/vm fn main() { vm.call_int32(1) }";
        let module = crate::parser::parse(source).unwrap();
        let mut registry = BindingRegistry::new();
        registry.register_module("joky/vm", []);
        let error = resolve(&module, &registry).unwrap_err();
        let start = source.find("vm.call_int32").unwrap();
        let end = source[start..].find(')').unwrap() + start + 1;
        assert_eq!(error.phase(), ErrorPhase::Resolve);
        assert_eq!(error.code(), ErrorCode::InvalidCall);
        assert_eq!(error.span(), Some(Span::new(start, end)));
        assert_eq!(
            error.span().unwrap().slice(source),
            Some("vm.call_int32(1)")
        );
    }

    #[test]
    fn resolves_bindings_from_custom_module_provider() {
        let module = crate::parser::parse("import user/math fn main() { user.add(1, 2) }").unwrap();
        let mut registry = BindingRegistry::new();
        registry.register_module(
            "user/math",
            [BindingSpec {
                name: "user.add".into(),
                signature: FunctionSignature::new(vec![Type::Int32, Type::Int32], Type::Int32),
            }],
        );

        let resolved = resolve(&module, &registry).unwrap();
        assert!(resolved.imports.is_empty());
        assert!(resolved.items.iter().any(|item| {
            matches!(item, Item::ExternFn(function) if function.name == "user.add")
        }));
    }
}
