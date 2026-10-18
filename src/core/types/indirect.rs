//! Finite nominal identities behind references. Definitions stay in the module's
//! nominal table, so recursive object types do not create reference-count cycles.
use super::*;
use crate::parser::ast::{EnumType, RecordType};
use std::sync::Arc;

pub(super) fn shallow(ty: &Type, arguments: Vec<crate::parser::ast::GenericArgument>) -> Type {
    match ty {
        Type::Record(r) => Type::Record(Arc::new(RecordType {
            closure: None,
            owners: if r.arguments == arguments {
                r.owners.clone()
            } else {
                r.owners
                    .iter()
                    .map(|owner| {
                        Arc::new(substitute_syntax(
                            owner,
                            &r.parameters
                                .iter()
                                .zip(&arguments)
                                .map(|(p, t)| (p.id, t.clone()))
                                .collect(),
                        ))
                    })
                    .collect()
            },
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
            owners: if e.arguments == arguments {
                e.owners.clone()
            } else {
                e.owners
                    .iter()
                    .map(|owner| {
                        Arc::new(substitute_syntax(
                            owner,
                            &e.parameters
                                .iter()
                                .zip(&arguments)
                                .map(|(p, t)| (p.id, t.clone()))
                                .collect(),
                        ))
                    })
                    .collect()
            },
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

/// Resolve identities behind owning storage without expanding element layouts.
pub(super) fn resolve_element(
    ty: &Type,
    canonical: &HashMap<&str, TypeDefinition>,
    registry: &TypeRegistry,
    depth: usize,
) -> Result<Type, Error> {
    let mut element = resolve(ty, canonical, registry, depth, &mut 65_536)?;
    // Storage retains identity only; the checked body is recovered separately
    // when an element layout or ownership descriptor is required.
    match &mut element {
        Type::Record(r) if r.indirect => Arc::get_mut(r).unwrap().owners.clear(),
        Type::Enum(e) if e.indirect => Arc::get_mut(e).unwrap().owners.clear(),
        _ => {}
    }
    Ok(element)
}

pub(super) fn resolve_target(
    ty: &Type,
    canonical: &HashMap<&str, TypeDefinition>,
    registry: &TypeRegistry,
    depth: usize,
) -> Result<Type, Error> {
    let target = resolve(ty, canonical, registry, depth, &mut 65_536)?;
    // Owner views need their element layout and provenance even through forward
    // names and generic arguments. Ordinary graph references stay shallow.
    // Tuple targets can hide a specialized nominal owner in any element.
    // Include nominal arguments, but classify the materialized value: phantom
    // owner arguments do not themselves make the reference an owner view.
    let owner_argument =
        super::TypeWalk::new(&target, super::TypeEdges::Arguments).any(Type::is_owner);
    let mut pending = vec![&target];
    let mut seen = std::collections::HashSet::new();
    let mut potential_owner = false;
    while let Some(node) = pending.pop() {
        if let Type::Tuple(tuple) = node {
            if seen.insert(Arc::as_ptr(tuple)) {
                pending.extend(&tuple.elements);
            }
            continue;
        }
        let Some((_, id)) = node.nominal() else {
            continue;
        };
        // Arguments influence specialization, but only stored tuple elements
        // can make this target an owner. Do not expand phantom arguments.
        potential_owner |= canonical
            .values()
            .chain(registry.forwards.borrow().values())
            .any(|definition| {
                let TypeDefinition::Type(ty) = definition else {
                    return false;
                };
                let concrete = matches!(ty, Type::Record(r) if !r.indirect)
                    || matches!(ty, Type::Enum(e) if !e.indirect);
                ty.nominal().is_some_and(|(_, actual)| actual == id)
                    && concrete
                    && (ty.is_owner()
                        || owner_argument
                            && ty
                                .scalar_types()
                                .iter()
                                .any(|ty| matches!(ty, Type::Parameter(_))))
            });
    }
    if !potential_owner {
        return Ok(target);
    }
    let definitions: HashMap<_, _> = registry
        .forwards
        .borrow()
        .values()
        .chain(canonical.values())
        .filter_map(|definition| match definition {
            TypeDefinition::Type(ty) => ty.nominal().map(|(_, id)| (id.to_string(), ty.clone())),
            TypeDefinition::Alias(_) => None,
        })
        .collect();
    let instance = materialize(&target, &definitions, registry)?;
    Ok(if instance.is_owner() {
        instance
    } else {
        target
    })
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
    if let Type::DynTrait(d) = ty {
        return registry
            .normalize_dyn_trait(d)
            .map(|d| Type::DynTrait(Arc::new(d)));
    }
    let (name, supplied) = match ty {
        Type::Named(name) if registry.resources.contains(name) => {
            return registry.resource_type(name);
        }
        Type::Named(name) => (name, None),
        Type::Applied(a) => (&a.name, Some(a.arguments.clone())),
        Type::Record(r) => return Ok(shallow(ty, r.arguments.clone())),
        Type::Enum(e) => return Ok(shallow(ty, e.arguments.clone())),
        Type::FixedArray(_) | Type::Function(_) | Type::Projection(_) => {
            // Crossing between reference and value normalization has larger
            // stack frames than a plain signature walk. Bound that path too.
            if depth > 32 {
                return Err(bad("Function reference type expansion exceeds 32 levels"));
            }
            let mut ty = ty.clone();
            super::resolve_type_at_depth(&mut ty, canonical, registry, depth + 1)?;
            return Ok(ty);
        }
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
    for argument in arguments.iter_mut().filter_map(|a| a.as_type_mut()) {
        *argument = resolve(argument, canonical, registry, depth + 1, budget)?;
        registry.validate(argument)?;
        if argument.contains_closures() || !argument.is_copy_storage_value() && !argument.is_owner()
        {
            return Err(bad(
                "Generic reference target arguments must be Copy types or owners",
            ));
        }
    }
    match definition {
        TypeDefinition::Alias(alias) => {
            if alias.parameters.len() != arguments.len() {
                return Err(bad(
                    "Reference target alias requires matching type arguments",
                ));
            }
            for (p, a) in alias.parameters.iter().zip(&arguments) {
                registry.validate_argument(p, a)?;
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
                for (p, a) in parameters.iter().zip(&arguments) {
                    registry.validate_argument(p, a)?;
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
        // Concrete arguments can originate in a consumer module while the
        // template is instantiated in the library's scope. Keep their checked
        // layouts instead of requiring them in the library's nominal table.
        if matches!(ty, Type::Record(r) if !r.indirect)
            || matches!(ty, Type::Enum(e) if !e.indirect)
        {
            return Ok(ty.clone());
        }
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
                            .map(|a| a.try_map_type(|t| visit(t, nominals, registry, cache)))
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
                            .map(|a| a.try_map_type(|t| visit(t, nominals, registry, cache)))
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

/// Validate the finite element graph separately from inline object layout.
/// Backedges retain nominal identity and cannot form strong metadata cycles.
pub(crate) fn validate_elements(
    root: &Type,
    nominals: &HashMap<String, Type>,
    registry: &TypeRegistry,
) -> Result<(), Error> {
    let mut pending = vec![(root.clone(), false)];
    let mut seen = Vec::new();
    while let Some((ty, reference_free)) = pending.pop() {
        if seen
            .iter()
            .any(|entry| entry == &(ty.clone(), reference_free))
        {
            continue;
        }
        if seen.len() >= 65_536 {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::ResourceLimit,
                "Owning element graph exceeds 65536 types",
            ));
        }
        seen.push((ty.clone(), reference_free));
        match &ty {
            Type::Buffer(element) | Type::BufferElement(element) | Type::BufferSlice(element) => {
                let element = materialize(element, nominals, registry)?;
                if !element.is_buffer_element() || reference_free && element.contains_references() {
                    return Err(Error::new_with_code(
                        ErrorPhase::TypeCheck,
                        ErrorCode::TypeMismatch,
                        "Buffer elements require supported Copy data or reference-free owners",
                    ));
                }
                let reference_free = reference_free || element.is_owner();
                pending.push((element, reference_free));
            }
            Type::Record(record) => pending.extend(
                record
                    .fields
                    .iter()
                    .map(|field| (field.ty.clone(), reference_free)),
            ),
            Type::Enum(enumeration) => pending.extend(
                enumeration
                    .variants
                    .iter()
                    .filter_map(|variant| variant.payload.as_ref())
                    .map(|ty| (ty.clone(), reference_free)),
            ),
            Type::Tuple(tuple) => {
                pending.extend(tuple.elements.iter().map(|ty| (ty.clone(), reference_free)))
            }
            Type::Reference(_) if reference_free => {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::TypeMismatch,
                    "Owning aggregate elements must be reference-free",
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

// Alias targets can still contain unresolved names. Substitute their syntax
// before resolving, without prematurely asking for an inline value layout.
pub(crate) fn substitute_syntax(ty: &Type, arguments: &crate::core::types::Substitutions) -> Type {
    match ty {
        Type::FixedArray(a) => Type::FixedArray(Arc::new(crate::parser::ast::FixedArrayType {
            element: substitute_syntax(&a.element, arguments),
            length: arguments.constant(&a.length),
        })),
        Type::Function(signature) => Type::function_with_effect(
            signature
                .signature
                .params
                .iter()
                .map(|t| substitute_syntax(t, arguments))
                .collect(),
            substitute_syntax(&signature.signature.ret, arguments),
            signature.suspends,
        ),
        Type::DynTrait(d) => {
            let mut d = d.as_ref().clone();
            for e in &mut d.equalities {
                e.ty = substitute_syntax(&e.ty, arguments);
            }
            d.interface = None;
            Type::DynTrait(Arc::new(d))
        }
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
                .map(|a| substitute_argument_syntax(a, arguments))
                .collect(),
        })),
        Type::Record(r) => shallow(
            ty,
            r.arguments
                .iter()
                .map(|a| substitute_argument_syntax(a, arguments))
                .collect(),
        ),
        Type::Enum(e) => shallow(
            ty,
            e.arguments
                .iter()
                .map(|a| substitute_argument_syntax(a, arguments))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

pub(crate) fn substitute_argument_syntax(
    a: &crate::parser::ast::GenericArgument,
    s: &crate::core::types::Substitutions,
) -> crate::parser::ast::GenericArgument {
    use crate::parser::ast::GenericArgument;
    match a {
        GenericArgument::Type(t) => GenericArgument::Type(substitute_syntax(t, s)),
        GenericArgument::Const(c) => GenericArgument::Const(s.constant(c)),
    }
}
