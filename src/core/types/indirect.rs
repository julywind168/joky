//! Finite nominal identities behind references. Definitions stay in the module's
//! nominal table, so recursive object types do not create reference-count cycles.
use super::*;
use crate::parser::ast::{EnumType, RecordType};
use std::sync::Arc;

pub(super) fn shallow(ty: &Type, arguments: Vec<Type>) -> Type {
    match ty {
        Type::Record(r) => Type::Record(Arc::new(RecordType {
            indirect: true,
            repr_c: r.repr_c,
            id: r.id,
            name: r.name.clone(),
            parameters: r.parameters.clone(),
            arguments,
            template: None,
            fields: Vec::new(),
            scalars: Vec::new(),
            depth: 1,
        })),
        Type::Enum(e) => Type::Enum(Arc::new(EnumType {
            indirect: true,
            id: e.id,
            name: e.name.clone(),
            parameters: e.parameters.clone(),
            arguments,
            template: None,
            variants: Vec::new(),
            scalars: Vec::new(),
            depth: 1,
        })),
        _ => ty.clone(),
    }
}

pub(super) fn resolve_target(
    ty: &Type,
    canonical: &HashMap<&str, TypeDefinition>,
    registry: &TypeRegistry,
    depth: usize,
) -> Result<Type, Error> {
    resolve(ty, canonical, registry, depth, &mut 65_536)
}

fn resolve(
    ty: &Type,
    canonical: &HashMap<&str, TypeDefinition>,
    registry: &TypeRegistry,
    depth: usize,
    budget: &mut usize,
) -> Result<Type, Error> {
    let bad = |message: &str| {
        Error::new_with_code(ErrorPhase::TypeCheck, ErrorCode::TypeMismatch, message)
    };
    if depth > 256 || *budget == 0 {
        return Err(bad(
            "Reference target or alias expansion exceeds 256 levels",
        ));
    }
    *budget -= 1;
    let (name, supplied) = match ty {
        Type::Named(name) => (name, None),
        Type::Applied(a) => (&a.name, Some(a.arguments.clone())),
        Type::Record(r) => return Ok(shallow(ty, r.arguments.clone())),
        Type::Enum(e) => return Ok(shallow(ty, e.arguments.clone())),
        Type::Tuple(t) => {
            return tuple_type(
                t.elements
                    .iter()
                    .map(|element| resolve(element, canonical, registry, depth + 1, budget))
                    .collect::<Result<_, _>>()?,
                registry,
            )
        }
        Type::Reference(r) => {
            let target = resolve(&r.target, canonical, registry, depth + 1, budget)?;
            let result = Type::reference(target, r.mutable);
            registry.validate(&result)?;
            return Ok(result);
        }
        Type::Buffer(element) | Type::BufferElement(element) | Type::BufferSlice(element) => {
            let element = resolve(element, canonical, registry, depth + 1, budget)?;
            // The view itself is indirect, but its elements are stored inline.
            // Recover nominal layouts by declaration identity after resolving
            // aliases, including consumer-defined generic arguments.
            let nominals = registry
                .forwards
                .borrow()
                .values()
                .chain(canonical.values())
                .filter_map(|definition| match definition {
                    TypeDefinition::Type(ty) => {
                        ty.nominal().map(|(_, id)| (id.to_string(), ty.clone()))
                    }
                    TypeDefinition::Alias(_) => None,
                })
                .collect();
            let element = materialize(&element, &nominals, registry)?;
            let result = ty.with_buffer_element(element);
            registry.validate(&result)?;
            return Ok(result);
        }
        _ => return Ok(ty.clone()),
    };
    let definition = canonical
        .get(name.as_str())
        .cloned()
        .or_else(|| registry.forwards.borrow().get(name).cloned());
    let Some(definition) = definition else {
        return Err(bad(&format!("Unknown reference target {name}")));
    };
    let mut arguments = supplied.clone().unwrap_or_default();
    for argument in &mut arguments {
        *argument = resolve(argument, canonical, registry, depth + 1, budget)?;
        registry.validate(argument)?;
        if !argument.is_copy_storage_value() {
            return Err(bad("Generic reference target arguments must be Copy types"));
        }
    }
    match definition {
        TypeDefinition::Alias(alias) => {
            if alias.parameters.len() != arguments.len() {
                return Err(bad(
                    "Reference target alias requires matching type arguments",
                ));
            }
            let substitutions = alias
                .parameters
                .iter()
                .zip(arguments)
                .map(|(p, t)| (p.id, t))
                .collect();
            let target = substitute_syntax(&alias.target, &substitutions);
            resolve(&target, canonical, registry, depth + 1, budget)
        }
        TypeDefinition::Type(target) => {
            let applied = match &target {
                Type::Record(r) if r.template.is_some() => Some(&r.arguments),
                Type::Enum(e) if e.template.is_some() => Some(&e.arguments),
                _ => None,
            };
            if let Some(arguments) = applied {
                if supplied.is_some() {
                    return Err(bad("A specialized alias does not take type arguments"));
                }
                return Ok(shallow(&target, arguments.clone()));
            }
            let parameters = match &target {
                Type::Record(r) => Some(&r.parameters),
                Type::Enum(e) => Some(&e.parameters),
                _ => None,
            };
            if let Some(parameters) = parameters {
                if parameters.len() != arguments.len() {
                    return Err(bad("Reference target requires matching type arguments"));
                }
                Ok(shallow(&target, arguments))
            } else if supplied.is_some() {
                Err(bad("Reference target does not take type arguments"))
            } else {
                resolve(&target, canonical, registry, depth + 1, budget)
            }
        }
    }
}

/// Materialize a pointee only when its fields or object layout are needed.
/// Looking up by declaration identity preserves private and imported nominals.
pub(crate) fn materialize(
    ty: &Type,
    nominals: &HashMap<String, Type>,
    registry: &TypeRegistry,
) -> Result<Type, Error> {
    #[derive(Default)]
    struct Cache {
        records: HashMap<*const RecordType, Type>,
        enums: HashMap<*const EnumType, Type>,
        tuples: HashMap<*const crate::parser::ast::TupleType, Type>,
    }
    fn visit(
        ty: &Type,
        nominals: &HashMap<String, Type>,
        registry: &TypeRegistry,
        cache: &mut Cache,
    ) -> Result<Type, Error> {
        // Inferred value types can be shared zero-sized DAGs. Expand each node
        // once, just as layout calculation and generic substitution do.
        let cached = match ty {
            Type::Record(r) => cache.records.get(&Arc::as_ptr(r)),
            Type::Enum(e) => cache.enums.get(&Arc::as_ptr(e)),
            Type::Tuple(t) => cache.tuples.get(&Arc::as_ptr(t)),
            _ => None,
        };
        if let Some(result) = cached {
            return Ok(result.clone());
        }
        let result = match ty {
            Type::Record(r) => {
                let target = nominals
                    .values()
                    .find(|t| t.nominal().is_some_and(|(_, id)| id == r.id))
                    .ok_or_else(|| {
                        Error::new_with_code(
                            ErrorPhase::TypeCheck,
                            ErrorCode::UnknownType,
                            "Reference target definition is not available in this module",
                        )
                    })?;
                let Type::Record(template) = target else {
                    unreachable!()
                };
                if template.parameters.is_empty() {
                    Ok(target.clone())
                } else {
                    registry.instantiate_record(
                        template,
                        r.arguments
                            .iter()
                            .map(|t| visit(t, nominals, registry, cache))
                            .collect::<Result<_, _>>()?,
                    )
                }
            }
            Type::Enum(e) => {
                let target = nominals
                    .values()
                    .find(|t| t.nominal().is_some_and(|(_, id)| id == e.id))
                    .ok_or_else(|| {
                        Error::new_with_code(
                            ErrorPhase::TypeCheck,
                            ErrorCode::UnknownType,
                            "Reference target definition is not available in this module",
                        )
                    })?;
                let Type::Enum(template) = target else {
                    unreachable!()
                };
                if template.parameters.is_empty() {
                    Ok(target.clone())
                } else {
                    registry.instantiate_enum(
                        template,
                        e.arguments
                            .iter()
                            .map(|t| visit(t, nominals, registry, cache))
                            .collect::<Result<_, _>>()?,
                    )
                }
            }
            Type::Tuple(t) => tuple_type(
                t.elements
                    .iter()
                    .map(|t| visit(t, nominals, registry, cache))
                    .collect::<Result<_, _>>()?,
                registry,
            ),
            _ => Ok(ty.clone()),
        }?;

        match ty {
            Type::Record(r) => {
                cache.records.insert(Arc::as_ptr(r), result.clone());
            }
            Type::Enum(e) => {
                cache.enums.insert(Arc::as_ptr(e), result.clone());
            }
            Type::Tuple(t) => {
                cache.tuples.insert(Arc::as_ptr(t), result.clone());
            }
            _ => {}
        }
        Ok(result)
    }
    visit(ty, nominals, registry, &mut Cache::default())
}

// Alias targets can still contain unresolved names. Substitute their syntax
// before resolving, without prematurely asking for an inline value layout.
fn substitute_syntax(ty: &Type, arguments: &HashMap<u64, Type>) -> Type {
    match ty {
        Type::Parameter(p) => arguments.get(&p.id).cloned().unwrap_or_else(|| ty.clone()),
        Type::Buffer(element) | Type::BufferElement(element) | Type::BufferSlice(element) => {
            ty.with_buffer_element(substitute_syntax(element, arguments))
        }
        Type::Reference(r) => Type::reference(substitute_syntax(&r.target, arguments), r.mutable),
        Type::Tuple(t) => Type::tuple(
            t.elements
                .iter()
                .map(|t| substitute_syntax(t, arguments))
                .collect(),
        ),
        Type::Applied(a) => Type::Applied(Arc::new(crate::parser::ast::TypeApplication {
            name: a.name.clone(),
            arguments: a
                .arguments
                .iter()
                .map(|t| substitute_syntax(t, arguments))
                .collect(),
        })),
        Type::Record(r) => shallow(
            ty,
            r.arguments
                .iter()
                .map(|t| substitute_syntax(t, arguments))
                .collect(),
        ),
        Type::Enum(e) => shallow(
            ty,
            e.arguments
                .iter()
                .map(|t| substitute_syntax(t, arguments))
                .collect(),
        ),
        _ => ty.clone(),
    }
}
