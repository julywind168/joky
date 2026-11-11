//! Typed recursive patterns, bounded coverage analysis, and consuming bindings.
use super::*;
use crate::parser::ast::{MatchArm, Pattern, PayloadPattern};
use std::collections::{BTreeMap, HashSet};

pub(super) struct CheckedPattern {
    pub(super) ty: Type,
    pub(super) kind: Kind,
    pub(super) span: Span,
}
pub(super) enum Kind {
    Sequence(Box<super::sequence_patterns::CheckedSequence>),
    Literal(Constant, u64),
    String(String),
    Range(Box<IntegerRange>),
    Any(Option<String>),
    Tuple(Vec<CheckedPattern>),
    Variant {
        tag: usize,
        payload: Option<Box<CheckedPattern>>,
    },
    /// Declared field indices in pattern order; the type carries the record.
    Record(Vec<(usize, CheckedPattern)>),
    Or(Vec<CheckedPattern>),
}

pub(super) struct IntegerRange {
    start: Constant,
    end: Constant,
    inclusive: bool,
    lower: i128,
    upper: i128,
}

fn range_endpoint(literal: &Literal, ty: &Type, span: Span) -> Result<(Constant, i128), Error> {
    if let Literal::UInt8(value) = literal {
        return if *ty == Type::UInt8 {
            Ok((Constant::UInt8(*value), i128::from(*value)))
        } else {
            Err(type_error(
                &span,
                ErrorCode::InvalidPattern,
                format!("Byte literal range bounds require UInt8, got {ty}"),
            ))
        };
    }
    let Literal::Integer {
        magnitude,
        negative,
    } = literal
    else {
        return Err(type_error(
            &span,
            ErrorCode::InvalidPattern,
            "Range patterns require integer literal bounds",
        ));
    };
    let (_, constant) = integer_literal(*magnitude, *negative, Some(ty), span)?;
    let value = i128::from(*magnitude);
    Ok((constant, if *negative { -value } else { value }))
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

    pub(super) fn alternatives(&self) -> &[Self] {
        match &self.kind {
            Kind::Or(alternatives) => alternatives,
            _ => std::slice::from_ref(self),
        }
    }

    pub(super) fn hints(&self, vars: &mut HashMap<String, Binding>, borrowed: Option<bool>) {
        self.visit_bindings(borrowed, &mut |name, ty, _| {
            if let Some(ty) = ty {
                vars.insert(name.to_owned(), payload_binding(&ty, 0));
            }
        });
    }

    fn visit_bindings(
        &self,
        borrowed: Option<bool>,
        visit: &mut impl FnMut(&str, Option<Type>, bool),
    ) {
        match &self.kind {
            Kind::Any(Some(name)) => {
                let loan = borrowed.filter(|&mutable| mutable || self.ty.is_owner());
                let ty = loan.map_or_else(
                    || self.ty.clone(),
                    |mutable| Type::reference(self.ty.clone(), mutable),
                );
                visit(name, Some(ty), loan.is_some());
            }
            Kind::Any(None) | Kind::Literal(..) | Kind::String(_) | Kind::Range(_) => {}
            Kind::Or(alternatives) => alternatives[0].visit_bindings(borrowed, visit),
            Kind::Sequence(p) => {
                for child in p.prefix.iter().chain(&p.suffix) {
                    child.visit_bindings(borrowed, visit);
                }
                if let Some(Some(name)) = &p.rest {
                    visit(name, p.rest_type(&self.ty, borrowed), borrowed.is_some());
                }
            }
            Kind::Tuple(elements) => elements
                .iter()
                .for_each(|p| p.visit_bindings(borrowed, visit)),
            Kind::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    payload.visit_bindings(borrowed, visit);
                }
            }
            Kind::Record(fields) => fields
                .iter()
                .for_each(|(_, p)| p.visit_bindings(borrowed, visit)),
        }
    }

    fn binding_signature(&self, borrowed: Option<bool>) -> BTreeMap<String, (Option<Type>, bool)> {
        let mut bindings = BTreeMap::new();
        self.visit_bindings(borrowed, &mut |name, ty, loan| {
            bindings.insert(name.to_owned(), (ty, loan));
        });
        bindings
    }

    pub(super) fn validate_alternative_bindings(
        &self,
        borrowed: Option<bool>,
    ) -> Result<(), Error> {
        let Kind::Or(alternatives) = &self.kind else {
            return Ok(());
        };
        if !alternatives.iter().any(Self::binds) {
            return Ok(());
        }
        let first = alternatives[0].binding_signature(borrowed);
        for alternative in &alternatives[1..] {
            let bindings = alternative.binding_signature(borrowed);
            if first.keys().ne(bindings.keys()) {
                return Err(type_error(
                    &alternative.span,
                    ErrorCode::InvalidPattern,
                    "Match alternatives must bind the same names",
                ));
            }
            if let Some((name, _)) = first
                .iter()
                .find(|(name, binding)| bindings.get(*name) != Some(*binding))
            {
                return Err(type_error(
                    &alternative.span,
                    ErrorCode::InvalidPattern,
                    format!("Binding `{name}` must have the same type and binding mode in every match alternative"),
                ));
            }
        }
        Ok(())
    }
    fn storage_children(&self) -> Result<Vec<(&Self, usize)>, Error> {
        use crate::memory_layout::MemoryLayoutShape;
        if matches!(
            self.kind,
            Kind::Any(_)
                | Kind::Literal(..)
                | Kind::String(_)
                | Kind::Range(_)
                | Kind::Sequence(_)
                | Kind::Or(_)
        ) {
            return Ok(Vec::new());
        }
        // Templates are checked before they have byte layouts; their IR is
        // discarded and rebuilt with concrete offsets for every specialization.
        if self
            .ty
            .scalar_types()
            .iter()
            .any(|ty| matches!(ty, Type::Parameter(_) | Type::Projection(_)))
        {
            return Ok(match &self.kind {
                Kind::Tuple(elements) => elements.iter().map(|p| (p, 0)).collect(),
                Kind::Variant { payload, .. } => payload.iter().map(|p| (p.as_ref(), 0)).collect(),
                Kind::Record(fields) => fields.iter().map(|(_, p)| (p, 0)).collect(),
                Kind::Any(_)
                | Kind::Literal(..)
                | Kind::String(_)
                | Kind::Range(_)
                | Kind::Or(_)
                | Kind::Sequence(_) => {
                    unreachable!()
                }
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

    pub(super) fn binds(&self) -> bool {
        match &self.kind {
            Kind::Literal(..) | Kind::String(_) | Kind::Range(_) => false,
            Kind::Or(alternatives) => alternatives[0].binds(),
            Kind::Sequence(p) => {
                p.rest.as_ref().is_some_and(Option::is_some)
                    || p.prefix.iter().chain(&p.suffix).any(Self::binds)
            }
            Kind::Any(name) => name.is_some(),
            Kind::Tuple(elements) => elements.iter().any(Self::binds),
            Kind::Variant { payload, .. } => payload.as_ref().is_some_and(|p| p.binds()),
            Kind::Record(fields) => fields.iter().any(|(_, p)| p.binds()),
        }
    }
    fn coverage(&self) -> Arc<Coverage> {
        Arc::new(match &self.kind {
            Kind::Literal(_, bits) => Coverage::Literal(*bits),
            Kind::String(text) => Coverage::String(text.clone()),
            Kind::Range(range) => Coverage::Interval(range.lower, range.upper),
            Kind::Sequence(p) => Coverage::Sequence {
                prefix: p.prefix.iter().map(Self::coverage).collect(),
                suffix: p.suffix.iter().map(Self::coverage).collect(),
                rest: p.rest.is_some(),
            },
            Kind::Any(_) => Coverage::Any,
            // Record patterns are irrefutable for their type.
            Kind::Record(_) => Coverage::Any,
            Kind::Tuple(elements) => {
                Coverage::Ctor(0, elements.iter().map(Self::coverage).collect())
            }
            Kind::Variant { tag, payload } => {
                Coverage::Ctor(*tag, payload.iter().map(|p| p.coverage()).collect())
            }
            Kind::Or(_) => unreachable!("alternatives are separate coverage rows"),
        })
    }
}

impl FunctionPlan {
    pub(super) fn check_match_pattern(
        &self,
        pattern: &Pattern,
        ty: &Type,
        borrowed: Option<bool>,
        types: &TypeRegistry,
    ) -> Result<CheckedPattern, Error> {
        let Pattern::Or { alternatives, span } = pattern else {
            return self.check_match_alternative(pattern, ty, types);
        };
        if alternatives.len() < 2 {
            return Err(type_error(
                span,
                ErrorCode::InvalidPattern,
                "Expected at least two alternatives",
            ));
        }
        let alternatives = alternatives
            .iter()
            .map(|alternative| self.check_match_alternative(alternative, ty, types))
            .collect::<Result<Vec<_>, _>>()?;
        let pattern = CheckedPattern::nested(ty.clone(), Kind::Or(alternatives), *span);
        pattern.validate_alternative_bindings(borrowed)?;
        Ok(pattern)
    }

    fn check_match_alternative(
        &self,
        pattern: &Pattern,
        ty: &Type,
        types: &TypeRegistry,
    ) -> Result<CheckedPattern, Error> {
        let mut names = HashSet::new();
        match pattern {
            Pattern::Value(p) => self.check_payload_pattern(p, ty, &mut names, types),
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
            Pattern::Or { span, .. } => Err(type_error(
                span,
                ErrorCode::InvalidPattern,
                "Nested match alternatives are not supported",
            )),
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
    pub(super) fn check_payload_pattern(
        &self,
        pattern: &PayloadPattern,
        ty: &Type,
        names: &mut HashSet<String>,
        types: &TypeRegistry,
    ) -> Result<CheckedPattern, Error> {
        let span = pattern.span();
        let kind = match pattern {
            PayloadPattern::Sequence(p) => {
                return self.check_sequence_pattern(
                    p,
                    ty,
                    names,
                    types,
                    Self::check_payload_pattern,
                )
            }
            PayloadPattern::Range {
                start,
                end,
                inclusive,
                ..
            } => {
                if ty.integer().is_none() || *ty == Type::Rune {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        "Range patterns require an integer scrutinee",
                    ));
                }
                let (start, lower) = range_endpoint(start, ty, span)?;
                let (end, last) = range_endpoint(end, ty, span)?;
                let upper = if *inclusive { last } else { last - 1 };
                if lower > upper {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        "Range pattern is empty or reversed",
                    ));
                }
                Kind::Range(Box::new(IntegerRange {
                    start,
                    end,
                    inclusive: *inclusive,
                    lower,
                    upper,
                }))
            }
            PayloadPattern::Literal {
                literal: Literal::String(text),
                ..
            } if *ty == Type::String => Kind::String(text.clone()),
            PayloadPattern::Literal { literal, .. } => {
                let (actual, constant, bits) = match literal {
                    Literal::Integer {
                        magnitude,
                        negative,
                    } if ty.integer().is_some() && *ty != Type::Rune => {
                        let (actual, constant) =
                            integer_literal(*magnitude, *negative, Some(ty), span)?;
                        (
                            actual,
                            constant,
                            if *negative {
                                magnitude.wrapping_neg()
                            } else {
                                *magnitude
                            },
                        )
                    }
                    Literal::Bool(value) => (Type::Bool, Constant::Bool(*value), u64::from(*value)),
                    Literal::Rune(value) => (Type::Rune, Constant::Rune(*value), u64::from(*value)),
                    Literal::UInt8(value) => {
                        (Type::UInt8, Constant::UInt8(*value), u64::from(*value))
                    }
                    _ => {
                        return Err(type_error(
                            &span,
                            ErrorCode::InvalidPattern,
                            format!("Literal pattern is incompatible with {ty}"),
                        ))
                    }
                };
                if actual != *ty {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        format!("Literal pattern expects {actual}, got {ty}"),
                    ));
                }
                Kind::Literal(constant, bits)
            }
            PayloadPattern::Discard { .. } => Kind::Any(None),
            PayloadPattern::Name { name, .. } => {
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
                resolved,
                type_args,
                fields,
                rest,
                ..
            } => {
                let record = self.resolve_record_pattern(
                    name,
                    resolved.as_deref(),
                    type_args,
                    ty,
                    span,
                    types,
                )?;
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

    pub(super) fn test_match_arm(
        &mut self,
        pattern: &CheckedPattern,
        value: &Slot,
        borrowed: bool,
        failure: Option<usize>,
    ) -> Result<Option<Slot>, Error> {
        let (last, preceding) = pattern.alternatives().split_last().unwrap();
        let choice = (!preceding.is_empty() && pattern.binds())
            .then(|| self.slot(Type::UInt64))
            .transpose()?;
        if failure.is_none() && choice.is_none() {
            return Ok(None);
        }
        if preceding.is_empty() {
            self.test_match_alternative(last, value, borrowed, failure.unwrap())?;
            return Ok(None);
        }
        let matched = self.label_count;
        self.label_count += 1;
        for (index, alternative) in preceding.iter().enumerate() {
            let next = self.label_count;
            self.label_count += 1;
            self.test_match_alternative(alternative, value, borrowed, next)?;
            if let Some(choice) = &choice {
                self.emit(Op::Literal(choice.clone(), Constant::UInt64(index as u64)));
            }
            self.emit(Op::Jump(matched));
            self.emit(Op::Label(next));
        }
        if let Some(failure) = failure {
            self.test_match_alternative(last, value, borrowed, failure)?;
        }
        self.current_span = pattern.span;
        if let Some(choice) = &choice {
            self.emit(Op::Literal(
                choice.clone(),
                Constant::UInt64(preceding.len() as u64),
            ));
        }
        self.emit(Op::Label(matched));
        Ok(choice)
    }

    fn test_match_alternative(
        &mut self,
        pattern: &CheckedPattern,
        value: &Slot,
        borrowed: bool,
        failure: usize,
    ) -> Result<(), Error> {
        if borrowed {
            self.borrowed_pattern_tests(pattern, value, 0, failure)
        } else if value.ty.is_place() && value.ty.is_owner() {
            // Reading an owning field from a place moves it. Test through a
            // view so a failed alternative leaves the entire input available.
            let root = self.sequence_value_reference(value, false)?;
            self.borrowed_pattern_tests(pattern, &root, 0, failure)
        } else {
            self.match_pattern_tests(pattern, value.index, failure)
        }
    }

    pub(super) fn bind_match_arm(
        &mut self,
        pattern: &CheckedPattern,
        value: &Slot,
        borrowed: bool,
        choice: Option<&Slot>,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        if !pattern.binds() {
            return Ok(());
        }
        let Some(choice) = choice else {
            return self.bind_match_alternative(pattern, value, borrowed, vars);
        };

        // Give the selected payloads common storage before compiling the guard
        // or body. An unselected projection is never borrowed or moved.
        let joined = self.label_count;
        self.label_count += 1;
        let alternatives = pattern.alternatives();
        let mut slots = BTreeMap::new();
        for (index, alternative) in alternatives.iter().enumerate() {
            self.current_span = alternative.span;
            let next = if index + 1 < alternatives.len() {
                let next = self.label_count;
                self.label_count += 1;
                let ordinal = self.pattern_u64(index as u64)?;
                self.pattern_compare(choice.clone(), ordinal, BinOp::Eq, next)?;
                Some(next)
            } else {
                None
            };

            self.local_scopes.push(Vec::new());
            let mut bindings = HashMap::new();
            self.bind_match_alternative(alternative, value, borrowed, &mut bindings)?;
            if index == 0 {
                for (name, binding) in bindings.into_iter().collect::<BTreeMap<_, _>>() {
                    let slot = self.snapshot(&binding.slot)?;
                    self.escape_owner(&Flow::Value(slot.clone()));
                    slots.insert(name, slot);
                }
            } else {
                for (name, dst) in &slots {
                    if let (Some(dst), Some(src)) = (dst, &bindings[name].slot) {
                        self.emit(Op::Copy(dst.clone(), src.clone()));
                    }
                }
            }
            self.end_local_scope();
            if let Some(next) = next {
                self.emit(Op::Jump(joined));
                self.emit(Op::Label(next));
            }
        }
        self.current_span = pattern.span;
        self.emit(Op::Label(joined));
        for (name, slot) in slots {
            let binding = self.local_binding(
                &name,
                Binding {
                    slot,
                    mutable: false,
                    place: None,
                },
            )?;
            vars.insert(name, binding);
        }
        Ok(())
    }

    fn bind_match_alternative(
        &mut self,
        pattern: &CheckedPattern,
        value: &Slot,
        borrowed: bool,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        if borrowed {
            self.bind_borrowed_pattern(pattern, value, 0, &mut Vec::new(), false, vars)
        } else {
            self.bind_match_pattern(pattern, Some(value.clone()), None, vars)
        }
    }

    pub(super) fn match_pattern_tests(
        &mut self,
        pattern: &CheckedPattern,
        index: usize,
        failure: usize,
    ) -> Result<(), Error> {
        self.current_span = pattern.span;
        if let Kind::Sequence(p) = &pattern.kind {
            let value = Slot {
                index,
                ty: pattern.ty.clone(),
            };
            let reference = self.sequence_value_reference(&value, false)?;
            return self.sequence_tests(p, &reference, failure);
        }
        if pattern.ty.is_place() {
            let base = Slot {
                index,
                ty: pattern.ty.clone(),
            };
            if let Kind::Variant { tag, .. } = &pattern.kind {
                let actual = self.place_read(&base, 0, Type::Int32)?.unwrap();
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
            for (child, offset) in pattern.storage_children()? {
                if !matches!(child.kind, Kind::Any(_)) {
                    if let Some(value) = self.place_read(&base, offset, child.ty.clone())? {
                        self.match_pattern_tests(child, value.index, failure)?;
                    }
                }
            }
            return Ok(());
        }
        match &pattern.kind {
            Kind::Sequence(_) | Kind::Or(_) => unreachable!(),
            Kind::String(text) => {
                let actual = Slot {
                    index,
                    ty: Type::String,
                };
                let reference = self.sequence_value_reference(&actual, false)?;
                self.string_pattern_test(reference, text, failure)?;
            }
            Kind::Range(range) => {
                let actual = Slot {
                    index,
                    ty: pattern.ty.clone(),
                };
                let start = self.slot(pattern.ty.clone())?;
                self.emit(Op::Literal(start.clone(), range.start.clone()));
                self.pattern_compare(actual.clone(), start, BinOp::Ge, failure)?;
                let end = self.slot(pattern.ty.clone())?;
                self.emit(Op::Literal(end.clone(), range.end.clone()));
                self.pattern_compare(
                    actual,
                    end,
                    if range.inclusive {
                        BinOp::Le
                    } else {
                        BinOp::Lt
                    },
                    failure,
                )?;
            }
            Kind::Literal(constant, _) => {
                self.literal_pattern_test(
                    Slot {
                        index,
                        ty: pattern.ty.clone(),
                    },
                    constant.clone(),
                    failure,
                )?;
            }
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

    pub(super) fn pattern_reference(
        &mut self,
        root: &Slot,
        ty: &Type,
        offset: usize,
        mutable: bool,
        path: &[usize],
    ) -> Result<Slot, Error> {
        let dst = self.slot(Type::reference(ty.clone(), mutable))?;
        let target = self.pointee(
            &root.ty.reference_type().unwrap().target,
            &self.type_registry,
        )?;
        if target.is_owner() && !target.is_inline_owner() && ty.is_owner() {
            // A one-field owner wrapper borrows the handle itself, just like
            // ordinary field access. It is not an address containing a handle.
            // Slice partitions still need their disjoint PatternProject loans.
            debug_assert_eq!(offset, 0);
            self.emit(Op::Reference {
                dst: dst.clone(),
                src: root.clone(),
                moving: false,
            });
        } else {
            self.emit(Op::PatternProject {
                dst: dst.clone(),
                src: root.clone(),
                offset,
                path: path.to_vec(),
            });
        }
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
        if let Kind::Sequence(p) = &pattern.kind {
            let root = self.sequence_root(root, &pattern.ty, offset, false, &[])?;
            return self.sequence_tests(p, &root, failure);
        }
        if let Kind::String(text) = &pattern.kind {
            let reference = self.pattern_reference(root, &Type::String, offset, false, &[])?;
            return self.string_pattern_test(reference, text, failure);
        }
        if matches!(&pattern.kind, Kind::Literal(..) | Kind::Range(_)) {
            let reference = self.pattern_reference(root, &pattern.ty, offset, false, &[])?;
            let actual = self.read_local(reference)?.unwrap();
            return self.match_pattern_tests(pattern, actual.index, failure);
        }
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
        if let Kind::Sequence(p) = &pattern.kind {
            let mutable = root.ty.reference_type().unwrap().mutable;
            let root = self.sequence_root(root, &pattern.ty, offset, mutable, path)?;
            return self.bind_sequence_borrowed(p, &root, binding_mutable, vars);
        }
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
        self.bind_value_pattern(pattern, value, parent, vars, false)
    }

    pub(super) fn bind_value_pattern(
        &mut self,
        pattern: &CheckedPattern,
        value: Option<Slot>,
        parent: Option<&Slot>,
        vars: &mut HashMap<String, Binding>,
        snapshot_copies: bool,
    ) -> Result<(), Error> {
        if !pattern.binds() {
            return Ok(());
        }
        self.current_span = pattern.span;
        if let Kind::Sequence(p) = &pattern.kind {
            let mut value = value.unwrap();
            if let Some(parent) = parent.filter(|_| value.ty.is_owner()) {
                let dst = self.slot(value.ty.clone())?;
                self.emit(Op::Buffer(BufferOp::Detach {
                    dst: dst.clone(),
                    src: value,
                    owner: parent.clone(),
                }));
                value = dst;
            }
            return self.bind_sequence_value(p, &value, vars, snapshot_copies);
        }
        if pattern.ty.is_place() && !matches!(pattern.kind, Kind::Any(_)) {
            let base = value.as_ref().unwrap();
            for (child, offset) in pattern.storage_children()? {
                if child.binds() {
                    let value = self.place_read(base, offset, child.ty.clone())?;
                    self.bind_value_pattern(child, value, None, vars, snapshot_copies)?;
                }
            }
            return Ok(());
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
            Kind::Literal(..)
            | Kind::String(_)
            | Kind::Range(_)
            | Kind::Sequence(_)
            | Kind::Or(_) => {}
            Kind::Any(Some(name)) => {
                let value = if snapshot_copies && !pattern.ty.is_owner() {
                    // A declaration outlives the tested scrutinee. A Copy
                    // binding must have its own storage before that value ends.
                    self.snapshot(&value)?
                } else {
                    value
                };
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
                    self.bind_value_pattern(element, child, parent, vars, snapshot_copies)?;
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
                    self.bind_value_pattern(
                        payload,
                        child,
                        slot.ty.is_owner().then_some(slot),
                        vars,
                        snapshot_copies,
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
                    self.bind_value_pattern(child, child_slot, parent, vars, snapshot_copies)?;
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
    Literal(u64),
    String(String),
    Interval(i128, i128),
    Sequence {
        prefix: Vec<Arc<Coverage>>,
        suffix: Vec<Arc<Coverage>>,
        rest: bool,
    },
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
    // Length classes stop once all prefixes and suffixes are disjoint and
    // every exact-length pattern has been passed. Larger lengths are equivalent.
    if let Some(element) = super::arrays::element_type(&types[0]) {
        let mut prefix = 0;
        let mut suffix = 0;
        let mut exact = 0;
        for p in matrix
            .iter()
            .map(|r| &r[0])
            .chain(std::iter::once(&query[0]))
        {
            if let Coverage::Sequence {
                prefix: a,
                suffix: b,
                rest,
            } = p.as_ref()
            {
                prefix = prefix.max(a.len());
                suffix = suffix.max(b.len());
                if !rest {
                    exact = exact.max(a.len() + b.len() + 1);
                }
            }
        }
        let limit = exact.max(prefix + suffix);
        if limit > *budget {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                "Sequence coverage exceeds analysis budget",
            ));
        }
        let lengths = match &types[0] {
            Type::FixedArray(a) => match a.length {
                crate::core::types::ConstArgument::Value(n) => vec![n as usize],
                crate::core::types::ConstArgument::Parameter(_) => vec![limit],
            },
            _ => (0..=limit).collect(),
        };
        for length in lengths {
            let mut indices: Vec<_> = (0..prefix.min(length)).collect();
            indices.extend(length.saturating_sub(suffix)..length);
            indices.sort_unstable();
            indices.dedup();
            let expand = |p: &Coverage| -> Option<Vec<Arc<Coverage>>> {
                match p {
                    Coverage::Any => Some(vec![any(); indices.len()]),
                    Coverage::Sequence {
                        prefix,
                        suffix,
                        rest,
                    } => {
                        let count = prefix.len() + suffix.len();
                        if length < count || !rest && length != count {
                            return None;
                        }
                        Some(
                            indices
                                .iter()
                                .map(|&index| {
                                    if index < prefix.len() {
                                        prefix[index].clone()
                                    } else if index >= length - suffix.len() {
                                        suffix[index - (length - suffix.len())].clone()
                                    } else {
                                        any()
                                    }
                                })
                                .collect(),
                        )
                    }
                    _ => unreachable!("sequence coverage"),
                }
            };
            let Some(mut candidate) = expand(&query[0]) else {
                continue;
            };
            candidate.extend_from_slice(&query[1..]);
            let rows: Vec<_> = matrix
                .iter()
                .filter_map(|row| {
                    let mut fields = expand(&row[0])?;
                    fields.extend_from_slice(&row[1..]);
                    Some(fields)
                })
                .collect();
            let mut fields = vec![element.clone(); indices.len()];
            fields.extend_from_slice(&types[1..]);
            if useful(&rows, &candidate, &fields, budget, span, depth + 1)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if let Some(integer) = types[0].integer().filter(|_| {
        types[0] != Type::Rune
            && (matches!(query[0].as_ref(), Coverage::Interval(..))
                || matrix
                    .iter()
                    .any(|row| matches!(row[0].as_ref(), Coverage::Interval(..))))
    }) {
        let (minimum, maximum) = if integer.signed() {
            let half = 1_i128 << (integer.bits() - 1);
            (-half, half - 1)
        } else {
            (0, (1_i128 << integer.bits()) - 1)
        };
        let interval = |pattern: &Coverage| match pattern {
            Coverage::Any => (minimum, maximum),
            Coverage::Interval(lower, upper) => (*lower, *upper),
            Coverage::Literal(bits) => {
                let value = if integer.signed() {
                    *bits as i64 as i128
                } else {
                    i128::from(*bits)
                };
                (value, value)
            }
            _ => unreachable!("integer pattern coverage"),
        };
        let (lower, upper) = interval(&query[0]);
        // Every interval is constant on each cell, preserving correlations with
        // the remaining columns without enumerating any integer values. i128
        // also represents UInt64::MAX + 1 for the exclusive analysis boundary.
        let mut cuts = vec![lower, upper + 1];
        for row in matrix {
            let (start, end) = interval(&row[0]);
            if start <= upper && end >= lower {
                cuts.push(start.max(lower));
                cuts.push((end + 1).min(upper + 1));
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for cell in cuts.windows(2) {
            // Charge the scan even when specialization discards most rows.
            *budget = budget.checked_sub(matrix.len()).ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::ResourceLimit,
                    "Match coverage analysis exceeds 65536 steps",
                )
            })?;
            let rows: Vec<_> = matrix
                .iter()
                .filter(|row| {
                    let (start, end) = interval(&row[0]);
                    start <= cell[0] && cell[0] <= end
                })
                .map(|row| row[1..].to_vec())
                .collect();
            if useful(&rows, &query[1..], &types[1..], budget, span, depth + 1)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if types[0].integer().is_some() || matches!(types[0], Type::Bool | Type::Rune) {
        let specialize = |value: Option<u64>, budget: &mut usize| {
            let rows: Vec<_> = matrix
                .iter()
                .filter(|row| match row[0].as_ref() {
                    Coverage::Any => true,
                    Coverage::Literal(actual) => Some(*actual) == value,
                    _ => false,
                })
                .map(|row| row[1..].to_vec())
                .collect();
            useful(&rows, &query[1..], &types[1..], budget, span, depth + 1)
        };
        if let Coverage::Literal(value) = query[0].as_ref() {
            return specialize(Some(*value), budget);
        }
        let values: HashSet<_> = matrix
            .iter()
            .filter_map(|row| match row[0].as_ref() {
                Coverage::Literal(v) => Some(*v),
                _ => None,
            })
            .collect();
        let count: u128 = match types[0] {
            Type::Bool => 2,
            Type::Int8 | Type::UInt8 => 1 << 8,
            Type::Int16 | Type::UInt16 => 1 << 16,
            Type::Int32 | Type::UInt32 => 1 << 32,
            Type::Int64 | Type::UInt64 => 1 << 64,
            Type::Rune => 0x110000 - 0x800,
            _ => unreachable!(),
        };
        if (values.len() as u128) < count && specialize(None, budget)? {
            return Ok(true);
        }
        for value in values {
            if specialize(Some(value), budget)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if types[0] == Type::String {
        let specialize = |value: Option<&str>, budget: &mut usize| {
            let rows: Vec<_> = matrix
                .iter()
                .filter(|row| match row[0].as_ref() {
                    Coverage::Any => true,
                    Coverage::String(actual) => Some(actual.as_str()) == value,
                    _ => false,
                })
                .map(|row| row[1..].to_vec())
                .collect();
            useful(&rows, &query[1..], &types[1..], budget, span, depth + 1)
        };
        if let Coverage::String(value) = query[0].as_ref() {
            return specialize(Some(value), budget);
        }
        // A finite set of strings cannot exhaust the String domain.
        if specialize(None, budget)? {
            return Ok(true);
        }
        let values: HashSet<_> = matrix
            .iter()
            .filter_map(|row| match row[0].as_ref() {
                Coverage::String(value) => Some(value.as_str()),
                _ => None,
            })
            .collect();
        for value in values {
            if specialize(Some(value), budget)? {
                return Ok(true);
            }
        }
        return Ok(false);
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
        Coverage::Literal(_)
        | Coverage::String(_)
        | Coverage::Interval(..)
        | Coverage::Sequence { .. } => {
            unreachable!("typed pattern coverage")
        }
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

fn validate_integer_literals(
    plan: &FunctionPlan,
    ty: &Type,
    borrowed: Option<bool>,
    domain: u128,
    arms: &[MatchArm],
    span: Span,
    types: &TypeRegistry,
) -> Result<Vec<CheckedPattern>, Error> {
    let mut covered = HashSet::new();
    let mut complete = false;
    let mut checked = Vec::new();
    let mut remaining = 65_536usize;
    for arm in arms {
        let location = arm.pattern.span();
        if complete {
            return Err(type_error(
                &location,
                ErrorCode::UnreachableCode,
                "Unreachable match arm after complete coverage",
            ));
        }
        remaining = remaining
            .checked_sub(arm.pattern.alternatives().len())
            .ok_or_else(|| {
                type_error(
                    &location,
                    ErrorCode::ResourceLimit,
                    "Match coverage analysis exceeds 65536 steps",
                )
            })?;
        let pattern = plan.check_match_pattern(&arm.pattern, ty, borrowed, types)?;
        for alternative in pattern.alternatives() {
            let useful = !complete
                && match &alternative.kind {
                    Kind::Literal(_, bits) => covered.insert(*bits),
                    Kind::Any(_) => true,
                    _ => unreachable!("scalar literal or binding"),
                };
            if !useful {
                return Err(type_error(
                    &alternative.span,
                    ErrorCode::UnreachableCode,
                    "Duplicate match arm or unreachable pattern",
                ));
            }
            complete = matches!(alternative.kind, Kind::Any(_)) || covered.len() as u128 == domain;
        }
        if arm.guard.is_some() {
            for alternative in pattern.alternatives() {
                if let Kind::Literal(_, bits) = &alternative.kind {
                    covered.remove(bits);
                }
            }
            complete = false;
        }
        checked.push(pattern);
    }
    if !complete {
        return Err(type_error(
            &span,
            ErrorCode::NonExhaustiveMatch,
            "Non-exhaustive match; missing sequence lengths or element values",
        ));
    }
    Ok(checked)
}

pub(super) fn validate(
    plan: &FunctionPlan,
    ty: &Type,
    borrowed: Option<bool>,
    arms: &[MatchArm],
    span: Span,
    types: &TypeRegistry,
) -> Result<Vec<CheckedPattern>, Error> {
    // One integer column has no correlations with other columns. For literal
    // arms, its covered constructors are precisely the distinct checked bits.
    if let Some(integer) = ty.integer().filter(|_| *ty != Type::Rune) {
        if arms
            .iter()
            .flat_map(|arm| arm.pattern.alternatives())
            .all(|pattern| match pattern {
                Pattern::Wildcard { .. } => true,
                Pattern::Value(p) => matches!(
                    p.as_ref(),
                    PayloadPattern::Literal { .. }
                        | PayloadPattern::Discard { .. }
                        | PayloadPattern::Name { .. }
                ),
                Pattern::Variant { .. } | Pattern::Or { .. } => false,
            })
        {
            return validate_integer_literals(
                plan,
                ty,
                borrowed,
                1u128 << integer.bits(),
                arms,
                span,
                types,
            );
        }
    }
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
        let pattern = plan.check_match_pattern(&arm.pattern, ty, borrowed, types)?;
        let covered = matrix.len();
        for alternative in pattern.alternatives() {
            let coverage = alternative.coverage();
            if !useful(
                &matrix,
                std::slice::from_ref(&coverage),
                std::slice::from_ref(ty),
                &mut budget,
                alternative.span,
                0,
            )? {
                return Err(type_error(
                    &alternative.span,
                    ErrorCode::UnreachableCode,
                    "Duplicate match arm or unreachable pattern",
                ));
            }
            matrix.push(vec![coverage]);
        }
        if arm.guard.is_some() {
            matrix.truncate(covered);
        }
        checked.push(pattern);
    }
    let Type::Enum(enumeration) = ty else {
        if useful(
            &matrix,
            &[any()],
            std::slice::from_ref(ty),
            &mut budget,
            span,
            0,
        )? {
            return Err(type_error(
                &span,
                ErrorCode::NonExhaustiveMatch,
                "Non-exhaustive match; missing sequence lengths or element values",
            ));
        }
        return Ok(checked);
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
