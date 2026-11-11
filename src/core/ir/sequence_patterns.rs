//! Sequence tests precede all moves; successful projections carry disjoint paths.
use super::patterns::{CheckedPattern, Kind};
use super::*;
use crate::core::types::{ConstArgument, FixedArrayType};
use crate::parser::ast::{BufferIntrinsic as BI, SequencePattern};
use std::collections::HashSet;

pub(super) struct CheckedSequence {
    pub prefix: Vec<CheckedPattern>,
    pub suffix: Vec<CheckedPattern>,
    pub rest: Option<Option<String>>,
    element: Type,
    span: Span,
}
impl CheckedSequence {
    pub(super) fn rest_type(&self, ty: &Type, borrowed: Option<bool>) -> Option<Type> {
        if let Some(mutable) = borrowed {
            return Some(Type::reference(
                Type::BufferSlice(Arc::new(self.element.clone())),
                mutable,
            ));
        }
        match ty {
            Type::FixedArray(a) => {
                let ConstArgument::Value(length) = a.length else {
                    return None;
                };
                Some(Type::FixedArray(Arc::new(FixedArrayType {
                    element: self.element.clone(),
                    length: ConstArgument::Value(
                        length - (self.prefix.len() + self.suffix.len()) as u64,
                    ),
                })))
            }
            Type::Record(r) if r.array => Some(ty.clone()),
            _ => None,
        }
    }
}

// Template IR is discarded; concrete specializations rebuild byte offsets.
pub(super) fn element_stride(element: &Type, span: Span) -> Result<usize, Error> {
    use crate::core::types::walk::{TypeEdges, TypeWalk};
    if TypeWalk::new(element, TypeEdges::Fields).any(|ty| {
        matches!(ty, Type::Parameter(_) | Type::Projection(_))
            || matches!(ty, Type::FixedArray(a) if matches!(a.length, ConstArgument::Parameter(_)))
    }) {
        return Ok(0);
    }
    element
        .storage_layout()
        .map(|layout| layout.size)
        .map_err(|e| type_error(&span, ErrorCode::TypeMismatch, e.to_string()))
}

impl FunctionPlan {
    pub(super) fn check_sequence_pattern<P>(
        &self,
        p: &SequencePattern<P>,
        ty: &Type,
        names: &mut HashSet<String>,
        types: &TypeRegistry,
        check: impl Fn(
            &Self,
            &P,
            &Type,
            &mut HashSet<String>,
            &TypeRegistry,
        ) -> Result<CheckedPattern, Error>,
    ) -> Result<CheckedPattern, Error> {
        let element = super::arrays::element_type(ty).ok_or_else(|| {
            type_error(
                &p.span,
                ErrorCode::InvalidPattern,
                format!("Sequence pattern requires an array or slice, got {ty}"),
            )
        })?;
        let count = (p.prefix.len() + p.suffix.len()) as u64;
        if let Type::FixedArray(a) = ty {
            if let ConstArgument::Value(length) = a.length {
                if count > length || p.rest.is_none() && count != length {
                    return Err(type_error(
                        &p.span,
                        ErrorCode::InvalidPattern,
                        format!("Sequence pattern cannot match FixedArray length {length}"),
                    ));
                }
            } else if p.rest.is_none() || count != 0 {
                return Err(type_error(
                    &p.span,
                    ErrorCode::InvalidPattern,
                    "Sequence patterns on a generic fixed length require an unconstrained rest",
                ));
            }
        }
        let prefix = p
            .prefix
            .iter()
            .map(|p| check(self, p, element, names, types))
            .collect::<Result<_, _>>()?;
        let suffix = p
            .suffix
            .iter()
            .map(|p| check(self, p, element, names, types))
            .collect::<Result<_, _>>()?;
        if let Some(Some(name)) = &p.rest {
            if !names.insert(name.clone()) {
                return Err(type_error(
                    &p.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate binding `{name}` in pattern"),
                ));
            }
        }
        Ok(CheckedPattern::nested(
            ty.clone(),
            Kind::Sequence(Box::new(CheckedSequence {
                prefix,
                suffix,
                rest: p.rest.clone(),
                element: element.clone(),
                span: p.span,
            })),
            p.span,
        ))
    }

    pub(super) fn pattern_u64(&mut self, n: u64) -> Result<Slot, Error> {
        let dst = self.slot(Type::UInt64)?;
        self.emit(Op::Literal(dst.clone(), Constant::UInt64(n)));
        Ok(dst)
    }
    pub(super) fn sequence_storage(
        &mut self,
        intrinsic: BI,
        element: &Type,
        args: Vec<Slot>,
        ty: Type,
    ) -> Result<Option<Slot>, Error> {
        // Array arguments can retain only a nominal identity after reference
        // normalization. Storage operations need the complete element layout,
        // just like explicit buffer intrinsics do.
        let element = self.pointee(element, &self.type_registry)?;
        let dst = if ty == Type::Unit {
            None
        } else {
            Some(self.slot(ty)?)
        };
        if let Some(dst) = dst.as_ref().filter(|s| s.ty.is_place()) {
            self.emit(Op::Place(PlaceOp::Allocate(dst.clone())));
        }
        self.emit(Op::Buffer(BufferOp::Storage {
            scratch: None,
            intrinsic,
            element,
            args,
            dst: dst.clone(),
        }));
        if let Some(dst) = dst.as_ref().filter(|s| s.ty.is_owner()) {
            self.register_owner(dst.clone(), None);
        }
        Ok(dst)
    }
    pub(super) fn literal_pattern_test(
        &mut self,
        actual: Slot,
        constant: Constant,
        failure: usize,
    ) -> Result<(), Error> {
        let expected = self.slot(actual.ty.clone())?;
        self.emit(Op::Literal(expected.clone(), constant));
        self.pattern_compare(actual, expected, BinOp::Eq, failure)
    }
    pub(super) fn pattern_compare(
        &mut self,
        left: Slot,
        right: Slot,
        op: BinOp,
        failure: usize,
    ) -> Result<(), Error> {
        let condition = self.slot(Type::Bool)?;
        self.emit(Op::Binary(condition.clone(), op, left, right));
        let success = self.label_count;
        self.label_count += 1;
        self.emit(Op::Branch {
            condition,
            then_label: success,
            else_label: failure,
        });
        self.emit(Op::Label(success));
        Ok(())
    }
    pub(super) fn sequence_value_reference(
        &mut self,
        value: &Slot,
        mutable: bool,
    ) -> Result<Slot, Error> {
        if value.ty.is_owner() {
            let dst = self.slot(Type::reference(value.ty.clone(), mutable))?;
            self.emit(Op::Buffer(BufferOp::Borrow {
                reservation: false,
                dst: dst.clone(),
                src: value.clone(),
                place: None,
            }));
            Ok(dst)
        } else {
            let dst = self.slot(Type::reference(value.ty.clone(), mutable))?;
            self.emit(Op::Local {
                dst: dst.clone(),
                value: Some(value.clone()),
                target: value.ty.clone(),
            });
            self.local_scopes.last_mut().unwrap().push(dst.clone());
            Ok(dst)
        }
    }
    pub(super) fn sequence_root(
        &mut self,
        root: &Slot,
        ty: &Type,
        offset: usize,
        mutable: bool,
        path: &[usize],
    ) -> Result<Slot, Error> {
        if offset == 0
            && root.ty.reference_type().is_some_and(|r| r.target == *ty)
            && path.is_empty()
        {
            // Owner references already point at their header, unlike an owning
            // payload stored in an enum/tuple or in a buffer element.
            let mut root = root.clone();
            root.ty = Type::reference(ty.clone(), mutable);
            Ok(root)
        } else {
            self.pattern_reference(root, ty, offset, mutable, path)
        }
    }
    pub(super) fn sequence_len(&mut self, root: &Slot, element: &Type) -> Result<Slot, Error> {
        match &root.ty.reference_type().unwrap().target {
            Type::FixedArray(a) => {
                let n = match a.length {
                    ConstArgument::Value(n) => n,
                    ConstArgument::Parameter(_) => 0,
                };
                self.pattern_u64(n)
            }
            Type::BufferSlice(_) => Ok(self
                .sequence_storage(BI::SliceLen, element, vec![root.clone()], Type::UInt64)?
                .unwrap()),
            _ => Ok(self
                .sequence_storage(BI::Len, element, vec![root.clone()], Type::UInt64)?
                .unwrap()),
        }
    }
    fn sequence_index(
        &mut self,
        length: &Slot,
        prefix: usize,
        suffix: usize,
        i: usize,
    ) -> Result<Slot, Error> {
        if i < prefix {
            return self.pattern_u64(i as u64);
        }
        let distance = self.pattern_u64((suffix - (i - prefix)) as u64)?;
        let index = self.slot(Type::UInt64)?;
        self.emit(Op::Binary(
            index.clone(),
            BinOp::Sub,
            length.clone(),
            distance,
        ));
        Ok(index)
    }
    pub(super) fn sequence_element(
        &mut self,
        root: &Slot,
        element: &Type,
        index: Slot,
        mutable: bool,
    ) -> Result<Slot, Error> {
        let target = &root.ty.reference_type().unwrap().target;
        if matches!(target, Type::FixedArray(_)) {
            let dst = self.slot(Type::reference(element.clone(), mutable))?;
            self.emit(Op::Place(PlaceOp::View {
                scratch: None,
                reference: root.clone(),
                start: index,
                end: None,
                dst: dst.clone(),
            }));
            Ok(dst)
        } else {
            // Lend the element's storage, including its owner lanes. The normal
            // pattern projector performs any necessary owner-header load.
            let intrinsic = match (matches!(target, Type::BufferSlice(_)), mutable) {
                (false, false) => BI::Borrow,
                (false, true) => BI::BorrowMut,
                (true, false) => BI::SliceBorrow,
                (true, true) => BI::SliceBorrowMut,
            };
            Ok(self
                .sequence_storage(
                    intrinsic,
                    element,
                    vec![root.clone(), index],
                    Type::reference(Type::BufferElement(Arc::new(element.clone())), mutable),
                )?
                .unwrap())
        }
    }
    pub(super) fn sequence_slice(
        &mut self,
        root: &Slot,
        element: &Type,
        start: Slot,
        end: Slot,
        mutable: bool,
    ) -> Result<Slot, Error> {
        let ty = Type::reference(Type::BufferSlice(Arc::new(element.clone())), mutable);
        let target = &root.ty.reference_type().unwrap().target;
        if matches!(target, Type::FixedArray(_)) {
            let dst = self.slot(ty)?;
            self.emit(Op::Place(PlaceOp::View {
                scratch: None,
                reference: root.clone(),
                start,
                end: Some(end),
                dst: dst.clone(),
            }));
            Ok(dst)
        } else {
            let intrinsic = match (matches!(target, Type::BufferSlice(_)), mutable) {
                (false, false) => BI::Slice,
                (false, true) => BI::SliceMut,
                (true, false) => BI::Subslice,
                (true, true) => BI::SubsliceMut,
            };
            Ok(self
                .sequence_storage(intrinsic, element, vec![root.clone(), start, end], ty)?
                .unwrap())
        }
    }
    pub(super) fn sequence_tests(
        &mut self,
        p: &CheckedSequence,
        root: &Slot,
        failure: usize,
    ) -> Result<(), Error> {
        let length = self.sequence_len(root, &p.element)?;
        let minimum = self.pattern_u64((p.prefix.len() + p.suffix.len()) as u64)?;
        self.pattern_compare(
            length.clone(),
            minimum,
            if p.rest.is_some() {
                BinOp::Ge
            } else {
                BinOp::Eq
            },
            failure,
        )?;
        for (i, child) in p.prefix.iter().chain(&p.suffix).enumerate() {
            if matches!(child.kind, Kind::Any(_)) {
                continue;
            }
            let index = self.sequence_index(&length, p.prefix.len(), p.suffix.len(), i)?;
            let reference = self.sequence_element(root, &p.element, index, false)?;
            self.borrowed_pattern_tests(child, &reference, 0, failure)?;
        }
        Ok(())
    }
    fn sequence_partition(
        &mut self,
        root: &Slot,
        element: &Type,
        index: usize,
    ) -> Result<Slot, Error> {
        let r = root.ty.reference_type().unwrap();
        let target = if matches!(r.target, Type::Record(ref record) if record.array) {
            Type::BufferSlice(Arc::new(element.clone()))
        } else {
            r.target.clone()
        };
        self.pattern_reference(root, &target, 0, r.mutable, &[index])
    }
    pub(super) fn bind_sequence_borrowed(
        &mut self,
        p: &CheckedSequence,
        root: &Slot,
        binding_mutable: bool,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        let mutable = root.ty.reference_type().unwrap().mutable;
        let length = self.sequence_len(root, &p.element)?;
        for (i, child) in p.prefix.iter().chain(&p.suffix).enumerate() {
            if !child.binds() {
                continue;
            }
            if mutable && p.element.contains_references() {
                return Err(type_error(
                    &p.span,
                    ErrorCode::TypeMismatch,
                    "Mutable sequence patterns cannot bind reference-bearing elements",
                ));
            }
            let partition = self.sequence_partition(root, &p.element, i)?;
            let index = self.sequence_index(&length, p.prefix.len(), p.suffix.len(), i)?;
            let reference = self.sequence_element(&partition, &p.element, index, mutable)?;
            self.bind_borrowed_pattern(
                child,
                &reference,
                0,
                &mut Vec::new(),
                binding_mutable,
                vars,
            )?;
        }
        if let Some(Some(name)) = &p.rest {
            let partition =
                self.sequence_partition(root, &p.element, p.prefix.len() + p.suffix.len())?;
            let start = self.pattern_u64(p.prefix.len() as u64)?;
            let suffix = self.pattern_u64(p.suffix.len() as u64)?;
            let end = self.slot(Type::UInt64)?;
            self.emit(Op::Binary(end.clone(), BinOp::Sub, length, suffix));
            let value = self.sequence_slice(&partition, &p.element, start, end, mutable)?;
            vars.insert(
                name.clone(),
                Binding {
                    slot: Some(value),
                    mutable: binding_mutable,
                    place: None,
                },
            );
        }
        Ok(())
    }
    pub(super) fn bind_sequence_value(
        &mut self,
        p: &CheckedSequence,
        value: &Slot,
        vars: &mut HashMap<String, Binding>,
        snapshot_copies: bool,
    ) -> Result<(), Error> {
        if let Type::FixedArray(a) = &value.ty {
            let ConstArgument::Value(length) = a.length else {
                return Err(type_error(
                    &p.span,
                    ErrorCode::InvalidPattern,
                    "By-value sequence bindings require a concrete fixed length",
                ));
            };
            let stride = element_stride(&p.element, p.span)?;
            for (i, child) in p.prefix.iter().chain(&p.suffix).enumerate() {
                if !child.binds() {
                    continue;
                }
                let index = if i < p.prefix.len() {
                    i
                } else {
                    length as usize - p.suffix.len() + i - p.prefix.len()
                };
                let child_value = self.place_read(value, index * stride, p.element.clone())?;
                self.bind_value_pattern(child, child_value, None, vars, snapshot_copies)?;
            }
            if let Some(Some(name)) = &p.rest {
                let ty = p.rest_type(&value.ty, None).unwrap();
                let rest = self.place_read(value, p.prefix.len() * stride, ty.clone())?;
                let pattern = CheckedPattern::nested(ty, Kind::Any(Some(name.clone())), p.span);
                self.bind_value_pattern(&pattern, rest, None, vars, snapshot_copies)?;
            }
            return Ok(());
        }
        let root = self.sequence_value_reference(value, true)?;
        // Remove suffixes from the back. Prefix removal rotates the remaining
        // sequence before take_last, preserving the order of the owned rest.
        let mut suffix_values = Vec::new();
        for _ in &p.suffix {
            suffix_values.push(self.sequence_storage(
                BI::TakeLast,
                &p.element,
                vec![root.clone()],
                p.element.clone(),
            )?);
        }
        for child in &p.prefix {
            let length = self.sequence_len(&root, &p.element)?;
            let one = self.pattern_u64(1)?;
            let last = self.slot(Type::UInt64)?;
            self.emit(Op::Binary(last.clone(), BinOp::Sub, length, one.clone()));
            let index = self.pattern_u64(0)?;
            let header = self.label_count;
            self.label_count += 1;
            let exit = self.label_count;
            self.label_count += 1;
            self.emit(Op::Label(header));
            self.pattern_compare(index.clone(), last, BinOp::Lt, exit)?;
            let next = self.slot(Type::UInt64)?;
            self.emit(Op::Binary(next.clone(), BinOp::Add, index.clone(), one));
            self.sequence_storage(
                BI::SwapElements,
                &p.element,
                vec![root.clone(), index.clone(), next.clone()],
                Type::Unit,
            )?;
            self.emit(Op::Copy(index, next));
            self.emit(Op::Jump(header));
            self.emit(Op::Label(exit));
            let child_value = self.sequence_storage(
                BI::TakeLast,
                &p.element,
                vec![root.clone()],
                p.element.clone(),
            )?;
            self.bind_value_pattern(child, child_value, None, vars, snapshot_copies)?;
        }
        for (child, value) in p.suffix.iter().zip(suffix_values.into_iter().rev()) {
            self.bind_value_pattern(child, value, None, vars, snapshot_copies)?;
        }
        if let Some(Some(name)) = &p.rest {
            let pattern =
                CheckedPattern::nested(value.ty.clone(), Kind::Any(Some(name.clone())), p.span);
            self.bind_value_pattern(&pattern, Some(value.clone()), None, vars, snapshot_copies)?;
        }
        Ok(())
    }
}
