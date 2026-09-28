//! Named type registration shared by every compilation backend.
//!
//! Primitive types are part of the language. Source aliases are module-local;
//! opaque host types must be registered before compilation.

use crate::{
    parser::ast::{Expr, Item, Module, Type},
    Error, ErrorCode, ErrorPhase,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

mod layout;
mod specialization;
pub(crate) use specialization::substitute;

const VM_HANDLE: &str = "vm.Handle";
const VM_FUNCTION: &str = "vm.Function";

#[derive(Clone, Debug)]
pub struct TypeRegistry {
    named: BTreeSet<String>,
    pub(crate) module_types: BTreeMap<String, BTreeMap<String, String>>,
    pub(crate) prelude: bool,
    pub(crate) hosts: super::resolver::BindingRegistry,
    pub(crate) standard: std::rc::Rc<std::cell::OnceCell<super::imports::StandardLibrary>>,
    parameters: HashMap<u64, String>,
    enum_instances:
        std::rc::Rc<std::cell::RefCell<Vec<std::sync::Arc<crate::parser::ast::EnumType>>>>,
}

impl TypeRegistry {
    /// Create a registry containing the built-in VM opaque types.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable implicit standard-library imports for future compilations.
    pub fn set_prelude_enabled(&mut self, enabled: bool) {
        self.prelude = enabled;
    }

    /// Whether new source modules receive the default prelude.
    pub fn prelude_enabled(&self) -> bool {
        self.prelude
    }

    /// Configure implicit imports while retaining standard declaration identities.
    pub fn with_prelude(mut self, enabled: bool) -> Self {
        self.set_prelude_enabled(enabled);
        self
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

    /// Signature of a native function supplied by this module environment.
    pub fn host_function_signature(&self, name: &str) -> Option<crate::FunctionSignature> {
        self.hosts
            .binding(name)
            .map(|binding| binding.signature.clone())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.named.contains(name)
    }

    /// Resolve module-local records, enums and transparent aliases in all type annotations.
    ///
    /// Binds the configured prelude and explicit built-in source-library imports.
    /// Returns a copy with no alias declarations or alias type references.
    /// Non-alias names remain subject to ordinary opaque-type validation.
    /// The input and registered host types are unchanged. Run before entry-point,
    /// built-in call and ABI checks; VM and AOT loading do this automatically.
    /// Intrinsic function names are rejected before frontend-specific host lookup.
    pub fn resolve_aliases(&self, module: &Module) -> Result<Module, Error> {
        let expanded = super::imports::expand(module, self)?;
        self.resolve_bound(expanded.as_ref())
            .map_err(|error| error.with_sources(&expanded.sources))
    }

    /// Resolve imports exclusively from explicit in-memory inputs and the standard library.
    pub fn resolve_with_modules(
        &self,
        module: &Module,
        sources: &crate::ModuleSources,
    ) -> Result<Module, Error> {
        let expanded = super::imports::expand_with(module, self, &|path| sources.load(path))?;
        self.resolve_aliases(&expanded)
    }

    /// Read an entry file and resolve module paths relative to its directory.
    pub fn resolve_file(&self, path: impl AsRef<std::path::Path>) -> Result<Module, Error> {
        super::imports::resolve_file(path.as_ref(), self)
    }

    fn resolve_bound(&self, module: &Module) -> Result<Module, Error> {
        let scoped = self.compilation_scope();
        let registry = &scoped;
        let mut canonical = layout::resolve(registry, module)?;
        canonical.retain(|name, _| {
            !name.contains('.')
                || module
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.types.contains_key(*name))
        });
        let mut resolved = module.clone();
        resolved.type_aliases.clear();
        for record in &mut resolved.records {
            for field in &mut record.fields {
                resolve_type(&mut field.ty, &canonical, registry)
                    .map_err(|e| e.with_span(field.span))?;
            }
        }
        for enumeration in &mut resolved.enums {
            let local = registry.with_parameters(&enumeration.type_params)?;
            for variant in &mut enumeration.variants {
                if let Some(ty) = &mut variant.payload {
                    resolve_type(ty, &canonical, &local).map_err(|e| e.with_span(variant.span))?;
                }
            }
        }
        for implementation in &mut resolved.implementations {
            resolve_type(&mut implementation.target, &canonical, registry)
                .map_err(|e| e.with_span(implementation.span))?;
            let target = &implementation.target;
            if implementation.target_name != target.to_string()
                || !matches!(
                    target,
                    Type::Int32
                        | Type::Int64
                        | Type::UInt32
                        | Type::UInt64
                        | Type::Bool
                        | Type::Unit
                        | Type::Record(_)
                        | Type::Enum(_)
                )
                || matches!(target, Type::Enum(e) if !e.parameters.is_empty())
            {
                return Err(Error::new_with_code(ErrorPhase::TypeCheck, ErrorCode::TypeMismatch,
                    "impl requires a concrete Copy primitive, record or non-generic enum; aliases cannot declare implementations")
                    .with_span(implementation.span));
            }
            for constant in &mut implementation.constants {
                resolve_type(&mut constant.ty, &canonical, registry)
                    .map_err(|e| e.with_span(constant.span))?;
                resolve_annotations(&mut constant.value, &canonical, registry)?;
            }
            // Reuse the ordinary function normalization and ABI-independent checks.
            for method in &mut implementation.methods {
                if !method.type_params.is_empty() {
                    return Err(Error::new_with_code(
                        ErrorPhase::TypeCheck,
                        ErrorCode::TypeMismatch,
                        "Generic inherent methods are not supported yet",
                    )
                    .with_span(method.span));
                }
                for param in &mut method.params {
                    resolve_type(&mut param.ty, &canonical, registry)
                        .map_err(|e| e.with_span(param.span))?;
                }
                resolve_type(&mut method.ret_ty, &canonical, registry)
                    .map_err(|e| e.with_span(method.span))?;
                resolve_annotations(&mut method.body, &canonical, registry)?;
            }
        }
        for item in &mut resolved.items {
            let local = match item {
                Item::FnDef(f) => registry.with_parameters(&f.type_params)?,
                Item::ExternFn(_) => registry.clone(),
            };
            let (name, span) = match item {
                Item::FnDef(f) => (&f.name, f.span),
                Item::ExternFn(f) => (&f.name, f.span),
            };
            super::integer::IntegerType::check_declaration(name, span)?;
            let (params, ret) = match item {
                Item::FnDef(function) => {
                    resolve_annotations(&mut function.body, &canonical, &local)?;
                    (&mut function.params, &mut function.ret_ty)
                }
                Item::ExternFn(function) => (&mut function.params, &mut function.ret_ty),
            };
            for param in params {
                resolve_type(&mut param.ty, &canonical, &local)
                    .map_err(|e| e.with_span(param.span))?;
            }
            resolve_type(ret, &canonical, &local).map_err(|e| {
                e.with_span(match item {
                    Item::FnDef(f) => f.span,
                    Item::ExternFn(f) => f.span,
                })
            })?;
            if let Item::ExternFn(function) = item {
                if function.params.iter().any(|p| p.ty.is_aggregate())
                    || function.ret_ty.is_aggregate()
                {
                    return Err(Error::new_with_code(
                        ErrorPhase::Abi,
                        ErrorCode::AbiError,
                        "Aggregate parameters and returns are not supported by the host ABI",
                    )
                    .with_span(function.span));
                }
            }
        }
        Ok(resolved)
    }

    pub(crate) fn compilation_scope(&self) -> Self {
        let mut local = self.clone();
        local.enum_instances = Default::default();
        local
    }

    pub(crate) fn with_parameters(
        &self,
        params: &[std::sync::Arc<crate::parser::ast::TypeParameter>],
    ) -> Result<Self, Error> {
        let mut local = self.clone();
        let mut names = BTreeSet::new();
        for param in params {
            if params.len() > 64
                || !param
                    .name
                    .chars()
                    .next()
                    .is_some_and(|c| c == '_' || c.is_alphabetic())
                || !param.name.chars().all(|c| c == '_' || c.is_alphanumeric())
                || param.name == "_"
                || !names.insert(&param.name)
                || local
                    .parameters
                    .insert(param.id, param.name.clone())
                    .is_some()
            {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::DuplicateDefinition,
                    "Invalid or duplicate generic type parameter",
                )
                .with_span(param.span));
            }
        }
        Ok(local)
    }

    pub(crate) fn is_parameter_name(&self, name: &str) -> bool {
        self.parameters.values().any(|parameter| parameter == name)
    }

    pub(crate) fn validate(&self, ty: &Type) -> Result<(), Error> {
        let mut pending = vec![ty];
        let mut tuples = std::collections::HashSet::new();
        let mut enums = std::collections::HashSet::new();
        while let Some(ty) = pending.pop() {
            let message = match ty {
                Type::Applied(_) => Some("Type applications must be resolved before planning".into()),
                Type::Parameter(p) if !self.parameters.contains_key(&p.id) =>
                    Some(format!("Type parameter `{}` is outside its declaring function", p.name)),
                Type::Named(name) if !self.contains(name) => Some(format!(
                    "Unknown type `{name}`; declare a record, enum or alias, or register an opaque host type")),
                Type::Tuple(t) => {
                    if t.depth == 0 {
                        return Err(Error::new_with_code(ErrorPhase::TypeCheck, ErrorCode::InvalidState,
                            "Tuple annotations must be normalized before planning"));
                    }
                    if tuples.insert(std::sync::Arc::as_ptr(t)) { pending.extend(&t.elements); }
                    None
                }
                Type::Enum(e) => {
                    if enums.insert(std::sync::Arc::as_ptr(e)) {
                        pending.extend(&e.arguments);
                        pending.extend(&e.scalars);
                    }
                    None
                }
                _ => None,
            };
            if let Some(message) = message {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::UnknownType,
                    message,
                ));
            }
        }
        Ok(())
    }
}

fn resolve_type(
    ty: &mut Type,
    aliases: &HashMap<&str, Type>,
    registry: &TypeRegistry,
) -> Result<(), Error> {
    fn resolve(
        ty: &mut Type,
        aliases: &HashMap<&str, Type>,
        registry: &TypeRegistry,
        depth: usize,
        tuples: &mut HashMap<*const crate::parser::ast::TupleType, Type>,
    ) -> Result<(), Error> {
        if depth > 256 {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::ResourceLimit,
                "Tuple nesting exceeds 256 levels",
            ));
        }
        if let Type::Tuple(tuple) = ty {
            let key = std::sync::Arc::as_ptr(tuple);
            if let Some(resolved) = tuples.get(&key) {
                *ty = resolved.clone();
                return Ok(());
            }
            let mut elements = tuple.elements.clone();
            for element in &mut elements {
                resolve(element, aliases, registry, depth + 1, tuples)?;
            }
            let resolved = tuple_type(elements, registry)?;
            tuples.insert(key, resolved.clone());
            *ty = resolved;
            return Ok(());
        }
        if let Type::Applied(application) = ty {
            let crate::parser::ast::TypeApplication { name, arguments } = application.as_ref();
            if registry.is_parameter_name(name) {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::TypeMismatch,
                    "Type parameters cannot take type arguments",
                ));
            }
            let Some(Type::Enum(template)) = aliases.get(name.as_str()).filter(|ty|
                matches!(ty, Type::Enum(e) if !e.parameters.is_empty() && e.template.is_none())) else {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::UnknownType,
                    format!("Type `{name}` is not a generic enum"),
                ));
            };
            let mut arguments = arguments.clone();
            for arg in &mut arguments {
                resolve(arg, aliases, registry, depth + 1, tuples)?;
            }
            *ty = registry.instantiate_enum(template, arguments)?;
            return Ok(());
        }
        let named = matches!(ty, Type::Named(_));
        let name = match ty {
            Type::Named(name) => name.as_str(),
            Type::Record(record) => record.name.as_str(),
            Type::Enum(enumeration) => enumeration.name.as_str(),
            _ => return Ok(()),
        };
        if let Some(target) = aliases.get(name) {
            if named
                && matches!(target, Type::Enum(e) if !e.parameters.is_empty() && e.template.is_none())
            {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::TypeMismatch,
                    format!("Generic enum `{name}` requires type arguments"),
                ));
            }
            // Canonical nominal identities cannot cross source modules by spelling.
            if !ty.is_aggregate() || ty == target {
                *ty = target.clone();
            }
        }
        Ok(())
    }
    let original = ty.clone();
    let result = resolve(ty, aliases, registry, 0, &mut HashMap::new());
    drop(original);
    result
}

/// Construct the shared checked layout for inferred tuple expressions.
pub(crate) fn tuple_type(elements: Vec<Type>, registry: &TypeRegistry) -> Result<Type, Error> {
    layout::tuple_type(elements, registry)
}

fn resolve_annotations(
    expr: &mut Expr,
    aliases: &HashMap<&str, Type>,
    registry: &TypeRegistry,
) -> Result<(), Error> {
    map_annotations(expr, &mut |ty, span| {
        resolve_type(ty, aliases, registry).map_err(|e| e.with_span(span))
    })
}

pub(crate) fn map_annotations(
    expr: &mut Expr,
    map: &mut impl FnMut(&mut Type, crate::Span) -> Result<(), Error>,
) -> Result<(), Error> {
    match expr {
        Expr::GenericName(name) => {
            for ty in &mut name.type_args {
                map(ty, name.span)?;
            }
        }
        Expr::MethodCall(call) => {
            map_annotations(&mut call.receiver, map)?;
            for ty in &mut call.type_args {
                map(ty, call.span)?;
            }
            for arg in &mut call.args {
                map_annotations(arg, map)?;
            }
        }
        Expr::Match { value, arms, .. } => {
            map_annotations(value, map)?;
            for arm in arms {
                map_annotations(&mut arm.body, map)?;
            }
        }
        Expr::Record { fields, .. } => {
            for field in fields {
                map_annotations(&mut field.value, map)?;
            }
        }
        Expr::Field {
            base,
            type_args,
            span,
            ..
        } => {
            for ty in type_args {
                map(ty, *span)?;
            }
            map_annotations(base, map)?;
        }
        Expr::FieldAssign { target, value, .. } => {
            map_annotations(target, map)?;
            map_annotations(value, map)?;
        }
        Expr::Destructure {
            annotation, value, ..
        }
        | Expr::Let {
            annotation, value, ..
        }
        | Expr::Var {
            annotation, value, ..
        } => {
            if let Some(ty) = annotation {
                map(ty, value.span())?;
            }
            map_annotations(value, map)?;
        }
        Expr::Assign { value, .. }
        | Expr::Unary { operand: value, .. }
        | Expr::Return {
            value: Some(value), ..
        } => map_annotations(value, map)?,
        Expr::While {
            condition, body, ..
        } => {
            map_annotations(condition, map)?;
            map_annotations(body, map)?;
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            map_annotations(condition, map)?;
            map_annotations(then_branch, map)?;
            if let Some(branch) = else_branch {
                map_annotations(branch, map)?;
            }
        }
        Expr::BinOp { lhs, rhs, .. } | Expr::Logical { lhs, rhs, .. } => {
            map_annotations(lhs, map)?;
            map_annotations(rhs, map)?;
        }
        Expr::Call {
            type_args,
            args,
            span,
            ..
        } => {
            for ty in type_args {
                map(ty, *span)?;
            }
            for arg in args {
                map_annotations(arg, map)?;
            }
        }
        Expr::Tuple {
            elements: exprs, ..
        }
        | Expr::Block { exprs, .. }
        | Expr::Sequence { exprs, .. }
        | Expr::Scope {
            branches: exprs, ..
        } => {
            for expr in exprs {
                map_annotations(expr, map)?;
            }
        }
        Expr::Literal { .. }
        | Expr::Ident { .. }
        | Expr::FunctionRef { .. }
        | Expr::Return { value: None, .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
    }
    Ok(())
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self {
            prelude: true,
            hosts: Default::default(),
            standard: Default::default(),
            named: BTreeSet::from([VM_HANDLE.into(), VM_FUNCTION.into()]),
            module_types: BTreeMap::from([(
                "joky/vm".into(),
                BTreeMap::from([
                    ("Handle".into(), VM_HANDLE.into()),
                    ("Function".into(), VM_FUNCTION.into()),
                ]),
            )]),
            parameters: HashMap::new(),
            enum_instances: Default::default(),
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

/// Canonical spelling of each primitive type. These names are reserved and
/// can never be redeclared. Lookups that resolve member paths keep their own
/// narrower sets; for example `String` deliberately has no member namespace.
pub(crate) fn primitive_type(name: &str) -> Option<Type> {
    Some(match name {
        "Int32" => Type::Int32,
        "Int64" => Type::Int64,
        "UInt32" => Type::UInt32,
        "UInt64" => Type::UInt64,
        "Bool" => Type::Bool,
        "Unit" => Type::Unit,
        "String" => Type::String,
        _ => return None,
    })
}

/// Reuse declaration identities after normalization; the host registry remains immutable.
pub(crate) fn nominal_types(
    registry: &TypeRegistry,
    module: &Module,
) -> Result<HashMap<String, Type>, Error> {
    let canonical = layout::resolve(registry, module)?;
    let mut result: HashMap<String, Type> = module
        .records
        .iter()
        .map(|d| &d.name)
        .chain(module.enums.iter().map(|d| &d.name))
        .chain(module.type_aliases.iter().map(|d| &d.name))
        .map(|name| (name.clone(), canonical[name.as_str()].clone()))
        .collect();
    if let Some(scope) = &module.scope {
        for name in scope.types.keys() {
            result.insert(name.clone(), canonical[name.as_str()].clone());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_parameters_cannot_escape_through_canonical_tuple_layouts() {
        let registry = TypeRegistry::new();
        let module = registry
            .resolve_aliases(
                &crate::parser::parse("fn id<T>(x: ((T,),)) -> ((T,),) { x }").unwrap(),
            )
            .unwrap();
        let Item::FnDef(template) = &module.items[0] else {
            panic!()
        };
        let local = registry.with_parameters(&template.type_params).unwrap();
        local.validate(&template.ret_ty).unwrap();
        assert_eq!(
            registry.validate(&template.ret_ty).unwrap_err().code(),
            ErrorCode::UnknownType
        );
        let mut foreign = registry
            .resolve_aliases(
                &crate::parser::parse("fn other<T>(x: ((T,),)) -> ((T,),) { x }").unwrap(),
            )
            .unwrap();
        let Item::FnDef(other) = &mut foreign.items[0] else {
            panic!()
        };
        other.ret_ty = template.ret_ty.clone();
        let error = crate::core::ir::ModulePlan::with_import_layouts_and_types(
            &foreign,
            &HashMap::new(),
            &registry,
        )
        .err()
        .unwrap();
        assert_eq!(error.code(), ErrorCode::UnknownType);
        assert!(error.message().contains("outside its declaring function"));
    }

    #[test]
    fn tuples_normalize_structurally_with_nested_aliases_and_nominal_members() {
        let registry = TypeRegistry::new();
        let source = "type Pair = (Count, Bool) type Count = Int32 struct R { value: Pair } enum E { Value((R, Pair)) } fn f(x: ((Int32), Bool)) -> Pair { x }";
        let parsed = crate::parser::parse(source).unwrap();
        let resolved = registry.resolve_aliases(&parsed).unwrap();
        let again = registry.resolve_aliases(&resolved).unwrap();
        let signature = |module: &Module| {
            let Item::FnDef(f) = &module.items[0] else {
                panic!()
            };
            assert_eq!(f.params[0].ty, f.ret_ty);
            f.ret_ty.clone()
        };
        let ty = signature(&resolved);
        assert_eq!(ty.scalar_types(), &[Type::Int32, Type::Bool]);
        assert_eq!(ty, signature(&again));
        assert_eq!(
            ty,
            signature(
                &registry
                    .resolve_aliases(&crate::parser::parse(source).unwrap())
                    .unwrap()
            )
        );
        assert_eq!(format!("{resolved:?}"), format!("{again:?}"));
        assert_ne!(
            ty,
            tuple_type(vec![Type::Bool, Type::Int32], &registry).unwrap()
        );
        assert_ne!(
            tuple_type(vec![Type::Int32], &registry).unwrap(),
            Type::Int32
        );
        assert_ne!(tuple_type(vec![Type::Unit], &registry).unwrap(), Type::Unit);
        assert_eq!(
            tuple_type(vec![], &registry).unwrap_err().code(),
            ErrorCode::TypeMismatch
        );
        let source = "struct R {} fn f(x: (R,)) -> (R,) { x }";
        let old = registry
            .resolve_aliases(&crate::parser::parse(source).unwrap())
            .unwrap();
        let mut new = registry
            .resolve_aliases(&crate::parser::parse(source).unwrap())
            .unwrap();
        assert_ne!(signature(&old), signature(&new));
        let Item::FnDef(f) = &mut new.items[0] else {
            panic!()
        };
        f.ret_ty = signature(&old);
        let error = crate::core::ir::ModulePlan::with_import_layouts_and_types(
            &new,
            &HashMap::new(),
            &registry,
        )
        .err()
        .unwrap();
        assert_eq!(error.code(), ErrorCode::UnknownType);
        let mut foreign_member = crate::parser::parse("struct R { field: Int32 }").unwrap();
        foreign_member.records[0].fields[0].ty = signature(&old);
        assert_eq!(
            registry
                .resolve_aliases(&foreign_member)
                .unwrap_err()
                .code(),
            ErrorCode::UnknownType
        );
    }

    #[test]
    fn tuple_layout_limits_and_shared_zero_sized_graphs_are_bounded() {
        let registry = TypeRegistry::new().with_prelude(false);
        let mut wide = "type T0 = Int32".to_owned();
        for n in 1..=17 {
            wide.push_str(&format!(" type T{n} = (T{}, T{})", n - 1, n - 1));
        }
        let error = registry
            .resolve_aliases(&crate::parser::parse(&wide).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("65536"));
        let mut deep = "type T0 = Unit".to_owned();
        for n in 1..=257 {
            deep.push_str(&format!(" type T{n} = (T{},)", n - 1));
        }
        let error = registry
            .resolve_aliases(&crate::parser::parse(&deep).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("256"));
        let mut dag = "type T0 = Unit".to_owned();
        for n in 1..=35 {
            dag.push_str(&format!(" type T{n} = (T{}, T{})", n - 1, n - 1));
        }
        dag.push_str(" fn f(x: T35) -> T35 { x }");
        let first = registry
            .resolve_aliases(&crate::parser::parse(&dag).unwrap())
            .unwrap();
        let second = registry.resolve_aliases(&first).unwrap();
        let independently_parsed = registry
            .resolve_aliases(&crate::parser::parse(&dag).unwrap())
            .unwrap();
        let Item::FnDef(f) = &first.items[0] else {
            panic!()
        };
        let Item::FnDef(g) = &independently_parsed.items[0] else {
            panic!()
        };
        assert_eq!(f.ret_ty, g.ret_ty);
        assert!(format!("{second:?}").len() < 10000);
        assert!(f.ret_ty.scalar_types().is_empty());
        let spelling = f.ret_ty.to_string();
        assert!(spelling.len() < 4096);
        assert!(spelling.contains("..."));
        crate::core::ir::ModulePlan::with_import_layouts_and_types(
            &second,
            &HashMap::new(),
            &registry,
        )
        .unwrap();
    }

    #[test]
    fn enums_resolve_nominal_identity_aliases_and_finite_typed_layouts() {
        let registry = TypeRegistry::new();
        let source = "type Alias = E enum E { Empty Number(Int32) Flag(Bool) Payload(R) } struct R { child: Inner } enum Inner { A B(Unit) } fn f(e: Alias) -> E { e }";
        let parsed = crate::parser::parse(source).unwrap();
        let resolved = registry.resolve_aliases(&parsed).unwrap();
        let again = registry.resolve_aliases(&resolved).unwrap();
        let signature = |module: &Module| {
            let Item::FnDef(f) = &module.items[0] else {
                panic!()
            };
            assert_eq!(f.params[0].ty, f.ret_ty);
            f.ret_ty.clone()
        };
        let ty = signature(&resolved);
        assert_eq!(ty, signature(&again));
        assert_eq!(
            ty.scalar_types(),
            &[Type::Int32, Type::Int32, Type::Bool, Type::Int32]
        );
        assert_ne!(
            ty,
            signature(
                &registry
                    .resolve_aliases(&crate::parser::parse(source).unwrap())
                    .unwrap()
            )
        );
        assert!(!registry.contains("E"));
        assert_eq!(format!("{resolved:?}"), format!("{again:?}"));
        // Public canonical types cannot cross source-module identities, even
        // when a fresh declaration uses the same name and layout.
        let mut foreign = registry
            .resolve_aliases(&crate::parser::parse(source).unwrap())
            .unwrap();
        let Item::FnDef(f) = &mut foreign.items[0] else {
            panic!()
        };
        f.ret_ty = ty.clone();
        let error = crate::core::ir::ModulePlan::with_import_layouts_and_types(
            &foreign,
            &HashMap::new(),
            &registry,
        )
        .err()
        .unwrap();
        assert_eq!(error.code(), ErrorCode::UnknownType);
        foreign.enums[0].variants[0].payload = Some(ty);
        assert_eq!(
            registry.resolve_aliases(&foreign).unwrap_err().code(),
            ErrorCode::UnknownType
        );
        let mut wide = "enum E0 { A }".to_owned();
        for n in 1..=16 {
            wide.push_str(&format!(" enum E{n} {{ A(E{}) B(E{}) }}", n - 1, n - 1));
        }
        let error = registry
            .resolve_aliases(&crate::parser::parse(&wide).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("65536"));
        let mut deep = "enum E0 { A }".to_owned();
        for n in 1..=256 {
            deep.push_str(&format!(" enum E{n} {{ A(E{}) }}", n - 1));
        }
        let error = registry
            .resolve_aliases(&crate::parser::parse(&deep).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("256"));
    }

    #[test]
    fn records_resolve_forward_aliases_identity_and_layout_limits() {
        let registry = TypeRegistry::new();
        let source = "type P = Pair struct Pair { x: Count empty: Empty flag: Bool } type Count = Int32 struct Empty {} fn f(p: P) -> Pair { p }";
        let parsed = crate::parser::parse(source).unwrap();
        let resolved = registry.resolve_aliases(&parsed).unwrap();
        let again = registry.resolve_aliases(&resolved).unwrap();
        let signature = |module: &Module| {
            let Item::FnDef(f) = &module.items[0] else {
                panic!()
            };
            assert_eq!(f.params[0].ty, f.ret_ty);
            f.ret_ty.clone()
        };
        let ty = signature(&resolved);
        assert_eq!(ty, signature(&again));
        assert_eq!(ty.scalar_types(), &[Type::Int32, Type::Bool]);
        assert_ne!(
            ty,
            signature(
                &registry
                    .resolve_aliases(&crate::parser::parse(source).unwrap())
                    .unwrap()
            )
        );
        assert!(!registry.contains("Pair"));
        assert!(!parsed.type_aliases.is_empty());
        let mut large = "struct R0 { value: Int32 }".to_owned();
        for n in 1..=17 {
            large.push_str(&format!(
                " struct R{n} {{ left: R{} right: R{} }}",
                n - 1,
                n - 1
            ));
        }
        let error = registry
            .resolve_aliases(&crate::parser::parse(&large).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("65536"));
        let mut deep = "struct R0 {}".to_owned();
        for n in 1..=256 {
            deep.push_str(&format!(" struct R{n} {{ inner: R{} }}", n - 1));
        }
        let error = registry
            .resolve_aliases(&crate::parser::parse(&deep).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("256"));
    }

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

#[cfg(test)]
mod generic_enum_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn generic_enum_instances_cache_aliases_keep_identity_and_validate_phantom_arguments() {
        let registry = TypeRegistry::new().with_prelude(false);
        let parsed = crate::parser::parse("enum Option<T> { None Some(T) } enum Phantom<T> { Value } type Alias = Int32 fn a(x: Option<Int32>) {} fn b(x: Option<Alias>) {} fn c(x: Option<Bool>) {} fn template<T>(x: Phantom<T>) {}").unwrap();
        let normalized = registry.resolve_aliases(&parsed).unwrap();
        let again = registry.resolve_aliases(&normalized).unwrap();
        let param = |module: &Module, index: usize| {
            let Item::FnDef(f) = &module.items[index] else {
                panic!()
            };
            f.params[0].ty.clone()
        };
        let a = param(&normalized, 0);
        let b = param(&normalized, 1);
        let (Type::Enum(left), Type::Enum(right)) = (&a, &b) else {
            panic!()
        };
        assert!(Arc::ptr_eq(left, right));
        assert_eq!(a, b);
        assert_ne!(a, param(&normalized, 2));
        assert_eq!(a, param(&again, 0));
        assert_eq!(a.to_string(), "Option<Int32>");
        let fresh = registry
            .resolve_aliases(
                &crate::parser::parse("enum Option<T> { None Some(T) } fn a(x: Option<Int32>) {}")
                    .unwrap(),
            )
            .unwrap();
        assert_ne!(a, param(&fresh, 0));
        let Item::FnDef(template) = &normalized.items[3] else {
            panic!()
        };
        let phantom = param(&normalized, 3);
        registry
            .with_parameters(&template.type_params)
            .unwrap()
            .validate(&phantom)
            .unwrap();
        assert_eq!(
            registry.validate(&phantom).unwrap_err().code(),
            ErrorCode::UnknownType
        );
        let mut foreign = fresh;
        let Item::FnDef(f) = &mut foreign.items[0] else {
            panic!()
        };
        f.params[0].ty = a;
        let error = crate::core::ir::ModulePlan::with_import_layouts_and_types(
            &foreign,
            &HashMap::new(),
            &registry,
        )
        .err()
        .unwrap();
        assert_eq!(error.code(), ErrorCode::UnknownType);
        assert!(registry.enum_instances.borrow().is_empty());
    }

    #[test]
    fn generic_enum_shared_phantom_graphs_have_bounded_comparison_and_substitution() {
        let registry = TypeRegistry::new();
        let mut source = "enum Phantom<T> { Value } type T0 = Unit".to_owned();
        for n in 1..=30 {
            source.push_str(&format!(" type T{n} = Phantom<(T{}, T{})>", n - 1, n - 1));
        }
        source.push_str(" fn identity<T>(x: T) -> T { x } fn hold(x: T30) -> T30 { identity(x) }");
        let module = crate::parser::parse(&source).unwrap();
        let a = registry.resolve_aliases(&module).unwrap();
        let b = registry.resolve_aliases(&module).unwrap();
        let Item::FnDef(f) = &a.items[1] else {
            panic!()
        };
        let Item::FnDef(g) = &b.items[1] else {
            panic!()
        };
        assert_eq!(f.ret_ty, g.ret_ty);
        assert!(f.ret_ty.to_string().len() < 6000);
        crate::core::ir::ModulePlan::with_import_layouts_and_types(&a, &HashMap::new(), &registry)
            .unwrap();
    }
}
