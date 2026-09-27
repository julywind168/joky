//! Named type registration shared by every compilation backend.
//!
//! Primitive types are part of the language. Source aliases are module-local;
//! opaque host types must be registered before compilation.

use crate::{
    parser::ast::{Expr, Item, Module, Type},
    Error, ErrorCode, ErrorPhase,
};
use std::collections::{BTreeSet, HashMap};

const VM_HANDLE: &str = "vm.Handle";
const VM_FUNCTION: &str = "vm.Function";

#[derive(Clone, Debug)]
pub struct TypeRegistry {
    named: BTreeSet<String>,
}

impl TypeRegistry {
    /// Create a registry containing the built-in VM opaque types.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_opaque(&mut self, name: impl Into<String>) -> Result<(), Error> {
        let name = name.into();
        if !is_type_name(&name) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                format!("Invalid opaque type name `{name}`"),
            ));
        }
        if !self.named.insert(name.clone()) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                format!("Opaque type `{name}` is already registered"),
            ));
        }
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.named.contains(name)
    }

    /// Resolve transparent aliases in signatures and local annotations.
    ///
    /// Returns a copy with no alias declarations or alias type references.
    /// Non-alias names remain subject to ordinary opaque-type validation.
    /// The input and host registry are unchanged. Run before entry-point,
    /// built-in call and ABI checks; VM and AOT loading do this automatically.
    pub fn resolve_aliases(&self, module: &Module) -> Result<Module, Error> {
        if module.type_aliases.is_empty() {
            return Ok(module.clone());
        }
        let mut declarations = HashMap::new();
        for alias in &module.type_aliases {
            if self.contains(&alias.name)
                || matches!(alias.name.as_str(), "Int32" | "Bool" | "Unit" | "String")
                || declarations.insert(alias.name.as_str(), alias).is_some()
            {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::DuplicateDefinition,
                    format!("Type `{}` is already defined", alias.name),
                )
                .with_span(alias.name_span));
            }
        }
        let mut canonical: HashMap<&str, Type> = HashMap::new();
        // Every alias has one target. Resolve chains iteratively and memoize
        // their terminal type, so long forward chains do not grow the Rust stack.
        for alias in &module.type_aliases {
            let mut path = Vec::new();
            let mut visiting = HashMap::new();
            let mut current = alias;
            let target = loop {
                if let Some(ty) = canonical.get(current.name.as_str()) {
                    break ty.clone();
                }
                visiting.insert(current.name.as_str(), path.len());
                path.push(current.name.as_str());
                if let Type::Named(name) = &current.target {
                    if let Some(&index) = visiting.get(name.as_str()) {
                        let mut cycle = path[index..].to_vec();
                        cycle.push(name);
                        return Err(Error::new_with_code(
                            ErrorPhase::TypeCheck,
                            ErrorCode::CyclicTypeAlias,
                            format!("Cyclic type alias: {}", cycle.join(" -> ")),
                        )
                        .with_span(current.target_span));
                    }
                    if let Some(next) = declarations.get(name.as_str()) {
                        current = next;
                        continue;
                    }
                }
                self.validate(&current.target)
                    .map_err(|error| error.with_span(current.target_span))?;
                break current.target.clone();
            };
            for name in path {
                canonical.insert(name, target.clone());
            }
        }
        let mut resolved = module.clone();
        resolved.type_aliases.clear();
        for item in &mut resolved.items {
            let (params, ret) = match item {
                Item::FnDef(function) => {
                    resolve_annotations(&mut function.body, &canonical);
                    (&mut function.params, &mut function.ret_ty)
                }
                Item::ExternFn(function) => (&mut function.params, &mut function.ret_ty),
            };
            for param in params {
                resolve_type(&mut param.ty, &canonical);
            }
            resolve_type(ret, &canonical);
        }
        Ok(resolved)
    }

    pub(crate) fn validate(&self, ty: &Type) -> Result<(), Error> {
        let Type::Named(name) = ty else { return Ok(()) };
        if self.contains(name) {
            Ok(())
        } else {
            Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::UnknownType,
                format!("Unknown type `{name}`; declare an alias or register an opaque host type"),
            ))
        }
    }
}

fn resolve_type(ty: &mut Type, aliases: &HashMap<&str, Type>) {
    if let Type::Named(name) = ty {
        if let Some(target) = aliases.get(name.as_str()) {
            *ty = target.clone();
        }
    }
}

fn resolve_annotations(expr: &mut Expr, aliases: &HashMap<&str, Type>) {
    match expr {
        Expr::Let {
            annotation, value, ..
        }
        | Expr::Var {
            annotation, value, ..
        } => {
            if let Some(ty) = annotation {
                resolve_type(ty, aliases);
            }
            resolve_annotations(value, aliases);
        }
        Expr::Assign { value, .. }
        | Expr::Unary { operand: value, .. }
        | Expr::Return {
            value: Some(value), ..
        } => resolve_annotations(value, aliases),
        Expr::While {
            condition, body, ..
        } => {
            resolve_annotations(condition, aliases);
            resolve_annotations(body, aliases);
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            resolve_annotations(condition, aliases);
            resolve_annotations(then_branch, aliases);
            if let Some(branch) = else_branch {
                resolve_annotations(branch, aliases);
            }
        }
        Expr::BinOp { lhs, rhs, .. } | Expr::Logical { lhs, rhs, .. } => {
            resolve_annotations(lhs, aliases);
            resolve_annotations(rhs, aliases);
        }
        Expr::Call { args: exprs, .. }
        | Expr::Block { exprs, .. }
        | Expr::Sequence { exprs, .. }
        | Expr::Scope {
            branches: exprs, ..
        } => {
            for expr in exprs {
                resolve_annotations(expr, aliases);
            }
        }
        Expr::Literal { .. }
        | Expr::Ident { .. }
        | Expr::FunctionRef { .. }
        | Expr::Return { value: None, .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
    }
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self {
            named: BTreeSet::from([VM_HANDLE.into(), VM_FUNCTION.into()]),
        }
    }
}

fn is_type_name(name: &str) -> bool {
    let mut segments = name.split('.');
    let valid = |segment: &str| {
        let mut chars = segment.chars();
        matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
            && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
    };
    !name.is_empty() && segments.all(valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_normalize_types_without_changing_the_source_or_registry() {
        let mut types = TypeRegistry::new();
        types.register_opaque("Host.Context").unwrap();
        let source = "type Count = Total type Total = Int32 type Context = Host.Context extern fn host(x: Count) -> Context fn f(x: Count) -> Total { let y: Total = x y }";
        let module = crate::parser::parse(source).unwrap();
        let resolved = types.resolve_aliases(&module).unwrap();
        assert!(resolved.type_aliases.is_empty());
        assert!(!types.contains("Count"));
        assert!(!types.contains("Context"));
        assert_eq!(module.type_aliases.len(), 3);
        let Item::ExternFn(host) = &resolved.items[0] else {
            panic!()
        };
        assert_eq!(host.params[0].ty, Type::Int32);
        assert_eq!(host.ret_ty, Type::Named("Host.Context".into()));
        let Item::FnDef(function) = &resolved.items[1] else {
            panic!()
        };
        assert_eq!(function.params[0].ty, Type::Int32);
        assert_eq!(function.ret_ty, Type::Int32);
        assert_eq!(function.params[0].span.slice(source), Some("x: Count"));
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        assert!(matches!(
            &exprs[0],
            Expr::Let {
                annotation: Some(Type::Int32),
                ..
            }
        ));
        let Item::FnDef(original) = &module.items[1] else {
            panic!()
        };
        assert_eq!(original.ret_ty, Type::Named("Total".into()));
        let again = types.resolve_aliases(&resolved).unwrap();
        assert_eq!(format!("{resolved:?}"), format!("{again:?}"));
    }

    #[test]
    fn unused_invalid_aliases_have_deterministic_precise_diagnostics() {
        let mut types = TypeRegistry::new();
        types.register_opaque("Context").unwrap();
        for (body, marked, code, message) in [
            (
                "type 数 = Missing",
                "Missing",
                ErrorCode::UnknownType,
                "Unknown type `Missing`",
            ),
            (
                "type A = B type B = Unknown.Target",
                "Unknown.Target",
                ErrorCode::UnknownType,
                "Unknown type `Unknown.Target`",
            ),
            (
                "type 数 = Int32 type 数 = Bool",
                "数",
                ErrorCode::DuplicateDefinition,
                "Type `数` is already defined",
            ),
            (
                "type Context = Int32",
                "Context",
                ErrorCode::DuplicateDefinition,
                "Type `Context` is already defined",
            ),
            ("type A = A", "A", ErrorCode::CyclicTypeAlias, "A -> A"),
            (
                "type A = B type B = C type C = A",
                "A",
                ErrorCode::CyclicTypeAlias,
                "A -> B -> C -> A",
            ),
            (
                "type Root = A type A = B type B = A",
                "A",
                ErrorCode::CyclicTypeAlias,
                "A -> B -> A",
            ),
        ] {
            let source = format!("// 🙂\n{body}");
            let module = crate::parser::parse(&source).unwrap();
            let error = types.resolve_aliases(&module).unwrap_err();
            assert_eq!(error.phase(), ErrorPhase::TypeCheck);
            assert_eq!(error.code(), code, "{body}");
            assert!(error.message().contains(message), "{body}: {error}");
            let start = source.rfind(marked).unwrap();
            assert_eq!(
                error.span(),
                Some(crate::Span::new(start, start + marked.len()))
            );
        }
    }

    #[test]
    fn long_alias_chains_resolve_iteratively_and_keep_opaque_identity() {
        let mut source = String::new();
        for i in 0..10_000 {
            source.push_str(&format!("type T{i} = T{}\n", i + 1));
        }
        source.push_str("type T10000 = vm.Handle fn f(x: T0) -> T10000 { x }");
        let module = crate::parser::parse(&source).unwrap();
        let resolved = TypeRegistry::default().resolve_aliases(&module).unwrap();
        let Item::FnDef(function) = &resolved.items[0] else {
            panic!()
        };
        assert_eq!(function.params[0].ty, Type::Named("vm.Handle".into()));
        assert_eq!(function.params[0].ty, function.ret_ty);
    }

    #[test]
    fn builtins_and_custom_opaque_types_are_registered_explicitly() {
        let mut types = TypeRegistry::new();
        assert!(types.contains(VM_HANDLE));
        assert!(types.contains(VM_FUNCTION));
        assert!(!types.contains("Host.Context"));
        types.register_opaque("Host.Context").unwrap();
        assert!(types.contains("Host.Context"));
        let duplicate = types.register_opaque("Host.Context").unwrap_err();
        assert_eq!(duplicate.phase(), ErrorPhase::Abi);
        assert_eq!(duplicate.code(), ErrorCode::AbiError);
        let invalid = types.register_opaque("not valid").unwrap_err();
        assert_eq!(invalid.phase(), ErrorPhase::Abi);
        assert_eq!(invalid.code(), ErrorCode::AbiError);
    }
}
