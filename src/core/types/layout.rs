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
) -> Result<HashMap<&'a str, Type>, Error> {
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
            || matches!(
                name,
                "Int32" | "Int64" | "UInt32" | "UInt64" | "Bool" | "Unit" | "String"
            )
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
    let mut edges = vec![Vec::new(); declarations.len()];
    for (index, &declaration) in declarations.iter().enumerate() {
        let mut pending = declaration.dependencies();
        pending.reverse();
        let mut seen_tuples = HashSet::new();
        while let Some((ty, span)) = pending.pop() {
            if let Type::Tuple(tuple) = ty {
                if !seen_tuples.insert(Arc::as_ptr(tuple)) {
                    continue;
                }
                pending.extend(tuple.elements.iter().rev().map(|ty| (ty, span)));
                continue;
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
                    let mut fields = Vec::new();
                    let mut scalars = Vec::new();
                    let mut depth = 1;
                    for field in &record.fields {
                        let ty = member_type(registry, &field.ty, &canonical, field.span)?;
                        let offset = scalars.len();
                        extend_layout(&mut scalars, &mut depth, &ty, field.span, "Record")?;
                        fields.push(RecordField {
                            name: field.name.clone(),
                            ty,
                            offset,
                        });
                    }
                    Type::Record(Arc::new(RecordType {
                        id: record.id,
                        name: record.name.clone(),
                        fields,
                        scalars,
                        depth,
                    }))
                }
                Declaration::Enum(enumeration) => {
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
                            .map(|ty| member_type(registry, ty, &canonical, variant.span))
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
                    Type::Enum(Arc::new(EnumType {
                        id: enumeration.id,
                        name: enumeration.name.clone(),
                        variants,
                        scalars,
                        depth,
                    }))
                }
                Declaration::Alias(alias) => {
                    let mut target = alias.target.clone();
                    resolve_type(&mut target, &canonical, registry)
                        .map_err(|e| e.with_span(alias.target_span))?;
                    registry
                        .validate(&target)
                        .map_err(|e| e.with_span(alias.target_span))?;
                    target
                }
            };
            canonical.insert(declaration.name(), ty);
            state[index] = 2;
            stack.pop();
        }
    }
    Ok(canonical)
}

fn member_type(
    registry: &TypeRegistry,
    ty: &Type,
    canonical: &HashMap<&str, Type>,
    span: Span,
) -> Result<Type, Error> {
    let mut ty = ty.clone();
    resolve_type(&mut ty, canonical, registry).map_err(|e| e.with_span(span))?;
    registry.validate(&ty).map_err(|e| e.with_span(span))?;
    if !ty.is_copy_scalar() && !ty.is_aggregate() {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            format!(
                "Field or payload must be an integer, Bool, Unit, a copyable record, enum or tuple, got {ty}"
            ),
        ));
    }
    Ok(ty)
}

fn extend_layout(
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
        if !ty.is_copy_scalar() && !ty.is_aggregate() {
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
