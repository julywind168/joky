//! Typed recursive patterns, bounded coverage analysis, and consuming bindings.
use super::*;
use crate::parser::ast::{MatchArm, Pattern, PayloadPattern};
use std::collections::HashSet;

pub(super) struct CheckedPattern {
    ty: Type,
    kind: Kind,
    span: Span,
}
pub(super) enum Kind {
    Any(Option<String>),
    Tuple(Vec<CheckedPattern>),
    Variant {
        tag: usize,
        payload: Option<Box<CheckedPattern>>,
    },
    /// Declared field indices in pattern order; the type carries the record.
    Record(Vec<(usize, CheckedPattern)>),
}

impl CheckedPattern {
    pub(super) fn record(ty: Type, fields: Vec<(usize, CheckedPattern)>, span: Span) -> Self {
        Self {
            ty,
            kind: Kind::Record(fields),
            span,
        }
    }

    pub(super) fn nested(ty: Type, kind: Kind, span: Span) -> Self {
        Self { ty, kind, span }
    }

    pub(super) fn hints(&self, vars: &mut HashMap<String, Binding>, borrowed: Option<bool>) {
        match &self.kind {
            Kind::Any(Some(name)) => {
                let ty = if borrowed.is_some_and(|mutable| mutable || self.ty.is_owner()) {
                    Type::reference(self.ty.clone(), borrowed.unwrap())
                } else {
                    self.ty.clone()
                };
                vars.insert(name.clone(), payload_binding(&ty, 0));
            }
            Kind::Any(None) => {}
            Kind::Tuple(elements) => elements.iter().for_each(|p| p.hints(vars, borrowed)),
            Kind::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    payload.hints(vars, borrowed);
                }
            }
            Kind::Record(fields) => fields.iter().for_each(|(_, p)| p.hints(vars, borrowed)),
        }
    }
    fn storage_children(&self) -> Result<Vec<(&Self, usize)>, Error> {
        use crate::memory_layout::MemoryLayoutShape;
        if matches!(self.kind, Kind::Any(_)) {
            return Ok(Vec::new());
        }
        // Templates are checked before they have byte layouts; their IR is
        // discarded and rebuilt with concrete offsets for every specialization.
        if self
            .ty
            .scalar_types()
            .iter()
            .any(|ty| matches!(ty, Type::Parameter(_)))
        {
            return Ok(match &self.kind {
                Kind::Tuple(elements) => elements.iter().map(|p| (p, 0)).collect(),
                Kind::Variant { payload, .. } => payload.iter().map(|p| (p.as_ref(), 0)).collect(),
                Kind::Record(fields) => fields.iter().map(|(_, p)| (p, 0)).collect(),
                Kind::Any(_) => unreachable!(),
            });
        }
        let layout = self
            .ty
            .storage_layout()
            .map_err(|e| type_error(&self.span, ErrorCode::TypeMismatch, e.to_string()))?;
        Ok(match (&self.kind, layout.shape) {
            (Kind::Tuple(elements), MemoryLayoutShape::Tuple(fields)) => elements
                .iter()
                .zip(fields)
                .map(|(p, f)| (p, f.offset))
                .collect(),
            (Kind::Record(children), MemoryLayoutShape::Record(fields)) => children
                .iter()
                .map(|(index, p)| (p, fields[*index].offset))
                .collect(),
            (Kind::Variant { tag, payload }, MemoryLayoutShape::Enum { variants, .. }) => payload
                .iter()
                .map(|p| (p.as_ref(), variants[*tag].payload.unwrap().offset))
                .collect(),
            _ => unreachable!("checked pattern storage"),
        })
    }

    fn binds(&self) -> bool {
        match &self.kind {
            Kind::Any(name) => name.is_some(),
            Kind::Tuple(elements) => elements.iter().any(Self::binds),
            Kind::Variant { payload, .. } => payload.as_ref().is_some_and(|p| p.binds()),
            Kind::Record(fields) => fields.iter().any(|(_, p)| p.binds()),
        }
    }
    fn coverage(&self) -> Arc<Coverage> {
        Arc::new(match &self.kind {
            Kind::Any(_) => Coverage::Any,
            // Record patterns are irrefutable for their type.
            Kind::Record(_) => Coverage::Any,
            Kind::Tuple(elements) => {
                Coverage::Ctor(0, elements.iter().map(Self::coverage).collect())
            }
            Kind::Variant { tag, payload } => {
                Coverage::Ctor(*tag, payload.iter().map(|p| p.coverage()).collect())
            }
        })
    }
}

impl FunctionPlan {
    pub(super) fn check_match_pattern(
        &self,
        pattern: &Pattern,
        ty: &Type,
        types: &TypeRegistry,
    ) -> Result<CheckedPattern, Error> {
        let mut names = HashSet::new();
        match pattern {
            Pattern::Wildcard { span } => Ok(CheckedPattern {
                ty: ty.clone(),
                kind: Kind::Any(None),
                span: *span,
            }),
            Pattern::Variant {
                name,
                binding,
                span,
            } => self.check_variant_pattern(name, binding.as_deref(), ty, *span, &mut names, types),
        }
    }
    fn check_variant_pattern(
        &self,
        name: &str,
        payload: Option<&PayloadPattern>,
        ty: &Type,
        span: Span,
        names: &mut HashSet<String>,
        types: &TypeRegistry,
    ) -> Result<CheckedPattern, Error> {
        let Type::Enum(enumeration) = ty else {
            return Err(type_error(
                &span,
                ErrorCode::InvalidPattern,
                format!("Variant pattern requires an enum, got {ty}"),
            ));
        };
        let variant_name = self.pattern_variant(enumeration, name).ok_or_else(|| {
            type_error(
                &span,
                ErrorCode::InvalidPattern,
                format!(
                    "Pattern '{name}' does not belong to enum '{}'",
                    enumeration.name
                ),
            )
        })?;
        let (tag, variant) = enumeration
            .variants
            .iter()
            .enumerate()
            .find(|(_, v)| v.name == variant_name)
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::InvalidPattern,
                    format!(
                        "Enum `{}` has no variant `{variant_name}`",
                        enumeration.name
                    ),
                )
            })?;
        if variant.payload.is_some() != payload.is_some() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidPattern,
                format!("Pattern `{name}` must match its variant's payload arity"),
            ));
        }
        let payload = payload
            .zip(variant.payload.as_ref())
            .map(|(p, ty)| {
                self.check_payload_pattern(p, ty, names, types)
                    .map(Box::new)
            })
            .transpose()?;
        Ok(CheckedPattern {
            ty: ty.clone(),
            kind: Kind::Variant { tag, payload },
            span,
        })
    }
    fn check_payload_pattern(
        &self,
        pattern: &PayloadPattern,
        ty: &Type,
        names: &mut HashSet<String>,
        types: &TypeRegistry,
    ) -> Result<CheckedPattern, Error> {
        let span = pattern.span();
        let kind = match pattern {
            PayloadPattern::Discard { .. } => Kind::Any(None),
            PayloadPattern::Name { name, .. } => {
                // Nullary constructors are selected by the expected enum identity,
                // including imports. Other bare payload names introduce bindings.
                if let Type::Enum(enumeration) = ty {
                    let variant = self.pattern_variant(enumeration, name);
                    if variant
                        .is_some_and(|name| enumeration.variants.iter().any(|v| v.name == name))
                        || self.constructor_binding(name).is_some()
                    {
                        return self.check_variant_pattern(name, None, ty, span, names, types);
                    }
                }
                if !names.insert(name.clone()) {
                    return Err(type_error(
                        &span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate binding `{name}` in pattern"),
                    ));
                }
                Kind::Any(Some(name.clone()))
            }
            PayloadPattern::Variant { name, payload, .. } => {
                return self.check_variant_pattern(name, payload.as_deref(), ty, span, names, types)
            }
            PayloadPattern::Tuple { elements, .. } => {
                let Type::Tuple(tuple) = ty else {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        format!("Tuple pattern requires a tuple payload, got {ty}"),
                    ));
                };
                if elements.len() != tuple.elements.len() {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        format!(
                            "Tuple pattern has {} elements, payload has {}",
                            elements.len(),
                            tuple.elements.len()
                        ),
                    ));
                }
                Kind::Tuple(
                    elements
                        .iter()
                        .zip(&tuple.elements)
                        .map(|(p, ty)| self.check_payload_pattern(p, ty, names, types))
                        .collect::<Result<_, _>>()?,
                )
            }
            PayloadPattern::Record {
                name,
                type_args,
                fields,
                rest,
                ..
            } => {
                let record = self.resolve_record_pattern(name, type_args, ty, span, types)?;
                let indices = self.check_record_pattern_fields(&record, fields, *rest, span)?;
                let mut children = Vec::with_capacity(fields.len());
                for (index, field) in indices.iter().zip(fields) {
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
                return Ok(CheckedPattern::record(Type::Record(record), children, span));
            }
        };
        Ok(CheckedPattern {
            ty: ty.clone(),
            kind,
            span,
        })
    }

    pub(super) fn match_pattern_tests(
        &mut self,
        pattern: &CheckedPattern,
        index: usize,
        failure: usize,
    ) -> Result<(), Error> {
        self.current_span = pattern.span;
        match &pattern.kind {
            Kind::Any(_) | Kind::Record(_) => {}
            Kind::Tuple(elements) => {
                let mut offset = index;
                for element in elements {
                    self.match_pattern_tests(element, offset, failure)?;
                    offset += element.ty.scalar_types().len();
                }
            }
            Kind::Variant { tag, payload } => {
                let expected = self.slot(Type::Int32)?;
                self.emit(Op::Literal(expected.clone(), Constant::Int32(*tag as i32)));
                let condition = self.slot(Type::Bool)?;
                self.emit(Op::Binary(
                    condition.clone(),
                    BinOp::Eq,
                    Slot {
                        index,
                        ty: Type::Int32,
                    },
                    expected,
                ));
                let success = self.label_count;
                self.label_count += 1;
                self.emit(Op::Branch {
                    condition,
                    then_label: success,
                    else_label: failure,
                });
                self.emit(Op::Label(success));
                if let Some(payload) = payload {
                    let Type::Enum(enumeration) = &pattern.ty else {
                        unreachable!()
                    };
                    self.match_pattern_tests(
                        payload,
                        index + enumeration.variants[*tag].offset,
                        failure,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn pattern_reference(
        &mut self,
        root: &Slot,
        ty: &Type,
        offset: usize,
        mutable: bool,
        path: &[usize],
    ) -> Result<Slot, Error> {
        let dst = self.slot(Type::reference(ty.clone(), mutable))?;
        self.emit(Op::PatternProject {
            dst: dst.clone(),
            src: root.clone(),
            offset,
            path: path.to_vec(),
        });
        Ok(dst)
    }

    /// Read only discriminants until an arm succeeds. Offsets refer to the
    /// original enum storage, never to a copied or detached owner payload.
    pub(super) fn borrowed_pattern_tests(
        &mut self,
        pattern: &CheckedPattern,
        root: &Slot,
        offset: usize,
        failure: usize,
    ) -> Result<(), Error> {
        self.current_span = pattern.span;
        if let Kind::Variant { tag, .. } = &pattern.kind {
            let reference = self.pattern_reference(root, &Type::Int32, offset, false, &[])?;
            let actual = self.read_local(reference)?.unwrap();
            let expected = self.slot(Type::Int32)?;
            self.emit(Op::Literal(expected.clone(), Constant::Int32(*tag as i32)));
            let condition = self.slot(Type::Bool)?;
            self.emit(Op::Binary(condition.clone(), BinOp::Eq, actual, expected));
            let success = self.label_count;
            self.label_count += 1;
            self.emit(Op::Branch {
                condition,
                then_label: success,
                else_label: failure,
            });
            self.emit(Op::Label(success));
        }
        for (child, child_offset) in pattern.storage_children()? {
            self.borrowed_pattern_tests(child, root, offset + child_offset, failure)?;
        }
        Ok(())
    }

    pub(super) fn bind_borrowed_pattern(
        &mut self,
        pattern: &CheckedPattern,
        root: &Slot,
        offset: usize,
        path: &mut Vec<usize>,
        binding_mutable: bool,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        if !pattern.binds() {
            return Ok(());
        }
        self.current_span = pattern.span;
        if let Kind::Any(Some(name)) = &pattern.kind {
            let mutable = root.ty.reference_type().unwrap().mutable;
            // A mutable payload binding is a reference, including Copy scalars.
            // Nested reference targets remain outside the language's type model.
            if mutable && pattern.ty.reference_type().is_some() {
                return Err(type_error(&pattern.span, ErrorCode::TypeMismatch,
                    "Mutable patterns cannot bind reference payloads; use a shared match or discard the payload"));
            }
            let reference = self.pattern_reference(root, &pattern.ty, offset, mutable, path)?;
            let value = if mutable || pattern.ty.is_owner() {
                Some(reference)
            } else {
                self.read_local(reference)?
            };
            let binding = self.local_binding(
                name,
                Binding {
                    slot: value,
                    mutable: binding_mutable,
                    place: None,
                },
            )?;
            vars.insert(name.clone(), binding);
        } else {
            for (index, (child, child_offset)) in
                pattern.storage_children()?.into_iter().enumerate()
            {
                let component = match &pattern.kind {
                    Kind::Variant { tag, .. } => *tag,
                    Kind::Record(fields) => fields[index].0,
                    _ => index,
                };
                path.push(component);
                self.bind_borrowed_pattern(
                    child,
                    root,
                    offset + child_offset,
                    path,
                    binding_mutable,
                    vars,
                )?;
                path.pop();
            }
        }
        Ok(())
    }

    pub(super) fn bind_match_pattern(
        &mut self,
        pattern: &CheckedPattern,
        value: Option<Slot>,
        parent: Option<&Slot>,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        if !pattern.binds() {
            return Ok(());
        }
        self.current_span = pattern.span;
        // Shape checking is shared with borrowed matches. Only a by-value
        // match can detach fields and therefore needs the Drop move check.
        if let Kind::Record(fields) = &pattern.kind {
            let Type::Record(record) = &pattern.ty else {
                unreachable!("checked record pattern")
            };
            let mut moves = vec![(false, false); record.fields.len()];
            for (index, child) in fields {
                moves[*index] = match &child.kind {
                    Kind::Any(name) => (name.is_some(), false),
                    _ => (true, true),
                };
            }
            self.check_record_pattern_moves(record, &moves, pattern.span)?;
        }
        // Tests finish before any detachment. Detach each owning subtree into
        // its own group so tuple siblings and nested enum payloads stay unique.
        let value = if let (Some(src), Some(parent)) =
            (value.as_ref().filter(|s| s.ty.is_owner()), parent)
        {
            let dst = self.slot(src.ty.clone())?;
            self.emit(Op::Buffer(BufferOp::Detach {
                dst: dst.clone(),
                src: src.clone(),
                owner: parent.clone(),
            }));
            Some(dst)
        } else {
            value
        };
        match &pattern.kind {
            Kind::Any(Some(name)) => {
                let binding = self.local_binding(
                    name,
                    Binding {
                        slot: value,
                        mutable: false,
                        place: None,
                    },
                )?;
                vars.insert(name.clone(), binding);
            }
            Kind::Any(None) => {}
            Kind::Tuple(elements) => {
                let parent = value.as_ref().filter(|s| s.ty.is_owner());
                let mut offset = value.as_ref().unwrap().index;
                for element in elements {
                    let child = (element.ty != Type::Unit).then(|| Slot {
                        index: offset,
                        ty: element.ty.clone(),
                    });
                    self.bind_match_pattern(element, child, parent, vars)?;
                    offset += element.ty.scalar_types().len();
                }
            }
            Kind::Variant { tag, payload } => {
                if let Some(payload) = payload {
                    let Type::Enum(enumeration) = &pattern.ty else {
                        unreachable!()
                    };
                    let slot = value.as_ref().unwrap();
                    let child = (payload.ty != Type::Unit).then(|| Slot {
                        index: slot.index + enumeration.variants[*tag].offset,
                        ty: payload.ty.clone(),
                    });
                    self.bind_match_pattern(
                        payload,
                        child,
                        slot.ty.is_owner().then_some(slot),
                        vars,
                    )?;
                }
            }
            Kind::Record(fields) => {
                let Type::Record(record) = &pattern.ty else {
                    unreachable!()
                };
                let slot = value.as_ref().unwrap();
                let parent = slot.ty.is_owner().then_some(slot);
                for (index, child) in fields {
                    let field = &record.fields[*index];
                    let child_slot = (field.ty != Type::Unit).then(|| Slot {
                        index: slot.index + field.offset,
                        ty: field.ty.clone(),
                    });
                    self.bind_match_pattern(child, child_slot, parent, vars)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
enum Coverage {
    Any,
    Ctor(usize, Vec<Arc<Coverage>>),
}
fn any() -> Arc<Coverage> {
    Arc::new(Coverage::Any)
}

/// Constructor specialization preserves correlations between tuple columns.
/// A default row covers every omitted constructor without expanding its type DAG.
fn useful(
    matrix: &[Vec<Arc<Coverage>>],
    query: &[Arc<Coverage>],
    types: &[Type],
    budget: &mut usize,
    span: Span,
    depth: usize,
) -> Result<bool, Error> {
    if depth >= 512 {
        return Err(type_error(
            &span,
            ErrorCode::ResourceLimit,
            "Match coverage analysis exceeds 512 levels",
        ));
    }
    let cost = 1 + matrix.iter().map(Vec::len).sum::<usize>() + query.len();
    *budget = budget.checked_sub(cost).ok_or_else(|| {
        type_error(
            &span,
            ErrorCode::ResourceLimit,
            "Match coverage analysis exceeds 65536 steps",
        )
    })?;
    if matrix.is_empty() {
        return Ok(true);
    }
    if query.is_empty() {
        return Ok(false);
    }
    if matrix
        .iter()
        .any(|row| row.iter().all(|p| matches!(p.as_ref(), Coverage::Any)))
    {
        return Ok(false);
    }
    // Skip untested columns together; wide tuples must not grow the call stack.
    let skipped = query
        .iter()
        .enumerate()
        .take_while(|(i, p)| {
            matches!(p.as_ref(), Coverage::Any)
                && matrix
                    .iter()
                    .all(|row| matches!(row[*i].as_ref(), Coverage::Any))
        })
        .count();
    if skipped > 0 {
        let rows: Vec<_> = matrix.iter().map(|row| row[skipped..].to_vec()).collect();
        return useful(
            &rows,
            &query[skipped..],
            &types[skipped..],
            budget,
            span,
            depth + 1,
        );
    }
    let head = &query[0];
    let specialized = |tag: usize, args: Vec<Arc<Coverage>>, budget: &mut usize| {
        let fields: Vec<Type> = match &types[0] {
            Type::Enum(e) => e.variants[tag].payload.iter().cloned().collect(),
            Type::Tuple(t) => t.elements.clone(),
            _ => unreachable!(),
        };
        let rows: Vec<_> = matrix
            .iter()
            .filter_map(|row| {
                let mut first = match row[0].as_ref() {
                    Coverage::Any => vec![any(); fields.len()],
                    Coverage::Ctor(actual, fields) if *actual == tag => fields.clone(),
                    _ => return None,
                };
                first.extend_from_slice(&row[1..]);
                Some(first)
            })
            .collect();
        let mut candidate = args;
        candidate.extend_from_slice(&query[1..]);
        let mut new_types = fields;
        new_types.extend_from_slice(&types[1..]);
        useful(&rows, &candidate, &new_types, budget, span, depth + 1)
    };
    match head.as_ref() {
        Coverage::Ctor(tag, args) => specialized(*tag, args.clone(), budget),
        Coverage::Any => {
            let tags: HashSet<_> = matrix
                .iter()
                .filter_map(|row| match row[0].as_ref() {
                    Coverage::Ctor(tag, _) => Some(*tag),
                    _ => None,
                })
                .collect();
            let count = match &types[0] {
                Type::Enum(e) => e.variants.len(),
                Type::Tuple(_) => 1,
                _ => 0,
            };
            if count > 0 && tags.len() == count {
                for tag in 0..count {
                    let arity = match &types[0] {
                        Type::Enum(e) => usize::from(e.variants[tag].payload.is_some()),
                        Type::Tuple(t) => t.elements.len(),
                        _ => unreachable!(),
                    };
                    if specialized(tag, vec![any(); arity], budget)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            } else {
                let rows: Vec<_> = matrix
                    .iter()
                    .filter(|row| matches!(row[0].as_ref(), Coverage::Any))
                    .map(|row| row[1..].to_vec())
                    .collect();
                useful(&rows, &query[1..], &types[1..], budget, span, depth + 1)
            }
        }
    }
}

pub(super) fn validate(
    plan: &FunctionPlan,
    ty: &Type,
    arms: &[MatchArm],
    span: Span,
    types: &TypeRegistry,
) -> Result<Vec<CheckedPattern>, Error> {
    let mut budget = 65_536;
    let mut matrix = Vec::new();
    let mut checked = Vec::new();
    for arm in arms {
        let location = arm.pattern.span();
        if !useful(
            &matrix,
            &[any()],
            std::slice::from_ref(ty),
            &mut budget,
            location,
            0,
        )? {
            return Err(type_error(
                &location,
                ErrorCode::UnreachableCode,
                "Unreachable match arm after complete coverage",
            ));
        }
        let pattern = plan.check_match_pattern(&arm.pattern, ty, types)?;
        let coverage = pattern.coverage();
        if !useful(
            &matrix,
            std::slice::from_ref(&coverage),
            std::slice::from_ref(ty),
            &mut budget,
            location,
            0,
        )? {
            return Err(type_error(
                &location,
                ErrorCode::UnreachableCode,
                "Duplicate match arm or unreachable pattern",
            ));
        }
        matrix.push(vec![coverage]);
        checked.push(pattern);
    }
    let Type::Enum(enumeration) = ty else {
        unreachable!()
    };
    let mut missing = Vec::new();
    for (tag, variant) in enumeration.variants.iter().enumerate() {
        let candidate = Arc::new(Coverage::Ctor(
            tag,
            variant.payload.iter().map(|_| any()).collect(),
        ));
        if useful(
            &matrix,
            &[candidate],
            std::slice::from_ref(ty),
            &mut budget,
            span,
            0,
        )? {
            missing.push(variant.name.as_str());
        }
    }
    if !missing.is_empty() {
        return Err(type_error(
            &span,
            ErrorCode::NonExhaustiveMatch,
            format!(
                "Non-exhaustive match; missing variants or payload cases: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(checked)
}
