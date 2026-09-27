//! Resolve finite copyable record layouts without recursing on source graphs.
use super::*;
use crate::{
    parser::ast::{RecordField, RecordType},
    Span,
};
use std::{collections::HashSet, sync::Arc};

pub(super) fn resolve<'a>(
    registry: &TypeRegistry,
    module: &'a Module,
) -> Result<HashMap<&'a str, Type>, Error> {
    // Aliases and records share one namespace. Edges through aliases participate
    // in layout cycle detection, even when the declaration is never used.
    let mut names = HashMap::new();
    for (index, record) in module.records.iter().enumerate() {
        check_name(registry, &mut names, &record.name, index, record.name_span)?;
        let mut fields = HashSet::new();
        for field in &record.fields {
            if !fields.insert(&field.name) {
                return Err(error(
                    ErrorCode::DuplicateDefinition,
                    field.span,
                    format!("Duplicate field `{}` in `{}`", field.name, record.name),
                ));
            }
        }
    }
    for (index, alias) in module.type_aliases.iter().enumerate() {
        check_name(
            registry,
            &mut names,
            &alias.name,
            module.records.len() + index,
            alias.name_span,
        )?;
    }
    let count = names.len();
    let mut edges = vec![Vec::new(); count];
    for (index, record) in module.records.iter().enumerate() {
        for field in &record.fields {
            if let Some(target) = dependency(&field.ty, &names, module, field.span)? {
                edges[index].push((target, field.span));
            }
        }
    }
    for (index, alias) in module.type_aliases.iter().enumerate() {
        if let Some(target) = dependency(&alias.target, &names, module, alias.target_span)? {
            edges[module.records.len() + index].push((target, alias.target_span));
        }
    }
    let mut state = vec![0u8; count];
    let mut canonical = HashMap::new();
    for root in 0..count {
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
                        let record_cycle =
                            cycle.iter().any(|&(node, _)| node < module.records.len());
                        let name = |node: usize| {
                            if node < module.records.len() {
                                module.records[node].name.as_str()
                            } else {
                                module.type_aliases[node - module.records.len()]
                                    .name
                                    .as_str()
                            }
                        };
                        let mut path: Vec<_> = cycle.iter().map(|&(node, _)| name(node)).collect();
                        path.push(name(target));
                        let (code, message) = if record_cycle {
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
            if index < module.records.len() {
                let record = &module.records[index];
                let mut fields = Vec::new();
                let mut scalars = Vec::new();
                let mut depth = 1;
                for field in &record.fields {
                    let mut ty = field.ty.clone();
                    resolve_type(&mut ty, &canonical);
                    registry
                        .validate(&ty)
                        .map_err(|e| e.with_span(field.span))?;
                    if !matches!(ty, Type::Int32 | Type::Bool | Type::Unit | Type::Record(_)) {
                        return Err(error(ErrorCode::TypeMismatch, field.span,
                            format!("Record field `{}` must be Int32, Bool, Unit or a copyable record, got {ty}", field.name)));
                    }
                    if let Type::Record(nested) = &ty {
                        depth = depth.max(nested.depth + 1);
                        if depth > 256 {
                            return Err(error(
                                ErrorCode::ResourceLimit,
                                field.span,
                                "Record nesting exceeds 256 levels",
                            ));
                        }
                    }
                    let offset = scalars.len();
                    // Bound expanded layouts before allocating, including products
                    // of nested records. This also bounds all native slot offsets.
                    if offset
                        .checked_add(ty.scalar_types().len())
                        .is_none_or(|size| size > 65_536)
                    {
                        return Err(error(
                            ErrorCode::ResourceLimit,
                            field.span,
                            "Record layout exceeds 65536 scalar slots",
                        ));
                    }
                    scalars.extend_from_slice(ty.scalar_types());
                    fields.push(RecordField {
                        name: field.name.clone(),
                        ty,
                        offset,
                    });
                }
                canonical.insert(
                    record.name.as_str(),
                    Type::Record(Arc::new(RecordType {
                        id: record.id,
                        name: record.name.clone(),
                        fields,
                        scalars,
                        depth,
                    })),
                );
            } else {
                let alias = &module.type_aliases[index - module.records.len()];
                let mut target = alias.target.clone();
                resolve_type(&mut target, &canonical);
                registry
                    .validate(&target)
                    .map_err(|e| e.with_span(alias.target_span))?;
                canonical.insert(alias.name.as_str(), target);
            }
            state[index] = 2;
            stack.pop();
        }
    }
    Ok(canonical)
}

fn dependency(
    ty: &Type,
    names: &HashMap<&str, usize>,
    module: &Module,
    span: Span,
) -> Result<Option<usize>, Error> {
    match ty {
        Type::Named(name) => Ok(names.get(name.as_str()).copied()),
        Type::Record(record) => {
            let index = names.get(record.name.as_str()).copied().filter(|&index| {
                module
                    .records
                    .get(index)
                    .is_some_and(|declaration| declaration.id == record.id)
            });
            index.map(Some).ok_or_else(|| {
                error(
                    ErrorCode::UnknownType,
                    span,
                    format!(
                        "Record `{}` does not belong to this source module",
                        record.name
                    ),
                )
            })
        }
        _ => Ok(None),
    }
}

fn check_name<'a>(
    registry: &TypeRegistry,
    names: &mut HashMap<&'a str, usize>,
    name: &'a str,
    index: usize,
    span: Span,
) -> Result<(), Error> {
    if registry.contains(name)
        || matches!(name, "Int32" | "Bool" | "Unit" | "String")
        || names.insert(name, index).is_some()
    {
        return Err(error(
            ErrorCode::DuplicateDefinition,
            span,
            format!("Type `{name}` is already defined"),
        ));
    }
    Ok(())
}

fn error(code: ErrorCode, span: Span, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message).with_span(span)
}
