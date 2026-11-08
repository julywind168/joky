//! Resolve finite nominal layouts and aliases without recursing on source graphs.
use super::*;
use crate::{
    parser::ast::{EnumDef, RecordDef, TypeAlias},
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
    fn used_parameters(self) -> HashSet<u64> {
        let mut used = HashSet::new();
        let mut pending: Vec<_> = self.dependencies().into_iter().map(|(ty, _)| ty).collect();
        while let Some(root) = pending.pop() {
            for ty in TypeWalk::new(root, TypeEdges::Fields) {
                match ty {
                    Type::Parameter(parameter) => {
                        used.insert(parameter.id);
                    }
                    Type::Applied(application) => {
                        pending.extend(application.arguments.iter().filter_map(|a| a.as_type()))
                    }
                    _ => {}
                }
            }
        }
        used
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
    let drop_trait = module
        .traits
        .iter()
        .find(|d| d.is_drop)
        .map(|d| d.self_parameter.id);
    let drop_targets: HashSet<_> = module
        .implementations
        .iter()
        .filter_map(|implementation| {
            let scope = implementation
                .scope
                .as_ref()
                .map(|s| s.imports.as_ref())
                .or(module.scope.as_deref())?;
            let binding = scope.traits.get(implementation.trait_name.as_ref()?)?;
            if Some(binding.id) != drop_trait {
                return None;
            }
            implementation
                .target
                .nominal()
                .map(|(_, id)| id)
                .or_else(|| scope.types.get(&implementation.target_name)?.declaration)
                .or_else(|| {
                    module
                        .records
                        .iter()
                        .find(|r| r.name == implementation.target_name)
                        .map(|r| r.id)
                })
        })
        .collect();
    let mut names = HashMap::new();
    for (index, &declaration) in declarations.iter().enumerate() {
        let name = declaration.name();
        if matches!(name, "Copy" | "Fn" | "FnMut" | "FnOnce")
            || registry.contains(name)
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
                custom_drop: drop_targets.contains(&d.id),
                array: d.array,
                list: d.list,
                map: d.map,
                set: d.set,
                closure: None,
                owners: Vec::new(),
                repr_c: d.repr_c(),
                indirect: true,
                id: d.id,
                name: d.name.clone(),
                parameters: d.type_params.clone(),
                arguments: d.type_params.iter().map(|p| p.argument()).collect(),
                template: None,
                fields: Vec::new(),
                scalars: Vec::new(),
                depth: 1,
            }))),
            Declaration::Enum(d) => TypeDefinition::Type(Type::Enum(Arc::new(EnumType {
                compact: false,
                owners: Vec::new(),
                indirect: true,
                id: d.id,
                name: d.name.clone(),
                parameters: d.type_params.clone(),
                arguments: d.type_params.iter().map(|p| p.argument()).collect(),
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
        let mut pending: Vec<_> = declaration
            .dependencies()
            .into_iter()
            .map(|(ty, span)| (ty.clone(), span))
            .collect();
        pending.reverse();
        let mut expanded = Vec::new();
        let mut expanded_aliases = Vec::new();
        let mut seen_tuples = HashSet::new();
        let mut seen_functions = HashSet::new();
        let mut seen_enums = HashSet::new();
        let mut seen_records = HashSet::new();
        // Pointer-based DAG deduplication must keep freshly normalized types
        // alive; later projections can otherwise reuse an earlier allocation.
        let mut normalized = Vec::new();
        while let Some((mut ty, span)) = pending.pop() {
            if matches!(&ty, Type::Projection(_))
                || matches!(&ty,Type::Named(name) if name.contains('.') && !names.contains_key(name.as_str()) && !module.scope.as_ref().is_some_and(|scope| scope.types.contains_key(name)))
            {
                let definitions = registry.forwards.borrow().clone();
                let canonical = definitions
                    .iter()
                    .map(|(name, definition)| (name.as_str(), definition.clone()))
                    .collect();
                resolve_type(&mut ty, &canonical, registry).map_err(|e| e.with_span(span))?;
                normalized.push(ty.clone());
            }
            // Imported nominal applications may already have resolved identity.
            // Discover their actual storage through the same declaration path.
            let application = match &ty {
                Type::Record(r) if !r.parameters.is_empty() => {
                    Some((r.name.clone(), r.arguments.clone()))
                }
                Type::Enum(e) if !e.parameters.is_empty() => {
                    Some((e.name.clone(), e.arguments.clone()))
                }
                _ => None,
            };
            if let Some((name, arguments)) = application {
                ty = Type::Applied(Arc::new(TypeApplication { name, arguments }));
            }
            let ty = &ty;
            if let Type::Applied(application) = ty {
                let TypeApplication { name, arguments } = application.as_ref();
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
                // Generic arguments occupy storage only where the declaration
                // actually stores them. Owning buffers end inline dependencies.
                if let Some(&target) = names.get(name.as_str()) {
                    if target == index {
                        continue;
                    }
                    if !expanded
                        .iter()
                        .any(|entry| entry == &(target, arguments.clone()))
                    {
                        if expanded.len() >= 256 {
                            return Err(error(
                                ErrorCode::ResourceLimit,
                                span,
                                "Inline dependency expansion exceeds 256 applications",
                            ));
                        }
                        expanded.push((target, arguments.clone()));
                        let parameters = match declarations[target] {
                            Declaration::Record(d) => &d.type_params,
                            Declaration::Enum(d) => &d.type_params,
                            Declaration::Alias(d) => &d.type_params,
                        };
                        // Erased arguments still need validation and cannot hide
                        // alias or phantom dependency cycles.
                        let used = declarations[target].used_parameters();
                        pending.extend(
                            parameters
                                .iter()
                                .zip(arguments)
                                .filter(|(parameter, _)| !used.contains(&parameter.id))
                                .filter_map(|(_, a)| a.as_type().map(|ty| (ty.clone(), span))),
                        );
                        let substitutions = parameters
                            .iter()
                            .zip(arguments)
                            .map(|(p, ty)| (p.id, ty.clone()))
                            .collect();
                        pending.extend(declarations[target].dependencies().into_iter().map(
                            |(ty, _)| (indirect::substitute_syntax(ty, &substitutions), span),
                        ));
                    }
                } else if let Some(TypeDefinition::Alias(alias)) = module
                    .scope
                    .as_ref()
                    .and_then(|scope| scope.types.get(name))
                    .and_then(|binding| binding.resolved.as_ref())
                {
                    // Imported aliases have no declaration node in this module.
                    // Follow their substituted storage, so FixedArray<T, N>
                    // orders T's layout while Array<T> still stops at Buffer.
                    let key = (name.clone(), arguments.clone());
                    if !expanded_aliases.contains(&key) {
                        if expanded_aliases.len() >= 256 {
                            return Err(error(
                                ErrorCode::ResourceLimit,
                                span,
                                "Inline dependency expansion exceeds 256 imported aliases",
                            ));
                        }
                        expanded_aliases.push(key);
                        let substitutions = alias
                            .parameters
                            .iter()
                            .zip(arguments)
                            .map(|(p, ty)| (p.id, ty.clone()))
                            .collect();
                        let used: HashSet<_> = TypeWalk::new(&alias.target, TypeEdges::Fields)
                            .filter_map(|ty| match ty {
                                Type::Parameter(p) => Some(p.id),
                                _ => None,
                            })
                            .collect();
                        pending.extend(
                            alias
                                .parameters
                                .iter()
                                .zip(arguments)
                                .filter(|(p, _)| !used.contains(&p.id))
                                .filter_map(|(_, a)| a.as_type().map(|ty| (ty.clone(), span))),
                        );
                        pending.push((
                            indirect::substitute_syntax(&alias.target, &substitutions),
                            span,
                        ));
                    }
                }
                continue;
            }
            if let Type::FixedArray(a) = ty {
                pending.push((a.element.clone(), span));
                continue;
            }
            if matches!(ty, Type::Reference(_) | Type::Buffer(_)) {
                continue; // A pointer breaks inline layout recursion.
            }
            if let Type::Function(signature) = ty {
                if seen_functions.insert(Arc::as_ptr(signature)) {
                    pending.extend(
                        signature
                            .signature
                            .params
                            .iter()
                            .map(|ty| (ty.clone(), span)),
                    );
                    pending.push((signature.signature.ret.clone(), span));
                }
                continue;
            }
            if let Type::Tuple(tuple) = ty {
                if !seen_tuples.insert(Arc::as_ptr(tuple)) {
                    continue;
                }
                pending.extend(tuple.elements.iter().rev().map(|ty| (ty.clone(), span)));
                continue;
            }
            if let Type::Enum(enumeration) = ty {
                if seen_enums.insert(Arc::as_ptr(enumeration)) {
                    pending.extend(
                        enumeration
                            .arguments
                            .iter()
                            .filter_map(|a| a.as_type())
                            .rev()
                            .map(|ty| (ty.clone(), span)),
                    );
                }
            }
            if let Type::Record(record) = ty {
                if seen_records.insert(Arc::as_ptr(record)) {
                    pending.extend(
                        record
                            .arguments
                            .iter()
                            .filter_map(|a| a.as_type())
                            .rev()
                            .map(|ty| (ty.clone(), span)),
                    );
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
    // Dependency discovery may specialize shallow nominal identities. Layouts
    // must be rebuilt after their topological dependencies become concrete.
    registry.fixed_arrays.borrow_mut().clear();
    registry.record_instances.borrow_mut().clear();
    registry.enum_instances.borrow_mut().clear();
    registry.alias_instances.borrow_mut().clear();
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
    let mut order = Vec::with_capacity(declarations.len());
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
            order.push(index);
            state[index] = 2;
            stack.pop();
        }
    }
    let wide = wide_enums(&declarations, &names, &edges, &order, &drop_targets);
    for index in order {
        let declaration = declarations[index];
        let ty = match declaration {
            Declaration::Record(record) => {
                let local = registry.with_parameters(&record.type_params)?;
                let mut fields = Vec::new();
                let mut scalars = Vec::new();
                let mut depth = 1;
                for field in &record.fields {
                    let ty = member_type(&local, &field.ty, &canonical, field.span)?;
                    if record.repr_c() && !crate::core::ffi::is_c_value(&ty) {
                        return Err(crate::core::ffi::error(field.span,
                                "#repr(C) fields require fixed-width numerics, nested #repr(C) records, or nonempty arrays of C-compatible elements"));
                    }
                    let offset = scalars.len();
                    extend_layout(&mut scalars, &mut depth, &ty, field.span, "Record")?;
                    fields.push(RecordField {
                        private: !record.public_fields.contains(&field.name),
                        name: field.name.clone(),
                        ty,
                        offset,
                    });
                }
                let owners = record_owners(&fields, record.span)?;
                TypeDefinition::Type(Type::Record(Arc::new(RecordType {
                    custom_drop: drop_targets.contains(&record.id),
                    array: record.array,
                    list: record.list,
                    map: record.map,
                    set: record.set,
                    closure: None,
                    owners,
                    indirect: false,
                    repr_c: record.repr_c(),
                    id: record.id,
                    name: record.name.clone(),
                    parameters: record.type_params.clone(),
                    arguments: record.type_params.iter().map(|p| p.argument()).collect(),
                    template: None,
                    fields,
                    scalars,
                    depth,
                })))
            }
            Declaration::Enum(enumeration) => {
                let local = registry.with_parameters(&enumeration.type_params)?;
                let compact = wide.contains(&index);
                let mut scalars = vec![Type::Int32];
                let mut variants = Vec::new();
                let mut depth = 1;
                for variant in &enumeration.variants {
                    let offset = scalars.len();
                    let payload = variant
                        .payload
                        .as_ref()
                        .map(|ty| member_type(&local, ty, &canonical, variant.span))
                        .transpose()?;
                    if let Some(ty) = &payload {
                        if compact {
                            depth = depth.max(ty.layout_depth() + 1);
                            if depth > 256 {
                                return Err(error(
                                    ErrorCode::ResourceLimit,
                                    variant.span,
                                    "Enum nesting exceeds 256 levels",
                                ));
                            }
                        } else {
                            extend_layout(&mut scalars, &mut depth, ty, variant.span, "Enum")?;
                        }
                    }
                    variants.push(EnumVariant {
                        name: variant.name.clone(),
                        payload,
                        offset,
                    });
                }
                TypeDefinition::Type(Type::Enum(Arc::new(EnumType {
                    compact,
                    owners: enum_owners(&variants, enumeration.span)?,
                    indirect: false,
                    id: enumeration.id,
                    parameters: enumeration.type_params.clone(),
                    arguments: enumeration
                        .type_params
                        .iter()
                        .map(|p| p.argument())
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
        if let TypeDefinition::Type(value) = &ty {
            let parameters = match declaration {
                Declaration::Record(d) => &d.type_params,
                Declaration::Enum(d) => &d.type_params,
                Declaration::Alias(d) => &d.type_params,
            };
            super::fixed_array::validate(value, &registry.with_parameters(parameters)?)
                .map_err(|e| e.with_span(declaration.span()))?;
        }
        if let Some(scope) = &module.scope {
            for (name, binding) in &scope.types {
                if binding.declaration == declaration.id() && binding.declaration.is_some() {
                    canonical.insert(name.as_str(), ty.clone());
                }
            }
        }
        registry
            .forwards
            .borrow_mut()
            .insert(declaration.name().to_owned(), ty.clone());
        canonical.insert(declaration.name(), ty);
    }
    // References deliberately break inline layout dependencies. Now that all
    // owners are known, refresh aliases in dependency order so a forward view
    // cannot retain a shallow Copy identity inside storage or a signature.
    // Re-specialize aliases and annotations against complete reference targets;
    // early layout discovery may have cached a forward, Copy-shaped view.
    registry.forwards.borrow_mut().extend(
        canonical
            .iter()
            .map(|(name, definition)| ((*name).to_owned(), definition.clone())),
    );
    registry.fixed_arrays.borrow_mut().clear();
    registry.record_instances.borrow_mut().clear();
    registry.enum_instances.borrow_mut().clear();
    registry.alias_instances.borrow_mut().clear();
    for alias in aliases_to_refresh {
        let ty = alias_type(registry, alias, &canonical)?;
        canonical.insert(alias.name.as_str(), ty);
    }
    let nominals = canonical
        .values()
        .filter_map(|definition| match definition {
            TypeDefinition::Type(ty) => ty.nominal().map(|(_, id)| (id.to_string(), ty.clone())),
            TypeDefinition::Alias(_) => None,
        })
        .collect();
    for definition in canonical.values() {
        if let TypeDefinition::Type(ty) = definition {
            let parameters = match ty {
                Type::Record(r) => r.parameters.as_slice(),
                Type::Enum(e) => e.parameters.as_slice(),
                _ => &[],
            };
            indirect::validate_elements(ty, &nominals, &registry.with_parameters(parameters)?)
                .map_err(|error| {
                    if let Some(declaration) = ty
                        .nominal()
                        .and_then(|(_, id)| declarations.iter().find(|d| d.id() == Some(id)))
                    {
                        error.with_span(declaration.span())
                    } else {
                        error
                    }
                })?;
        }
    }
    Ok(canonical)
}

/// Compute flat widths without allocating lanes. Only plain, concrete data
/// participates. Mark enum payload dependencies as well as the overflow root
/// so construction and extraction agree on the representation at every level.
fn wide_enums(
    declarations: &[Declaration<'_>],
    names: &HashMap<&str, usize>,
    edges: &[Vec<(usize, Span)>],
    order: &[usize],
    drops: &HashSet<u64>,
) -> HashSet<usize> {
    const OVERFLOW: usize = MAX_SCALAR_SLOTS + 1;
    fn width(
        ty: &Type,
        names: &HashMap<&str, usize>,
        widths: &[Option<usize>],
        tuples: &mut HashMap<*const TupleType, Option<usize>>,
        depth: usize,
    ) -> Option<usize> {
        if depth > 256 {
            return None;
        }
        match ty {
            Type::Unit => Some(0),
            ty if crate::core::instance::scalar(ty) => Some(1),
            Type::Tuple(t) => {
                let key = Arc::as_ptr(t);
                if let Some(cached) = tuples.get(&key) {
                    return *cached;
                }
                let result = t.elements.iter().try_fold(0, |n, t| {
                    Some(OVERFLOW.min(n + width(t, names, widths, tuples, depth + 1)?))
                });
                tuples.insert(key, result);
                result
            }
            Type::Named(n) => widths[*names.get(n.as_str())?],
            ty => widths[*names.get(ty.nominal()?.0)?],
        }
    }
    let mut tuples = HashMap::new();
    let mut widths = vec![None; declarations.len()];
    let mut wide = HashSet::new();
    for &i in order {
        let decl = declarations[i];
        let eligible = match decl {
            Declaration::Enum(e) => e.type_params.is_empty(),
            Declaration::Record(r) => r.type_params.is_empty() && !drops.contains(&r.id),
            Declaration::Alias(a) => a.type_params.is_empty(),
        };
        if eligible {
            let tag = usize::from(matches!(decl, Declaration::Enum(_)));
            widths[i] = decl.dependencies().into_iter().try_fold(tag, |n, (ty, _)| {
                Some(OVERFLOW.min(n + width(ty, names, &widths, &mut tuples, 0)?))
            });
            if tag == 1 && widths[i] == Some(OVERFLOW) {
                wide.insert(i);
            }
        }
    }
    for &i in order.iter().rev() {
        if wide.contains(&i) {
            wide.extend(edges[i].iter().map(|&(j, _)| j));
        }
    }
    wide.retain(|&i| matches!(declarations[i], Declaration::Enum(e) if e.type_params.is_empty() && e.variants.iter().any(|v| v.payload.is_some())));
    wide
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
) -> Result<Type, Error> {
    let mut ty = ty.clone();
    resolve_type(&mut ty, canonical, registry).map_err(|e| e.with_span(span))?;
    registry.validate(&ty).map_err(|e| e.with_span(span))?;
    if !ty.is_copy_storage_value() && !ty.is_scoped_borrowed_value() && !ty.is_owner() {
        let requirement = "Record fields and enum payloads require a Copy value or an owner";
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            format!("{requirement}; buffer views cannot be stored, got {ty}"),
        ));
    }
    Ok(ty)
}

fn validate_cursor_packet<'a>(
    types: impl Iterator<Item = &'a Type> + Clone,
    span: Span,
) -> Result<(), Error> {
    if types.clone().any(Type::is_scoped_cursor_value)
        && types.clone().any(|ty| {
            !ty.is_scoped_cursor_value()
                && (!ty.is_copy_storage_value() || ty.contains_references())
        })
    {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            "Exclusive cursor packets require cursor loans and reference-free Copy metadata",
        ));
    }
    Ok(())
}

/// Owning records contain one or more owners and reference-free Copy metadata.
pub(super) fn record_owners(fields: &[RecordField], span: Span) -> Result<Vec<Arc<Type>>, Error> {
    validate_cursor_packet(fields.iter().map(|field| &field.ty), span)?;
    let owners: Vec<_> = fields
        .iter()
        .filter(|field| field.ty.is_owner())
        .flat_map(|field| field.ty.buffer_elements().iter().cloned())
        .collect();
    if !owners.is_empty()
        && fields.iter().any(|field| {
            !field.ty.is_owner()
                && (!field.ty.is_copy_storage_value() || field.ty.contains_references())
        })
    {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            "Additional fields of an owning record must be reference-free Copy data",
        ));
    }
    Ok(owners)
}

/// Owning sums keep fixed owner lanes for every variant. Inactive lanes are null.
pub(super) fn enum_owners(variants: &[EnumVariant], span: Span) -> Result<Vec<Arc<Type>>, Error> {
    if variants
        .iter()
        .filter_map(|v| v.payload.as_ref())
        .any(Type::is_scoped_cursor_value)
    {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            "Exclusive cursor packets cannot be stored in enum payloads",
        ));
    }
    let owners: Vec<_> = variants
        .iter()
        .filter_map(|v| v.payload.as_ref())
        .filter(|ty| ty.is_owner())
        .flat_map(|ty| ty.buffer_elements().iter().cloned())
        .collect();
    if !owners.is_empty()
        && variants
            .iter()
            .filter_map(|v| v.payload.as_ref())
            .any(|ty| !ty.is_owner() && !ty.is_copy_storage_value() && !ty.is_scoped_shared_value())
    {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            "Payloads of an owning enum must be owners, Copy data or scoped shared borrows",
        ));
    }
    Ok(owners)
}

const MAX_SCALAR_SLOTS: usize = 65_536;

pub(super) fn extend_layout(
    scalars: &mut Vec<Type>,
    depth: &mut usize,
    ty: &Type,
    span: Span,
    kind: &str,
) -> Result<(), Error> {
    if ty.contains_explicit_arena() {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            "Arena capabilities cannot be stored in aggregates or containers",
        ));
    }
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
        .is_none_or(|size| size > MAX_SCALAR_SLOTS)
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
        if !ty.is_copy_storage_value() && !ty.is_scoped_borrowed_value() && !ty.is_owner() {
            return Err(error(
                ErrorCode::TypeMismatch,
                span,
                format!(
                    "Tuple element must be an integer, Bool, Unit, a copyable aggregate or an owner, got {ty}"
                ),
            ));
        }
        extend_layout(&mut scalars, &mut depth, ty, span, "Tuple")?;
    }
    validate_cursor_packet(elements.iter(), span)?;
    let owners: Vec<_> = elements
        .iter()
        .filter(|ty| ty.is_owner())
        .flat_map(|ty| ty.buffer_elements().iter().cloned())
        .collect();
    if !owners.is_empty()
        && elements
            .iter()
            .any(|ty| !ty.is_owner() && ty.contains_references())
    {
        return Err(error(
            ErrorCode::TypeMismatch,
            span,
            "Additional elements of an owning tuple must be reference-free Copy data",
        ));
    }
    let ty = Type::Tuple(Arc::new(TupleType {
        owners,
        elements,
        scalars,
        depth,
    }));
    super::fixed_array::validate(&ty, registry)?;
    Ok(ty)
}
