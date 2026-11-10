//! Record pattern resolution, irrefutable destructuring plans, and shared
//! field-level checks for let/var bindings and enum match payloads.
use super::patterns::{CheckedPattern, Kind};
use super::*;
use crate::core::types::{ConstArgument, FixedArrayType, RecordType};
use crate::parser::ast::{BindingPattern, RecordFieldPattern};
use std::collections::HashSet;

/// A fully checked irrefutable pattern over scalar-slot coordinates.
pub(super) struct CheckedBinding {
    ty: Type,
    kind: BindingKind,
    span: Span,
}

enum BindingKind {
    Sequence(Vec<(usize, CheckedBinding)>),
    Any(Option<String>),
    /// Children with their scalar-slot offsets relative to this value.
    Tuple(Vec<(usize, CheckedBinding)>),
    /// Children with their declared field indices, in pattern order.
    Record(Vec<(usize, CheckedBinding)>),
}

struct BindingLeaf {
    owner: Option<Slot>,
    name: String,
    slot: Option<Slot>,
    span: Span,
}

impl CheckedBinding {
    fn binds(&self) -> bool {
        match &self.kind {
            BindingKind::Any(name) => name.is_some(),
            BindingKind::Tuple(elements) | BindingKind::Sequence(elements) => {
                elements.iter().any(|(_, p)| p.binds())
            }
            BindingKind::Record(fields) => fields.iter().any(|(_, p)| p.binds()),
        }
    }
}

impl FunctionPlan {
    /// Resolve the record a pattern head names and check its nominal match
    /// against the value type being destructured.
    pub(super) fn resolve_record_pattern(
        &self,
        name: &str,
        resolved_head: Option<&Type>,
        type_args: &[crate::core::types::GenericArgument],
        expected: &Type,
        span: Span,
        types: &TypeRegistry,
    ) -> Result<Arc<RecordType>, Error> {
        if let Some(head) = resolved_head {
            validate_nominal_type(head, &self.nominals, &span)?;
            if let Type::Record(actual) = expected {
                if head == expected {
                    return Ok(actual.clone());
                }
            }
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                format!("Record pattern `{name}` requires {head}, got {expected}"),
            ));
        }
        let template = match self.named_type(name) {
            Some(ty) => ty.clone(),
            None => {
                // Transparent aliases bind their resolved definition in scope.
                let binding = self.scope.types.get(name).ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::UnknownType,
                        format!("Unknown record pattern `{name}`"),
                    )
                })?;
                let definition = binding.resolved.as_ref().ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::UnknownType,
                        format!("Unknown record pattern `{name}`"),
                    )
                })?;
                match definition {
                    super::super::types::TypeDefinition::Type(ty) => ty.clone(),
                    super::super::types::TypeDefinition::Alias(alias) => {
                        types.instantiate_alias(alias, type_args.to_vec())?
                    }
                }
            }
        };
        let Type::Record(template) = template else {
            return Err(type_error(
                &span,
                ErrorCode::InvalidPattern,
                format!("`{name}` is not a record; record patterns require a nominal record"),
            ));
        };
        let resolved = if template.parameters.is_empty() {
            if !type_args.is_empty() {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Non-generic record patterns do not accept type arguments",
                ));
            }
            Type::Record(template.clone())
        } else if type_args.is_empty() {
            match expected {
                Type::Record(actual) if actual.id == template.id => Type::Record(actual.clone()),
                _ => {
                    return Err(type_error(
                        &span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "Cannot infer type parameters for record pattern `{name}`; \
                             write `{name}::<...>` or rely on the value's nominal type"
                        ),
                    ))
                }
            }
        } else {
            if type_args.len() != template.parameters.len() {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    format!(
                        "Type argument count mismatch for record `{name}`: expected {}",
                        template.parameters.len()
                    ),
                ));
            }
            let ty = types
                .instantiate_record(&template, type_args.to_vec())
                .map_err(|e| e.with_span(span))?;
            validate_nominal_type(&ty, &self.nominals, &span)?;
            ty
        };
        let Type::Record(actual) = expected else {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                format!("Record pattern `{name}` requires {resolved}, got {expected}"),
            ));
        };
        // Rigid parameters retain their identities during template checking;
        // unused templates must obey the same nominal and argument equality.
        if resolved != *expected {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                format!("Record pattern `{name}` requires {resolved}, got {expected}"),
            ));
        }
        Ok(actual.clone())
    }

    /// Check listed fields for existence, visibility, duplicates, and the
    /// completeness contract; returns the declared field indices in pattern order.
    pub(super) fn check_record_pattern_fields(
        &self,
        record: &RecordType,
        fields: &[RecordFieldPattern],
        rest: bool,
        span: Span,
    ) -> Result<Vec<usize>, Error> {
        let mut indices = Vec::with_capacity(fields.len());
        for field in fields {
            let Some(index) = record.fields.iter().position(|f| f.name == field.name) else {
                return Err(type_error(
                    &field.span,
                    ErrorCode::UnknownName,
                    format!("Record `{}` has no field `{}`", record.name, field.name),
                ));
            };
            self.check_field(record, &field.name, &field.span)?;
            if indices.contains(&index) {
                return Err(type_error(
                    &field.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate field `{}` in record pattern", field.name),
                ));
            }
            indices.push(index);
        }
        if !rest {
            let missing: Vec<&crate::core::types::RecordField> = record
                .fields
                .iter()
                .enumerate()
                .filter(|(index, _)| !indices.contains(index))
                .map(|(_, field)| field)
                .collect();
            if !missing.is_empty() {
                // Never list inaccessible fields in the diagnostic.
                if missing
                    .iter()
                    .any(|field| !self.field_visible(record, &field.name))
                {
                    return Err(type_error(
                        &span,
                        ErrorCode::UnknownName,
                        format!(
                            "Record pattern `{}` must list every field or end with `..`; \
                             some fields are not visible here",
                            record.name
                        ),
                    ));
                }
                let names = missing
                    .iter()
                    .map(|field| format!("`{}`", field.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "Record pattern `{}` is missing field{} {names}",
                        record.name,
                        if missing.len() == 1 { "" } else { "s" },
                    ),
                ));
            }
        }
        Ok(indices)
    }

    pub(super) fn check_binding_pattern(
        &self,
        pattern: &BindingPattern,
        ty: &Type,
        types: &TypeRegistry,
        names: &mut HashSet<String>,
    ) -> Result<CheckedBinding, Error> {
        let span = pattern.span();
        let kind = match pattern {
            BindingPattern::Sequence(p) => {
                let Type::FixedArray(array) = ty else {
                    return Err(type_error(&span, ErrorCode::InvalidPattern, "Sequence let/var destructuring requires FixedArray; use match or if let for dynamic lengths"));
                };
                let ConstArgument::Value(length) = array.length else {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        "Sequence let/var destructuring requires a concrete fixed length",
                    ));
                };
                let count = (p.prefix.len() + p.suffix.len()) as u64;
                if count > length || p.rest.is_none() && count != length {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        "Sequence binding must match the fixed length",
                    ));
                }
                let stride = super::sequence_patterns::element_stride(&array.element, span)?;
                let mut children = Vec::new();
                for (i, child) in p.prefix.iter().chain(&p.suffix).enumerate() {
                    let index = if i < p.prefix.len() {
                        i
                    } else {
                        length as usize - p.suffix.len() + i - p.prefix.len()
                    };
                    children.push((
                        index * stride,
                        self.check_binding_pattern(child, &array.element, types, names)?,
                    ));
                }
                if let Some(Some(name)) = &p.rest {
                    if !names.insert(name.clone()) {
                        return Err(type_error(
                            &span,
                            ErrorCode::DuplicateDefinition,
                            format!("Duplicate binding `{name}` in pattern"),
                        ));
                    }
                    let rest = Type::FixedArray(Arc::new(FixedArrayType {
                        element: array.element.clone(),
                        length: ConstArgument::Value(length - count),
                    }));
                    children.push((
                        p.prefix.len() * stride,
                        CheckedBinding {
                            ty: rest,
                            kind: BindingKind::Any(Some(name.clone())),
                            span,
                        },
                    ));
                }
                BindingKind::Sequence(children)
            }
            BindingPattern::Name { name, span } => {
                if !names.insert(name.clone()) {
                    return Err(type_error(
                        span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate binding `{name}` in pattern"),
                    ));
                }
                BindingKind::Any(Some(name.clone()))
            }
            BindingPattern::Discard { .. } => BindingKind::Any(None),
            BindingPattern::Tuple { elements, span } => {
                let Type::Tuple(tuple) = ty else {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!("Tuple pattern requires a tuple, got {ty}"),
                    ));
                };
                if elements.len() != tuple.elements.len() {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "Tuple pattern has {} elements, value has {}",
                            elements.len(),
                            tuple.elements.len()
                        ),
                    ));
                }
                let mut offset = 0;
                let mut children = Vec::with_capacity(elements.len());
                for (element, element_ty) in elements.iter().zip(&tuple.elements) {
                    children.push((
                        offset,
                        self.check_binding_pattern(element, element_ty, types, names)?,
                    ));
                    offset += element_ty.scalar_types().len();
                }
                BindingKind::Tuple(children)
            }
            BindingPattern::Record { inner, span } => {
                let record = self.resolve_record_pattern(
                    &inner.name,
                    inner.resolved.as_deref(),
                    &inner.type_args,
                    ty,
                    *span,
                    types,
                )?;
                let indices =
                    self.check_record_pattern_fields(&record, &inner.fields, inner.rest, *span)?;
                let mut children = Vec::with_capacity(inner.fields.len());
                for (index, field) in indices.iter().zip(&inner.fields) {
                    children.push((
                        *index,
                        self.check_binding_pattern(
                            &field.pattern,
                            &record.fields[*index].ty,
                            types,
                            names,
                        )?,
                    ));
                }
                let checked = CheckedBinding {
                    ty: Type::Record(record.clone()),
                    kind: BindingKind::Record(children),
                    span: *span,
                };
                return Ok(checked);
            }
        };
        Ok(CheckedBinding {
            ty: ty.clone(),
            kind,
            span,
        })
    }

    /// Check a let/var pattern (or a record-pattern field) against a shared
    /// reference target and bind it through borrowed projections.
    fn bind_shared_binding(
        &mut self,
        pattern: &BindingPattern,
        root: &Slot,
        types: &TypeRegistry,
        mutable: bool,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        let target = self.pointee(&root.ty.reference_type().unwrap().target, types)?;
        let mut names = HashSet::new();
        let checked = self.check_nested_binding(pattern, &target, types, &mut names)?;
        self.bind_borrowed_pattern(&checked, root, 0, &mut Vec::new(), mutable, vars)
    }

    /// Check a BindingPattern against a type, producing the shared CheckedPattern
    /// tree used by borrowed projections and match payload record fields.
    pub(super) fn check_nested_binding(
        &self,
        pattern: &BindingPattern,
        ty: &Type,
        types: &TypeRegistry,
        names: &mut HashSet<String>,
    ) -> Result<CheckedPattern, Error> {
        let span = pattern.span();
        let kind = match pattern {
            BindingPattern::Sequence(p) => {
                // Record-field binding patterns are irrefutable as well.
                self.check_binding_pattern(pattern, ty, types, &mut HashSet::new())?;
                return self.check_sequence_pattern(
                    p,
                    ty,
                    names,
                    types,
                    |plan, p, ty, names, types| plan.check_nested_binding(p, ty, types, names),
                );
            }
            BindingPattern::Name { name, span } => {
                if !names.insert(name.clone()) {
                    return Err(type_error(
                        span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate binding `{name}` in pattern"),
                    ));
                }
                Kind::Any(Some(name.clone()))
            }
            BindingPattern::Discard { .. } => Kind::Any(None),
            BindingPattern::Tuple { elements, span } => {
                let Type::Tuple(tuple) = ty else {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!("Tuple pattern requires a tuple, got {ty}"),
                    ));
                };
                if elements.len() != tuple.elements.len() {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "Tuple pattern has {} elements, value has {}",
                            elements.len(),
                            tuple.elements.len()
                        ),
                    ));
                }
                Kind::Tuple(
                    elements
                        .iter()
                        .zip(&tuple.elements)
                        .map(|(element, ty)| self.check_nested_binding(element, ty, types, names))
                        .collect::<Result<_, _>>()?,
                )
            }
            BindingPattern::Record { inner, span } => {
                let record = self.resolve_record_pattern(
                    &inner.name,
                    inner.resolved.as_deref(),
                    &inner.type_args,
                    ty,
                    *span,
                    types,
                )?;
                let indices =
                    self.check_record_pattern_fields(&record, &inner.fields, inner.rest, *span)?;
                let mut children = Vec::with_capacity(inner.fields.len());
                for (index, field) in indices.iter().zip(&inner.fields) {
                    children.push((
                        *index,
                        self.check_nested_binding(
                            &field.pattern,
                            &record.fields[*index].ty,
                            types,
                            names,
                        )?,
                    ));
                }
                // Shared projections never move fields or release owners.
                return Ok(CheckedPattern::record(
                    Type::Record(record.clone()),
                    children,
                    *span,
                ));
            }
        };
        Ok(CheckedPattern::nested(ty.clone(), kind, span))
    }
    /// Destructure a checked value pattern: detach owning leaves, publish all
    /// bindings in pattern order, then release the ignored remainder.
    fn lower_binding_pattern(
        &mut self,
        checked: &CheckedBinding,
        value: Option<Slot>,
        owner: Option<&Slot>,
        leaves: &mut Vec<BindingLeaf>,
    ) -> Result<(), Error> {
        if !checked.binds() {
            return Ok(());
        }
        self.current_span = checked.span;
        match &checked.kind {
            BindingKind::Any(name) => leaves.push(BindingLeaf {
                owner: owner.cloned(),
                name: name.clone().expect("binding name"),
                slot: value,
                span: checked.span,
            }),
            BindingKind::Sequence(children) => {
                let base = value.as_ref().unwrap();
                for (offset, child) in children {
                    let value = self.place_read(base, *offset, child.ty.clone())?;
                    self.lower_binding_pattern(
                        child,
                        value.clone(),
                        value.as_ref().filter(|v| v.ty.is_owner()),
                        leaves,
                    )?;
                }
            }
            BindingKind::Tuple(children) => {
                let base = value.as_ref().unwrap().index;
                for (index, (offset, child)) in children.iter().enumerate() {
                    if checked.ty.is_place() {
                        let slot = self.place_child(value.as_ref().unwrap(), index)?;
                        self.lower_binding_pattern(
                            child,
                            slot.clone(),
                            slot.as_ref().filter(|v| v.ty.is_owner()),
                            leaves,
                        )?;
                        continue;
                    }
                    let slot = (child.ty != Type::Unit).then(|| Slot {
                        index: base + offset,
                        ty: child.ty.clone(),
                    });
                    self.lower_binding_pattern(child, slot, owner, leaves)?;
                }
            }
            BindingKind::Record(children) => {
                let Type::Record(record) = &checked.ty else {
                    unreachable!("checked record pattern")
                };
                let base = value.as_ref().unwrap().index;
                for (index, child) in children {
                    if checked.ty.is_place() {
                        let slot = self.place_child(value.as_ref().unwrap(), *index)?;
                        self.lower_binding_pattern(
                            child,
                            slot.clone(),
                            slot.as_ref().filter(|v| v.ty.is_owner()),
                            leaves,
                        )?;
                        continue;
                    }
                    let field = &record.fields[*index];
                    let slot = (field.ty != Type::Unit).then(|| Slot {
                        index: base + field.offset,
                        ty: field.ty.clone(),
                    });
                    self.lower_binding_pattern(child, slot, owner, leaves)?;
                }
            }
        }
        Ok(())
    }

    /// Plan an irrefutable let/var destructuring after its value is evaluated.
    pub(super) fn destructure_binding(
        &mut self,
        pattern: &BindingPattern,
        mutable: bool,
        value: Option<Slot>,
        suspends: bool,
        types: &TypeRegistry,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        if let Some(entry) = value.as_ref().filter(|slot| {
            matches!(pattern, BindingPattern::Tuple { .. })
                && slot
                    .ty
                    .reference_type()
                    .is_some_and(|r| matches!(r.target, Type::BufferEntry(_)))
        }) {
            return self.bind_entry_pattern(pattern, entry, mutable, vars);
        }
        // Shared-borrow destructuring covers record patterns; a tuple pattern
        // on a reference keeps its existing rejection.
        if let Some(reference) = value
            .as_ref()
            .filter(|slot| slot.ty.reference_type().is_some())
            .filter(|_| {
                matches!(
                    pattern,
                    BindingPattern::Record { .. } | BindingPattern::Sequence(_)
                )
            })
        {
            let borrowed = reference.ty.reference_type().unwrap();
            if borrowed.mutable {
                return Err(type_error(
                    &pattern.span(),
                    ErrorCode::InvalidAssignment,
                    "Exclusive references cannot be destructured; use a shared reference",
                ));
            }
            if suspends {
                return Err(type_error(
                    &pattern.span(),
                    ErrorCode::InvalidCall,
                    "Borrowed record destructuring requires a synchronous function",
                ));
            }
            return self.bind_shared_binding(pattern, reference, types, mutable, vars);
        }
        let mut names = HashSet::new();
        let checked = self.check_binding_pattern(pattern, &slot_type(&value), types, &mut names)?;
        let mut leaves = Vec::new();
        let owner = value.as_ref().filter(|slot| slot.ty.is_owner()).cloned();
        self.lower_binding_pattern(&checked, value, owner.as_ref(), &mut leaves)?;
        for leaf in leaves {
            self.current_span = leaf.span;
            let slot = if slot_type(&leaf.slot).reference_type().is_some() {
                self.reference_value(leaf.slot, true)?
            } else if let Some(src) = leaf.slot.as_ref().filter(|slot| slot.ty.is_owner()) {
                let dst = self.slot(src.ty.clone())?;
                let root = leaf.owner.as_ref().expect("owner leaf");
                if root.ty.is_place() || root.index == src.index && root.ty == src.ty {
                    self.emit(Op::Copy(dst.clone(), src.clone()));
                } else {
                    self.emit(Op::Buffer(BufferOp::Detach {
                        dst: dst.clone(),
                        src: src.clone(),
                        owner: root.clone(),
                    }));
                }
                Some(dst)
            } else if mutable || owner.is_some() {
                self.snapshot(&leaf.slot)?
            } else {
                leaf.slot
            };
            let binding = self.local_binding(
                &leaf.name,
                Binding {
                    slot,
                    mutable,
                    place: None,
                },
            )?;
            vars.insert(leaf.name.clone(), binding);
        }
        if let Some(owner) = owner {
            self.emit(Op::Buffer(BufferOp::End(owner)));
        }
        Ok(())
    }
}
