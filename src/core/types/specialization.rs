//! Bounded nominal specialization and substitution shared with generic functions.
use super::*;
use std::sync::Arc;

// Includes rigid instances from imported standard-library templates.
pub(super) const MAX_RECORD_INSTANCES: usize = 512;
pub(super) const MAX_ENUM_INSTANCES: usize = 512;

// Nominal equality intentionally ignores layout metadata. A forward reference
// may gain ownership after its declaration is resolved; do not reuse an alias
// expansion checked against the earlier Copy classification.
fn owner_classifications(ty: &Type) -> BTreeSet<(u64, bool)> {
    TypeWalk::new(ty, TypeEdges::Arguments)
        .filter_map(|ty| match ty {
            Type::Record(record) => Some((record.id, ty.is_owner())),
            _ => None,
        })
        .collect()
}

impl TypeRegistry {
    pub(crate) fn instantiate_alias(
        &self,
        alias: &Arc<AliasTemplate>,
        arguments: Vec<GenericArgument>,
    ) -> Result<Type, Error> {
        self.with_alias_obligation(&alias.name, &arguments, || {
            self.instantiate_alias_inner(alias, arguments.clone())
        })
    }

    fn instantiate_alias_inner(
        &self,
        alias: &Arc<AliasTemplate>,
        arguments: Vec<GenericArgument>,
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
        for (p, a) in alias.parameters.iter().zip(&arguments) {
            self.validate_argument(p, a)?;
        }
        // Validate even unused parameters before erasing the transparent alias.
        for arg in arguments.iter().filter_map(|a| a.as_type()) {
            self.validate(arg)?;
            if arg.contains_unstorable_closures() && !arg.is_scoped_borrowed_value()
                || !arg.is_copy_storage_value()
                    && !arg.is_scoped_borrowed_value()
                    && !arg.is_owner()
                    && !arg.is_callable_parameter()
            {
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
        arguments: Vec<GenericArgument>,
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
        for arg in arguments.iter().filter_map(|a| a.as_type()) {
            self.validate(arg)?;
            if arg.contains_unstorable_closures() && !arg.is_scoped_borrowed_value()
                || !arg.is_copy_storage_value()
                    && !arg.is_scoped_borrowed_value()
                    && !arg.is_owner()
                    && !arg.is_callable_parameter()
            {
                return Err(error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "Generic record argument must be a Copy type or owning record, got {arg}"
                    ),
                ));
            }
        }
        for (p, a) in template.parameters.iter().zip(&arguments) {
            self.validate_argument(p, a)?;
        }
        if let Some(instance) = self
            .record_instances
            .borrow()
            .iter()
            .find(|r| r.id == template.id && r.arguments == arguments)
        {
            return Ok(Type::Record(instance.clone()));
        }
        if self.record_instances.borrow().len() >= MAX_RECORD_INSTANCES {
            return Err(error(
                ErrorCode::ResourceLimit,
                format!("Generic record instantiation exceeds {MAX_RECORD_INSTANCES} instances"),
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
            .map(|a| a.as_type().map_or(0, |ty| ty.layout_depth()) + 1)
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
        let owners = layout::record_owners(&fields, crate::Span::empty(0))?;

        let instance = Arc::new(RecordType {
            array: template.array,
            list: template.list,
            map: template.map,
            set: template.set,
            closure: None,
            owners,
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
        if self.record_instances.borrow().len() >= MAX_RECORD_INSTANCES {
            return Err(error(
                ErrorCode::ResourceLimit,
                format!("Generic record instantiation exceeds {MAX_RECORD_INSTANCES} instances"),
            ));
        }
        super::fixed_array::validate(&Type::Record(instance.clone()), self)?;
        self.record_instances.borrow_mut().push(instance.clone());
        Ok(Type::Record(instance))
    }

    pub(crate) fn instantiate_enum(
        &self,
        enumeration: &Arc<EnumType>,
        arguments: Vec<GenericArgument>,
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
        for arg in arguments.iter().filter_map(|a| a.as_type()) {
            self.validate(arg)?;
            if arg.contains_closures()
                || !arg.is_copy_storage_value()
                    && !arg.is_scoped_borrowed_value()
                    && !arg.is_owner()
            {
                return Err(error(
                    ErrorCode::TypeMismatch,
                    format!("Generic enum argument must be a Copy type or an owner, got {arg}"),
                ));
            }
        }
        for (p, a) in template.parameters.iter().zip(&arguments) {
            self.validate_argument(p, a)?;
        }
        if let Some(instance) = self
            .enum_instances
            .borrow()
            .iter()
            .find(|e| e.id == template.id && e.arguments == arguments)
        {
            return Ok(Type::Enum(instance.clone()));
        }
        if self.enum_instances.borrow().len() >= MAX_ENUM_INSTANCES {
            return Err(error(
                ErrorCode::ResourceLimit,
                format!("Generic enum instantiation exceeds {MAX_ENUM_INSTANCES} instances"),
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
            .map(|a| a.as_type().map_or(0, |ty| ty.layout_depth()) + 1)
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
            compact: false,
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
        if self.enum_instances.borrow().len() >= MAX_ENUM_INSTANCES {
            return Err(error(
                ErrorCode::ResourceLimit,
                format!("Generic enum instantiation exceeds {MAX_ENUM_INSTANCES} instances"),
            ));
        }
        super::fixed_array::validate(&Type::Enum(instance.clone()), self)?;
        self.enum_instances.borrow_mut().push(instance.clone());
        Ok(Type::Enum(instance))
    }
}

pub(crate) fn substitute(
    ty: &Type,
    substitutions: &Substitutions,
    types: &TypeRegistry,
) -> Result<Type, Error> {
    #[derive(Default)]
    struct Cache {
        functions: HashMap<*const FunctionType, Type>,
        tuples: HashMap<*const TupleType, Type>,
        enums: HashMap<*const EnumType, Type>,
        records: HashMap<*const RecordType, Type>,
    }
    fn visit(
        ty: &Type,
        substitutions: &Substitutions,
        types: &TypeRegistry,
        cache: &mut Cache,
    ) -> Result<Type, Error> {
        Ok(match ty {
            Type::FixedArrayInit(a) => {
                let Type::FixedArray(shape) = types.fixed_array(
                    visit(&a.element, substitutions, types, cache)?,
                    substitutions.constant(&a.length),
                )?
                else {
                    unreachable!()
                };
                Type::FixedArrayInit(shape)
            }
            Type::FixedArray(a) => types.fixed_array(
                visit(&a.element, substitutions, types, cache)?,
                substitutions.constant(&a.length),
            )?,
            Type::DynTrait(d) => {
                let mut d = d.as_ref().clone();
                for e in &mut d.equalities {
                    e.ty = visit(&e.ty, substitutions, types, cache)?;
                }
                d.interface = None;
                Type::DynTrait(Arc::new(types.normalize_dyn_trait(&d)?))
            }
            Type::Function(signature) => {
                let key = Arc::as_ptr(signature);
                if let Some(ty) = cache.functions.get(&key) {
                    return Ok(ty.clone());
                }
                let ty = Type::function_with_contract(
                    signature
                        .signature
                        .params
                        .iter()
                        .map(|p| visit(p, substitutions, types, cache))
                        .collect::<Result<_, _>>()?,
                    visit(&signature.signature.ret, substitutions, types, cache)?,
                    signature.suspends,
                    signature.noescape,
                );
                cache.functions.insert(key, ty.clone());
                ty
            }
            Type::Buffer(element)
            | Type::BufferElement(element)
            | Type::BufferSlice(element)
            | Type::BufferIterator(element)
            | Type::BufferPairIterator(element)
            | Type::BufferEntry(element) => {
                let element = visit(element, substitutions, types, cache)?;
                // A recursive argument can be shallow inside an otherwise
                // specialized element. Keep its identity until the complete
                // declaration graph can supply the inline element layout.
                let element = if matches!(ty, Type::Buffer(_))
                    && element.nominal().is_some()
                    && TypeWalk::new(&element, TypeEdges::Inline)
                        .skip(1)
                        .any(|ty| {
                            matches!(ty, Type::Record(r) if r.indirect)
                                || matches!(ty, Type::Enum(e) if e.indirect)
                        }) {
                    indirect::resolve_element(&element, &HashMap::new(), types, 0)?
                } else {
                    element
                };
                let result = ty.with_buffer_element(element);
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
                            .map(|a| match a {
                                GenericArgument::Type(t) => Ok::<_, Error>(GenericArgument::Type(
                                    visit(t, substitutions, types, cache)?,
                                )),
                                GenericArgument::Const(c) => {
                                    Ok(GenericArgument::Const(substitutions.constant(c)))
                                }
                            })
                            .collect::<Result<_, _>>()?,
                    ),
                    Type::Enum(enumeration) => indirect::shallow(
                        &r.target,
                        enumeration
                            .arguments
                            .iter()
                            .map(|a| match a {
                                GenericArgument::Type(t) => Ok::<_, Error>(GenericArgument::Type(
                                    visit(t, substitutions, types, cache)?,
                                )),
                                GenericArgument::Const(c) => {
                                    Ok(GenericArgument::Const(substitutions.constant(c)))
                                }
                            })
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
                Type::Applied(Arc::new(TypeApplication {
                    name: application.name.clone(),
                    arguments: application
                        .arguments
                        .iter()
                        .map(|a| match a {
                            GenericArgument::Type(t) => Ok::<_, Error>(GenericArgument::Type(
                                visit(t, substitutions, types, cache)?,
                            )),
                            GenericArgument::Const(c) => {
                                Ok(GenericArgument::Const(substitutions.constant(c)))
                            }
                        })
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
                let arguments =
                    enumeration
                        .arguments
                        .iter()
                        .map(|a| match a {
                            GenericArgument::Type(t) => Ok::<_, Error>(GenericArgument::Type(
                                visit(t, substitutions, types, cache)?,
                            )),
                            GenericArgument::Const(c) => {
                                Ok(GenericArgument::Const(substitutions.constant(c)))
                            }
                        })
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
                let arguments =
                    record
                        .arguments
                        .iter()
                        .map(|a| match a {
                            GenericArgument::Type(t) => Ok::<_, Error>(GenericArgument::Type(
                                visit(t, substitutions, types, cache)?,
                            )),
                            GenericArgument::Const(c) => {
                                Ok(GenericArgument::Const(substitutions.constant(c)))
                            }
                        })
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
    substitutions: &mut Substitutions,
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
            (Type::FixedArray(a), Type::FixedArray(b))
            | (Type::FixedArrayInit(a), Type::FixedArrayInit(b)) => {
                unify_constant(&a.length, &b.length, params, substitutions, span)?;
                pending.push((&a.element, &b.element));
            }
            (Type::Projection(_), _) => {} // Checked after ordinary parameter inference.
            (Type::DynTrait(a), Type::DynTrait(b))
                if a.interface
                    .as_ref()
                    .zip(b.interface.as_ref())
                    .is_some_and(|(a, b)| a.trait_id == b.trait_id) =>
            {
                let a = a.interface.as_ref().unwrap();
                let b = b.interface.as_ref().unwrap();
                pending.extend(
                    a.bindings
                        .iter()
                        .zip(&b.bindings)
                        .map(|((_, a), (_, b))| (a, b)),
                );
            }

            (Type::Function(a), Type::Function(b))
                if a.suspends == b.suspends
                    && (!a.noescape || b.noescape)
                    && a.signature.params.len() == b.signature.params.len() =>
            {
                if functions.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.signature.params.iter().zip(&b.signature.params));
                    pending.push((&a.signature.ret, &b.signature.ret));
                }
            }
            (Type::Function(a), Type::Function(b))
                if !a.suspends && b.suspends || a.noescape && !b.noescape =>
            {
                let help = if !a.suspends && b.suspends {
                    "Use a synchronous callback without @suspends"
                } else {
                    "Use an @noescape callback that does not retain or return argument borrows"
                };
                return Err(unification_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "Generic argument type mismatch: expected {formal}, got {actual}. {help}"
                    ),
                ));
            }
            (Type::Buffer(a), Type::Buffer(b))
            | (Type::BufferElement(a), Type::BufferElement(b))
            | (Type::BufferSlice(a), Type::BufferSlice(b))
            | (Type::BufferIterator(a), Type::BufferIterator(b))
            | (Type::BufferPairIterator(a), Type::BufferPairIterator(b))
            | (Type::BufferEntry(a), Type::BufferEntry(b)) => pending.push((a, b)),
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
                    for (a, b) in a.arguments.iter().zip(&b.arguments) {
                        match (a, b) {
                            (GenericArgument::Type(a), GenericArgument::Type(b)) => {
                                pending.push((a, b))
                            }
                            (GenericArgument::Const(a), GenericArgument::Const(b)) => {
                                unify_constant(a, b, params, substitutions, span)?
                            }
                            _ => {
                                return Err(unification_error(
                                    span,
                                    ErrorCode::TypeMismatch,
                                    "Generic argument kind mismatch",
                                ))
                            }
                        }
                    }
                }
            }
            (Type::Record(a), Type::Record(b))
                if a.id == b.id && a.arguments.len() == b.arguments.len() =>
            {
                if records.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    for (a, b) in a.arguments.iter().zip(&b.arguments) {
                        match (a, b) {
                            (GenericArgument::Type(a), GenericArgument::Type(b)) => {
                                pending.push((a, b))
                            }
                            (GenericArgument::Const(a), GenericArgument::Const(b)) => {
                                unify_constant(a, b, params, substitutions, span)?
                            }
                            _ => {
                                return Err(unification_error(
                                    span,
                                    ErrorCode::TypeMismatch,
                                    "Generic argument kind mismatch",
                                ))
                            }
                        }
                    }
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

pub(crate) fn unify_constant(
    a: &ConstArgument,
    b: &ConstArgument,
    params: &[Arc<TypeParameter>],
    s: &mut Substitutions,
    span: Span,
) -> Result<(), Error> {
    if let ConstArgument::Parameter(p) = a {
        if params.iter().any(|q| q.id == p.id) {
            if let Some(previous) = s.constants.get(&p.id) {
                if previous == b {
                    return Ok(());
                }
            } else {
                s.constants.insert(p.id, b.clone());
                return Ok(());
            }
        }
    }
    if a == b {
        Ok(())
    } else {
        Err(unification_error(
            span,
            ErrorCode::TypeMismatch,
            format!("Conflicting constant arguments: {a} and {b}"),
        ))
    }
}
