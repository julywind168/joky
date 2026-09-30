//! Resolve finite nominal layouts and aliases without recursing on source graphs.
use super::*;
use crate::{
    parser::ast::{
        EnumDef, EnumType, EnumVariant, RecordDef, RecordField, RecordType, TupleType, TypeAlias,
    },
    Span,
};
use std::{collections::HashSet, sync::Arc};

#[derive(Clone, Copy)]
enum Declaration<'a> {
    Record(&'a RecordDef),
    Enum(&'a EnumDef),
    Alias(&'a TypeAlias),
}
impl<'a> Declaration<'a> {
    fn name(self) -> &'a str {
        match self {
            Self::Record(d) => &d.name,
            Self::Enum(d) => &d.name,
            Self::Alias(d) => &d.name,
        }
    }
    fn span(self) -> Span {
        match self {
            Self::Record(d) => d.name_span,
            Self::Enum(d) => d.name_span,
            Self::Alias(d) => d.name_span,
        }
    }
    fn id(self) -> Option<u64> {
        match self {
            Self::Record(d) => Some(d.id),
            Self::Enum(d) => Some(d.id),
            Self::Alias(_) => None,
        }
    }
    fn dependencies(self) -> Vec<(&'a Type, Span)> {
        match self {
            Self::Record(d) => d
                .fields
                .iter()
                .map(|field| (&field.ty, field.span))
                .collect(),
            Self::Enum(d) => d
                .variants
                .iter()
                .filter_map(|v| v.payload.as_ref().map(|ty| (ty, v.span)))
                .collect(),
            Self::Alias(d) => vec![(&d.target, d.target_span)],
        }
    }
}

pub(super) fn resolve<'a>(
    registry: &TypeRegistry,
    module: &'a Module,
) -> Result<HashMap<&'a str, TypeDefinition>, Error> {
    let declarations: Vec<_> = module
        .records
        .iter()
        .map(Declaration::Record)
        .chain(module.enums.iter().map(Declaration::Enum))
        .chain(module.type_aliases.iter().map(Declaration::Alias))
        .collect();
    let mut names = HashMap::new();
    for (index, &declaration) in declarations.iter().enumerate() {
        let name = declaration.name();
        if registry.contains(name)
            || primitive_type(name).is_some()
            || names.insert(name, index).is_some()
        {
            return Err(error(
                ErrorCode::DuplicateDefinition,
                declaration.span(),
                format!("Type `{name}` is already defined"),
            ));
        }
        let mut members = HashSet::new();
        match declaration {
            Declaration::Record(record) => {
                crate::core::ffi::validate_attributes(record)?;
                for field in &record.fields {
                    if !members.insert(&field.name) {
                        return Err(error(
                            ErrorCode::DuplicateDefinition,
                            field.span,
                            format!("Duplicate field `{}` in `{name}`", field.name),
                        ));
                    }
                }
            }
            Declaration::Enum(enumeration) => {
                if enumeration.variants.is_empty() {
                    return Err(error(
                        ErrorCode::InvalidPattern,
                        enumeration.span,
                        "An enum must declare at least one variant",
                    ));
                }
                if enumeration.variants.len() > i32::MAX as usize {
                    return Err(error(
                        ErrorCode::ResourceLimit,
                        enumeration.span,
                        "Enum variant count exceeds the Int32 tag range",
                    ));
                }
                for variant in &enumeration.variants {
                    if variant.name == "_" || !members.insert(&variant.name) {
                        return Err(error(
                            ErrorCode::DuplicateDefinition,
                            variant.span,
                            format!(
                                "Duplicate or reserved variant `{}` in `{name}`",
                                variant.name
                            ),
                        ));
                    }
                }
            }
            Declaration::Alias(_) => {}
        }
    }
    if let Some(scope) = &module.scope {
        for (name, binding) in &scope.types {
            if let Some(id) = binding.declaration {
                let index = declarations
                    .iter()
                    .position(|d| d.id() == Some(id))
                    .ok_or_else(|| {
                        error(
                            ErrorCode::UnknownType,
                            Span::empty(0),
                            "Imported declaration is missing",
                        )
                    })?;
                names.insert(name.as_str(), index);
            }
        }
    }
    // Reference targets need nominal identity, not an inline layout. Keep
    // shallow identities in a separate namespace; never create strong Arc cycles.
    let mut forwards = HashMap::new();
    for (name, &index) in &names {
        let definition = match declarations[index] {
            Declaration::Record(d) => TypeDefinition::Type(Type::Record(Arc::new(RecordType {
                owner: None,
                repr_c: d.repr_c(),
                indirect: true,
                id: d.id,
                name: d.name.clone(),
                parameters: d.type_params.clone(),
                arguments: d.type_params.iter().cloned().map(Type::Parameter).collect(),
                template: None,
                fields: Vec::new(),
                scalars: Vec::new(),
                depth: 1,
            }))),
            Declaration::Enum(d) => TypeDefinition::Type(Type::Enum(Arc::new(EnumType {
                indirect: true,
                id: d.id,
                name: d.name.clone(),
                parameters: d.type_params.clone(),
                arguments: d.type_params.iter().cloned().map(Type::Parameter).collect(),
                template: None,
                variants: Vec::new(),
                scalars: Vec::new(),
                depth: 1,
            }))),
            Declaration::Alias(d) => TypeDefinition::Alias(Arc::new(AliasTemplate {
                name: d.name.clone(),
                parameters: d.type_params.clone(),
                target: d.target.clone(),
            })),
        };
        forwards.insert((*name).to_owned(), definition);
    }
    *registry.forwards.borrow_mut() = forwards;
    let mut edges = vec![Vec::new(); declarations.len()];
    for (index, &declaration) in declarations.iter().enumerate() {
        let mut pending = declaration.dependencies();
        pending.reverse();
        let mut seen_tuples = HashSet::new();
        let mut seen_functions = HashSet::new();
        let mut seen_enums = HashSet::new();
        let mut seen_records = HashSet::new();
        while let Some((ty, span)) = pending.pop() {
            if let Type::Applied(application) = ty {
                let crate::parser::ast::TypeApplication { name, arguments } = application.as_ref();
                let parameters = match declaration {
                    Declaration::Record(d) => &d.type_params,
                    Declaration::Enum(d) => &d.type_params,
                    Declaration::Alias(d) => &d.type_params,
                };
                if parameters.iter().any(|p| p.name == *name) {
                    return Err(error(
                        ErrorCode::TypeMismatch,
                        span,
                        "Type parameters cannot take type arguments",
                    ));
                }
                if let Some(&target) = names.get(name.as_str()) {
                    edges[index].push((target, span));
                } else if !module.scope.as_ref().is_some_and(|scope| {
                    scope
                        .types
                        .get(name)
                        .is_some_and(|binding| binding.resolved.is_some())
                }) {
                    return Err(error(
                        ErrorCode::UnknownType,
                        span,
                        format!("Unknown type `{name}`"),
                    ));
                }
                pending.extend(arguments.iter().rev().map(|ty| (ty, span)));
                continue;
            }
            if matches!(ty, Type::Reference(_)) {
                continue; // A pointer breaks inline layout recursion.
            }
            if let Type::Function(signature) = ty {
                if seen_functions.insert(Arc::as_ptr(signature)) {
                    pending.extend(signature.signature.params.iter().map(|ty| (ty, span)));
                    pending.push((&signature.signature.ret, span));
                }
                continue;
            }
            if let Type::Tuple(tuple) = ty {
                if !seen_tuples.insert(Arc::as_ptr(tuple)) {
                    continue;
                }
                pending.extend(tuple.elements.iter().rev().map(|ty| (ty, span)));
                continue;
            }
            if let Type::Enum(enumeration) = ty {
                if seen_enums.insert(Arc::as_ptr(enumeration)) {
                    pending.extend(enumeration.arguments.iter().rev().map(|ty| (ty, span)));
                }
            }
            if let Type::Record(record) = ty {
                if seen_records.insert(Arc::as_ptr(record)) {
                    pending.extend(record.arguments.iter().rev().map(|ty| (ty, span)));
                }
            }
            let target = match ty {
                Type::Named(name) => names.get(name.as_str()).copied(),
                _ => {
                    if let Some((name, id)) = ty.nominal() {
                        Some(
                            names
                                .get(name)
                                .copied()
                                .filter(|&i| declarations[i].id() == Some(id))
                                .ok_or_else(|| {
                                    error(
                                        ErrorCode::UnknownType,
                                        span,
                                        format!(
                                            "Type `{name}` does not belong to this source module"
                                        ),
                                    )
                                })?,
                        )
                    } else {
                        None
                    }
                }
            };
            if let Some(target) = target {
                edges[index].push((target, span));
            }
        }
    }
    let mut state = vec![0u8; declarations.len()];
    let mut canonical = HashMap::new();
    let mut aliases_to_refresh = Vec::new();
    if let Some(scope) = &module.scope {
        for (name, binding) in &scope.types {
            if binding.declaration.is_none() {
                canonical.insert(
                    name.as_str(),
                    binding
                        .resolved
                        .clone()
                        .unwrap_or_else(|| TypeDefinition::Type(Type::Named(binding.name.clone()))),
                );
            }
        }
    }
    for root in 0..declarations.len() {
        if state[root] == 2 {
            continue;
        }
        state[root] = 1;
        let mut stack = vec![(root, 0)];
        while let Some(&mut (index, ref mut next)) = stack.last_mut() {
            if let Some(&(target, span)) = edges[index].get(*next) {
                *next += 1;
                match state[target] {
                    0 => {
                        state[target] = 1;
                        stack.push((target, 0));
                    }
                    1 => {
                        let start = stack.iter().position(|&(node, _)| node == target).unwrap();
                        let cycle = &stack[start..];
                        let mut path: Vec<_> = cycle
                            .iter()
                            .map(|&(node, _)| declarations[node].name())
                            .collect();
                        path.push(declarations[target].name());
                        let (code, message) =
                            if cycle.iter().any(|&(node, _)| {
                                matches!(declarations[node], Declaration::Enum(_))
                            }) {
                                (ErrorCode::RecursiveType, "Infinite inline enum layout")
                            } else if cycle.iter().any(|&(node, _)| {
                                matches!(declarations[node], Declaration::Record(_))
                            }) {
                                (ErrorCode::RecursiveRecord, "Infinite inline record layout")
                            } else {
                                (ErrorCode::CyclicTypeAlias, "Cyclic type alias")
                            };
                        return Err(error(
                            code,
                            span,
                            format!("{message}: {}", path.join(" -> ")),
                        ));
                    }
                    _ => {}
                }
                continue;
            }
            let declaration = declarations[index];
            let ty = match declaration {
                Declaration::Record(record) => {
                    let local = registry.with_parameters(&record.type_params)?;
                    let mut fields = Vec::new();
                    let mut scalars = Vec::new();
                    let mut depth = 1;
                    for field in &record.fields {
                        let ty = member_type(&local, &field.ty, &canonical, field.span, true)?;
                        if record.repr_c() && !crate::core::ffi::is_c_value(&ty) {
                            return Err(crate::core::ffi::error(field.span,
                                "#repr(C) fields require fixed-width integers or nested #repr(C) records"));
                        }
                        let offset = scalars.len();
                        extend_layout(&mut scalars, &mut depth, &ty, field.span, "Record")?;
                        fields.push(RecordField {
                            private: record.private_fields.contains(&field.name),
                            name: field.name.clone(),
                            ty,
                            offset,
                        });
                    }
                    let owner = record_owner(&fields, record.span)?;
                    TypeDefinition::Type(Type::Record(Arc::new(RecordType {
                        owner,
                        indirect: false,
                        repr_c: record.repr_c(),
                        id: record.id,
                        name: record.name.clone(),
                        parameters: record.type_params.clone(),
                        arguments: record
                            .type_params
                            .iter()
                            .cloned()
                            .map(Type::Parameter)
                            .collect(),
                        template: None,
                        fields,
                        scalars,
                        depth,
                    })))
                }
                Declaration::Enum(enumeration) => {
                    let local = registry.with_parameters(&enumeration.type_params)?;
                    // One Int32 tag followed by disjoint, typed payload regions.
                    // Constructors initialize every lane; inactive regions contain
                    // zero/false. No uninitialized or type-punned values can cross
                    // a copy, call or suspension. Union packing is a future pass.
                    let mut scalars = vec![Type::Int32];
                    let mut variants = Vec::new();
                    let mut depth = 1;
                    for variant in &enumeration.variants {
                        let offset = scalars.len();
                        let payload = variant
                            .payload
                            .as_ref()
                            .map(|ty| member_type(&local, ty, &canonical, variant.span, false))
                            .transpose()?;
                        if let Some(ty) = &payload {
                            extend_layout(&mut scalars, &mut depth, ty, variant.span, "Enum")?;
                        }
                        variants.push(EnumVariant {
                            name: variant.name.clone(),
                            payload,
                            offset,
                        });
                    }
                    TypeDefinition::Type(Type::Enum(Arc::new(EnumType {
                        indirect: false,
                        id: enumeration.id,
                        parameters: enumeration.type_params.clone(),
                        arguments: enumeration
                            .type_params
                            .iter()
                            .cloned()
                            .map(Type::Parameter)
                            .collect(),
                        template: None,
                        name: enumeration.name.clone(),
                        variants,
                        scalars,
                        depth,
                    })))
                }
                Declaration::Alias(alias) => {
                    aliases_to_refresh.push(alias);
                    alias_type(registry, alias, &canonical)?
                }
            };
            if let Some(scope) = &module.scope {
                for (name, binding) in &scope.types {
                    if binding.declaration == declaration.id() && binding.declaration.is_some() {
                        canonical.insert(name.as_str(), ty.clone());
                    }
                }
            }
            canonical.insert(declaration.name(), ty);
            state[index] = 2;
            stack.pop();
        }
    }
    // References deliberately break inline layout dependencies. Now that all
    // owners are known, refresh aliases in dependency order so a forward view
    // cannot retain a shallow Copy identity inside storage or a signature.
    for alias in aliases_to_refresh {
        let ty = alias_type(registry, alias, &canonical)?;
        canonical.insert(alias.name.as_str(), ty);
    }
    Ok(canonical)
}

fn alias_type(
    registry: &TypeRegistry,
    alias: &TypeAlias,
    canonical: &HashMap<&str, TypeDefinition>,
) -> Result<TypeDefinition, Error> {
    let local = registry.with_parameters(&alias.type_params)?;
    let mut target = alias.target.clone();
    resolve_type(&mut target, canonical, &local).map_err(|e| e.with_span(alias.target_span))?;
    local
        .validate(&target)
        .map_err(|e| e.with_span(alias.target_span))?;
    Ok(if alias.type_params.is_empty() {
        TypeDefinition::Type(target)
    } else {
        TypeDefinition::Alias(Arc::new(AliasTemplate {
            name: alias.name.clone(),
            parameters: alias.type_params.clone(),
            target,
        }))
    })
}

pub(super) fn member_type(
    registry: &TypeRegistry,
    ty: &Type,
    canonical: &HashMap<&str, TypeDefinition>,
    span: Span,
    owning: bool,
) -> Result<Type, Error> {
    let mut ty = ty.clone();
    resolve_type(&mut ty, canonical, registry).map_err(|e| e.with_span(span))?;
    registry.validate(&ty).map_err(|e| e.with_span(span))?;
    if !ty.is_copy_storage_value() && !(owning && ty.is_owner()) {
        let requirement = if owning {
            "Record fields require a Copy value or a single owning field"
        } else {
            "Enum payloads require a Copy value"
        };
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            format!("{requirement}; buffer views cannot be stored, got {ty}"),
        ));
    }
    Ok(ty)
}

/// The first owning-record slice has exactly one owning field. Its value and
/// borrowed representation are the underlying stable buffer capability.
pub(super) fn record_owner(fields: &[RecordField], span: Span) -> Result<Option<Arc<Type>>, Error> {
    if !fields.iter().any(|f| f.ty.is_owner()) {
        return Ok(None);
    }
    if fields.len() != 1 {
        return Err(error(ErrorCode::TypeMismatch, span,
            "Owning records currently require exactly one owning field; additional fields and partial moves are not supported"));
    }
    Ok(Some(Arc::new(
        fields[0].ty.buffer_element().unwrap().clone(),
    )))
}

pub(super) fn extend_layout(
    scalars: &mut Vec<Type>,
    depth: &mut usize,
    ty: &Type,
    span: Span,
    kind: &str,
) -> Result<(), Error> {
    *depth = (*depth).max(ty.layout_depth() + 1);
    if *depth > 256 {
        return Err(error(
            ErrorCode::ResourceLimit,
            span,
            format!("{kind} nesting exceeds 256 levels"),
        ));
    }
    if scalars
        .len()
        .checked_add(ty.scalar_types().len())
        .is_none_or(|size| size > 65_536)
    {
        return Err(error(
            ErrorCode::ResourceLimit,
            span,
            format!("{kind} layout exceeds 65536 scalar slots"),
        ));
    }
    scalars.extend_from_slice(ty.scalar_types());
    Ok(())
}

fn error(code: ErrorCode, span: Span, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message).with_span(span)
}

pub(super) fn tuple_type(elements: Vec<Type>, registry: &TypeRegistry) -> Result<Type, Error> {
    let span = Span::empty(0);
    if elements.is_empty() {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            "A tuple must contain at least one element; use Unit for no value",
        ));
    }
    let mut scalars = Vec::new();
    let mut depth = 1;
    for ty in &elements {
        registry.validate(ty)?;
        if !ty.is_copy_storage_value() {
            return Err(error(
                ErrorCode::TypeMismatch,
                span,
                format!(
                    "Tuple element must be an integer, Bool, Unit or a copyable aggregate, got {ty}"
                ),
            ));
        }
        extend_layout(&mut scalars, &mut depth, ty, span, "Tuple")?;
    }
    Ok(Type::Tuple(Arc::new(TupleType {
        elements,
        scalars,
        depth,
    })))
}
