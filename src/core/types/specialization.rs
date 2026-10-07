//! Bounded nominal specialization and substitution shared with generic functions.
use super::*;
use crate::parser::ast::{
    EnumType, EnumVariant, RecordField, RecordType, TupleType, TypeParameter,
};
use std::sync::Arc;

// Nominal equality intentionally ignores layout metadata. A forward reference
// may gain ownership after its declaration is resolved; do not reuse an alias
// expansion checked against the earlier Copy classification.
fn owner_classifications(ty: &Type) -> BTreeSet<(u64, bool)> {
    TypeWalk::new(ty, TypeEdges::Arguments)
        .filter_map(|ty| match ty {
            Type::Record(record) => Some((record.id, !record.owners.is_empty())),
            _ => None,
        })
        .collect()
}

impl TypeRegistry {
    pub(crate) fn instantiate_alias(
        &self,
        alias: &Arc<AliasTemplate>,
        arguments: Vec<Type>,
    ) -> Result<Type, Error> {
        self.with_alias_obligation(&alias.name, &arguments, || {
            self.instantiate_alias_inner(alias, arguments.clone())
        })
    }

    fn instantiate_alias_inner(
        &self,
        alias: &Arc<AliasTemplate>,
        arguments: Vec<Type>,
    ) -> Result<Type, Error> {
        if alias.parameters.len() != arguments.len() {
            return Err(error(
                ErrorCode::TypeMismatch,
                format!(
                    "Type argument count mismatch for alias `{}`: expected {}",
                    alias.name,
                    alias.parameters.len()
                ),
            ));
        }
        // Validate even unused parameters before erasing the transparent alias.
        for arg in &arguments {
            self.validate(arg)?;
            if arg.contains_closures() || !arg.is_copy_storage_value() && !arg.is_owner() {
                return Err(error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "Generic alias argument must be a Copy type or owning record, got {arg}"
                    ),
                ));
            }
        }
        // Imports share the template. Re-normalized declarations retain their
        // parameters, but public AST callers can change targets between passes.
        let cached = self
            .alias_instances
            .borrow()
            .iter()
            .find(|instance| {
                (Arc::ptr_eq(&instance.template, alias)
                    || (instance.template.parameters == alias.parameters
                        && instance.template.target == alias.target
                        && owner_classifications(&instance.template.target)
                            == owner_classifications(&alias.target)))
                    && instance.arguments == arguments
            })
            .map(|instance| instance.target.clone());
        // A compilation cache has no request-local dependency height. Replay
        // expansion inside the solver so a warm alias cannot evade its budget.
        if !self.solving_traits() {
            if let Some(target) = &cached {
                return Ok(target.clone());
            }
        }
        if cached.is_none() && self.alias_instances.borrow().len() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Generic alias expansion exceeds 256 instances",
            ));
        }
        let substitutions = alias
            .parameters
            .iter()
            .zip(&arguments)
            .map(|(p, ty)| (p.id, ty.clone()))
            .collect();
        let mut target = substitute(&alias.target, &substitutions, self)?;
        let definitions = self.forwards.borrow().clone();
        let canonical = definitions
            .iter()
            .map(|(name, definition)| (name.as_str(), definition.clone()))
            .collect();
        resolve_type(&mut target, &canonical, self)?;
        self.validate(&target)?;
        if cached.is_none() {
            self.alias_instances.borrow_mut().push(AliasInstance {
                template: alias.clone(),
                arguments,
                target: target.clone(),
            });
        }
        Ok(target)
    }

    pub(crate) fn instantiate_record(
        &self,
        record: &Arc<RecordType>,
        arguments: Vec<Type>,
    ) -> Result<Type, Error> {
        let template = record.template.as_ref().unwrap_or(record);
        if template.parameters.is_empty() || template.parameters.len() != arguments.len() {
            return Err(error(
                ErrorCode::TypeMismatch,
                format!(
                    "Type argument count mismatch for record `{}`: expected {}",
                    template.name,
                    template.parameters.len()
                ),
            ));
        }
        for arg in &arguments {
            self.validate(arg)?;
            if arg.contains_closures() || !arg.is_copy_storage_value() && !arg.is_owner() {
                return Err(error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "Generic record argument must be a Copy type or owning record, got {arg}"
                    ),
                ));
            }
        }
        if let Some(instance) = self
            .record_instances
            .borrow()
            .iter()
            .find(|r| r.id == template.id && r.arguments == arguments)
        {
            return Ok(Type::Record(instance.clone()));
        }
        if self.record_instances.borrow().len() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Generic record instantiation exceeds 256 instances",
            ));
        }
        let substitutions = template
            .parameters
            .iter()
            .zip(&arguments)
            .map(|(p, ty)| (p.id, ty.clone()))
            .collect();
        let mut scalars = Vec::new();
        // Phantom arguments contribute to depth even when they occupy no lanes.
        let mut depth = arguments
            .iter()
            .map(|ty| ty.layout_depth() + 1)
            .max()
            .unwrap_or(1);
        if depth > 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Record type nesting exceeds 256 levels",
            ));
        }
        let mut fields = Vec::new();
        for field in &template.fields {
            let offset = scalars.len();
            let ty = substitute(&field.ty, &substitutions, self)?;
            layout::extend_layout(
                &mut scalars,
                &mut depth,
                &ty,
                crate::Span::empty(0),
                "Record",
            )?;
            fields.push(RecordField {
                private: field.private,
                name: field.name.clone(),
                ty,
                offset,
            });
        }
        let instance = Arc::new(RecordType {
            closure: None,
            owners: layout::record_owners(&fields, crate::Span::empty(0))?,
            repr_c: template.repr_c,
            indirect: false,
            id: template.id,
            name: template.name.clone(),
            parameters: template.parameters.clone(),
            arguments,
            template: Some(template.clone()),
            fields,
            scalars,
            depth,
        });
        // Substituting nested fields can populate this shared cache too.
        if self.record_instances.borrow().len() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Generic record instantiation exceeds 256 instances",
            ));
        }
        self.record_instances.borrow_mut().push(instance.clone());
        Ok(Type::Record(instance))
    }

    pub(crate) fn instantiate_enum(
        &self,
        enumeration: &Arc<EnumType>,
        arguments: Vec<Type>,
    ) -> Result<Type, Error> {
        let template = enumeration.template.as_ref().unwrap_or(enumeration);
        if template.parameters.is_empty() || template.parameters.len() != arguments.len() {
            return Err(error(
                ErrorCode::TypeMismatch,
                format!(
                    "Type argument count mismatch for enum `{}`: expected {}",
                    template.name,
                    template.parameters.len()
                ),
            ));
        }
        for arg in &arguments {
            self.validate(arg)?;
            if arg.contains_closures() || !arg.is_copy_storage_value() && !arg.is_owner() {
                return Err(error(
                    ErrorCode::TypeMismatch,
                    format!("Generic enum argument must be a Copy type or an owner, got {arg}"),
                ));
            }
        }
        if let Some(instance) = self
            .enum_instances
            .borrow()
            .iter()
            .find(|e| e.id == template.id && e.arguments == arguments)
        {
            return Ok(Type::Enum(instance.clone()));
        }
        if self.enum_instances.borrow().len() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Generic enum instantiation exceeds 256 instances",
            ));
        }
        let substitutions = template
            .parameters
            .iter()
            .zip(&arguments)
            .map(|(p, ty)| (p.id, ty.clone()))
            .collect();
        let mut scalars = vec![Type::Int32];
        let mut depth = arguments
            .iter()
            .map(|ty| ty.layout_depth() + 1)
            .max()
            .unwrap_or(1);
        if depth > 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Enum type nesting exceeds 256 levels",
            ));
        }
        let mut variants = Vec::new();
        for variant in &template.variants {
            let offset = scalars.len();
            let payload = variant
                .payload
                .as_ref()
                .map(|ty| substitute(ty, &substitutions, self))
                .transpose()?;
            if let Some(ty) = &payload {
                layout::extend_layout(&mut scalars, &mut depth, ty, crate::Span::empty(0), "Enum")?;
            }
            variants.push(EnumVariant {
                name: variant.name.clone(),
                payload,
                offset,
            });
        }
        let instance = Arc::new(EnumType {
            owners: layout::enum_owners(&variants, crate::Span::empty(0))?,
            indirect: false,
            id: template.id,
            name: template.name.clone(),
            parameters: template.parameters.clone(),
            arguments,
            template: Some(template.clone()),
            variants,
            scalars,
            depth,
        });
        // Nested payload applications may also have populated the shared cache.
        if self.enum_instances.borrow().len() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Generic enum instantiation exceeds 256 instances",
            ));
        }
        self.enum_instances.borrow_mut().push(instance.clone());
        Ok(Type::Enum(instance))
    }
}

pub(crate) fn substitute(
    ty: &Type,
    substitutions: &HashMap<u64, Type>,
    types: &TypeRegistry,
) -> Result<Type, Error> {
    #[derive(Default)]
    struct Cache {
        functions: HashMap<*const crate::parser::ast::FunctionType, Type>,
        tuples: HashMap<*const TupleType, Type>,
        enums: HashMap<*const EnumType, Type>,
        records: HashMap<*const RecordType, Type>,
    }
    fn visit(
        ty: &Type,
        substitutions: &HashMap<u64, Type>,
        types: &TypeRegistry,
        cache: &mut Cache,
    ) -> Result<Type, Error> {
        Ok(match ty {
            Type::Function(signature) => {
                let key = Arc::as_ptr(signature);
                if let Some(ty) = cache.functions.get(&key) {
                    return Ok(ty.clone());
                }
                let ty = Type::function_with_effect(
                    signature
                        .signature
                        .params
                        .iter()
                        .map(|p| visit(p, substitutions, types, cache))
                        .collect::<Result<_, _>>()?,
                    visit(&signature.signature.ret, substitutions, types, cache)?,
                    signature.suspends,
                );
                cache.functions.insert(key, ty.clone());
                ty
            }
            Type::Buffer(element) | Type::BufferElement(element) | Type::BufferSlice(element) => {
                let result = ty.with_buffer_element(visit(element, substitutions, types, cache)?);
                types.validate(&result)?;
                result
            }
            Type::Reference(r) => {
                let target = match &r.target {
                    Type::Record(record) => indirect::shallow(
                        &r.target,
                        record
                            .arguments
                            .iter()
                            .map(|t| visit(t, substitutions, types, cache))
                            .collect::<Result<_, _>>()?,
                    ),
                    Type::Enum(enumeration) => indirect::shallow(
                        &r.target,
                        enumeration
                            .arguments
                            .iter()
                            .map(|t| visit(t, substitutions, types, cache))
                            .collect::<Result<_, _>>()?,
                    ),
                    _ => visit(&r.target, substitutions, types, cache)?,
                };
                let target = if (target.nominal().is_some() || matches!(target, Type::Tuple(_)))
                    && super::TypeWalk::new(&target, super::TypeEdges::Arguments)
                        .any(Type::is_owner)
                {
                    let definitions = types.forwards.borrow().clone();
                    let canonical = definitions
                        .iter()
                        .map(|(name, definition)| (name.as_str(), definition.clone()))
                        .collect();
                    indirect::resolve_target(&target, &canonical, types, 0)?
                } else {
                    target
                };
                let result = Type::reference(target, r.mutable);
                types.validate(&result)?;
                result
            }
            Type::Applied(application) => {
                Type::Applied(Arc::new(crate::parser::ast::TypeApplication {
                    name: application.name.clone(),
                    arguments: application
                        .arguments
                        .iter()
                        .map(|t| visit(t, substitutions, types, cache))
                        .collect::<Result<_, _>>()?,
                }))
            }
            Type::Projection(p) => {
                let mut p = p.as_ref().clone();
                p.base = visit(&p.base, substitutions, types, cache)?;
                types.normalize_projection(&p, &HashMap::new())?
            }
            Type::Parameter(p) => substitutions
                .get(&p.id)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            Type::Tuple(tuple) => {
                let key = Arc::as_ptr(tuple);
                if let Some(ty) = cache.tuples.get(&key) {
                    return Ok(ty.clone());
                }
                let elements = tuple
                    .elements
                    .iter()
                    .map(|ty| visit(ty, substitutions, types, cache))
                    .collect::<Result<_, _>>()?;
                let ty = tuple_type(elements, types)?;
                cache.tuples.insert(key, ty.clone());
                ty
            }
            Type::Enum(enumeration) if !enumeration.parameters.is_empty() => {
                let key = Arc::as_ptr(enumeration);
                if let Some(ty) = cache.enums.get(&key) {
                    return Ok(ty.clone());
                }
                let arguments = enumeration
                    .arguments
                    .iter()
                    .map(|ty| visit(ty, substitutions, types, cache))
                    .collect::<Result<_, _>>()?;
                let ty = if enumeration.indirect {
                    indirect::shallow(ty, arguments)
                } else {
                    types.instantiate_enum(enumeration, arguments)?
                };
                cache.enums.insert(key, ty.clone());
                ty
            }
            Type::Record(record) if !record.parameters.is_empty() => {
                let key = Arc::as_ptr(record);
                if let Some(ty) = cache.records.get(&key) {
                    return Ok(ty.clone());
                }
                let arguments = record
                    .arguments
                    .iter()
                    .map(|ty| visit(ty, substitutions, types, cache))
                    .collect::<Result<_, _>>()?;
                // Nested arguments of a reference target can also be shallow.
                // Do not cache their absent fields as a concrete value layout.
                let ty = if record.indirect {
                    indirect::shallow(ty, arguments)
                } else {
                    types.instantiate_record(record, arguments)?
                };
                cache.records.insert(key, ty.clone());
                ty
            }
            _ => ty.clone(),
        })
    }
    visit(ty, substitutions, types, &mut Cache::default())
}

fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message)
}

pub(crate) fn unify(
    formal: &Type,
    actual: &Type,
    params: &[Arc<TypeParameter>],
    substitutions: &mut HashMap<u64, Type>,
    span: Span,
) -> Result<(), Error> {
    // Iterative tuple matching avoids expanding shared zero-sized type DAGs.
    let mut pending = vec![(formal, actual)];
    let mut seen = std::collections::HashSet::new();
    let mut enums = std::collections::HashSet::new();
    let mut records = std::collections::HashSet::new();
    let mut functions = std::collections::HashSet::new();
    while let Some((formal, actual)) = pending.pop() {
        match (formal, actual) {
            (Type::Projection(_), _) => {} // Checked after ordinary parameter inference.

            (Type::Function(a), Type::Function(b))
                if a.suspends == b.suspends
                    && a.signature.params.len() == b.signature.params.len() =>
            {
                if functions.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.signature.params.iter().zip(&b.signature.params));
                    pending.push((&a.signature.ret, &b.signature.ret));
                }
            }
            (Type::Buffer(a), Type::Buffer(b))
            | (Type::BufferElement(a), Type::BufferElement(b))
            | (Type::BufferSlice(a), Type::BufferSlice(b)) => pending.push((a, b)),
            (Type::Reference(a), Type::Reference(b)) if a.mutable == b.mutable || !a.mutable => {
                pending.push((&a.target, &b.target))
            }
            (Type::Parameter(p), _) if params.iter().any(|param| param.id == p.id) => {
                if let Some(previous) = substitutions.get(&p.id) {
                    if previous != actual {
                        return Err(unification_error(span, ErrorCode::TypeMismatch,
                            format!("Conflicting types for generic parameter `{}`: {previous} and {actual}", p.name)));
                    }
                } else {
                    substitutions.insert(p.id, actual.clone());
                }
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.elements.len() == b.elements.len() => {
                if seen.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.elements.iter().zip(&b.elements));
                }
            }
            (Type::Enum(a), Type::Enum(b))
                if a.id == b.id && a.arguments.len() == b.arguments.len() =>
            {
                if enums.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.arguments.iter().zip(&b.arguments));
                }
            }
            (Type::Record(a), Type::Record(b))
                if a.id == b.id && a.arguments.len() == b.arguments.len() =>
            {
                if records.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.arguments.iter().zip(&b.arguments));
                }
            }
            _ if formal == actual => {}
            _ => {
                return Err(unification_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!("Generic argument type mismatch: expected {formal}, got {actual}"),
                ))
            }
        }
    }
    Ok(())
}

fn unification_error(span: Span, code: ErrorCode, message: impl Into<String>) -> Error {
    error(code, message).with_span(span)
}
