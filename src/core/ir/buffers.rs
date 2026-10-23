//! Compiler-owned storage definitions and buffer capability operations.
use super::*;
use crate::parser::ast::BufferIntrinsic;

#[derive(Clone)]

pub(crate) enum BufferOp {
    Storage {
        intrinsic: BufferIntrinsic,
        element: Type,
        args: Vec<Slot>,
        dst: Option<Slot>,
    },
    /// Exchange borrowed storage with a registered temporary owner, without dropping either.
    Swap {
        reference: Slot,
        value: Slot,
        place: Option<Slot>,
    },
    /// Exchange two disjoint checked places without moving their cleanup roots.
    SwapReferences {
        left: Slot,
        right: Slot,
        target: Type,
    },
    /// Atomically transfer the whole value and its cleanup group.
    Move { dst: Slot, src: Slot },
    /// Move one field while consuming the entire source record.
    Extract { dst: Slot, src: Slot, owner: Slot },
    /// Detach one field of an irrefutable record destructuring. Same runtime
    /// transfer as Extract; reference analysis consumes only the selected
    /// subtree, and the remainder's responsibility ends at an explicit End.
    Detach { dst: Slot, src: Slot, owner: Slot },
    /// Drop an old field, move its replacement, and update stable storage atomically.
    Replace {
        dst: Slot,
        src: Slot,
        owner: Slot,
        place: Slot,
        path: Vec<String>,
    },
    Borrow {
        dst: Slot,
        src: Slot,
        /// Mixed records lend stable inline storage, not the buffer header.
        place: Option<Slot>,
    },
    /// Validate metadata access against the whole record's moved/borrowed state.
    Access { src: Slot, mutable: bool },
    /// End a lexical owner binding, unless its capability was already moved.
    End(Slot),
}

impl FunctionPlan {
    pub(crate) fn has_buffers(&self) -> bool {
        self.result
            .as_ref()
            .is_some_and(|s| s.ty.contains_buffers())
            || self.ops.iter().any(|op| match op {
                Op::Buffer(_) => true,
                Op::Reference { dst, .. } => dst.ty.is_buffer_view(),
                Op::Call { args, result, .. } => args
                    .iter()
                    .flatten()
                    .chain(result)
                    .any(|s| s.ty.contains_buffers()),
                _ => false,
            })
    }
}

impl BufferIntrinsic {
    pub(crate) const ALL: [Self; 37] = [
        Self::Allocate,
        Self::Transfer,
        Self::TakeLast,
        Self::SwapElements,
        Self::Len,
        Self::Capacity,
        Self::CapacityLimit,
        Self::Load,
        Self::Store,
        Self::InitializeNext,
        Self::Truncate,
        Self::Replace,
        Self::Release,
        Self::Borrow,
        Self::BorrowRef,
        Self::BorrowMutRef,
        Self::BorrowMut,
        Self::Slice,
        Self::SliceMut,
        Self::ElementLoad,
        Self::ElementStore,
        Self::SliceLen,
        Self::SliceLoad,
        Self::SliceStore,
        Self::SliceBorrow,
        Self::SliceBorrowMut,
        Self::SliceBorrowRef,
        Self::SliceBorrowMutRef,
        Self::Subslice,
        Self::SubsliceMut,
        Self::Iter,
        Self::IterMut,
        Self::SliceIter,
        Self::SliceIterMut,
        Self::IterAdvance,
        Self::IterCurrent,
        Self::IterCurrentMut,
    ];

    pub(crate) fn borrows_storage(self) -> bool {
        matches!(
            self,
            Self::Borrow
                | Self::BorrowRef
                | Self::BorrowMutRef
                | Self::BorrowMut
                | Self::Slice
                | Self::SliceMut
                | Self::SliceBorrow
                | Self::SliceBorrowMut
                | Self::SliceBorrowRef
                | Self::SliceBorrowMutRef
                | Self::Subslice
                | Self::SubsliceMut
                | Self::Iter
                | Self::IterMut
                | Self::SliceIter
                | Self::SliceIterMut
                | Self::IterCurrent
                | Self::IterCurrentMut
        )
    }
    pub(crate) fn allocates(self) -> bool {
        matches!(
            self,
            Self::Allocate
                | Self::Slice
                | Self::SliceMut
                | Self::Subslice
                | Self::SubsliceMut
                | Self::Iter
                | Self::IterMut
                | Self::SliceIter
                | Self::SliceIterMut
        )
    }

    pub(crate) fn needs_arena(self) -> bool {
        self.allocates()
            || matches!(
                self,
                Self::Replace | Self::Release | Self::Store | Self::Truncate
            )
    }

    pub(crate) fn signature(self, element: &Type) -> crate::FunctionSignature {
        let owner = Type::Buffer(Arc::new(element.clone()));
        let shared = Type::reference(owner.clone(), false);
        let exclusive = Type::reference(owner.clone(), true);
        let borrowed_element = Type::BufferElement(Arc::new(element.clone()));
        let slice = Type::BufferSlice(Arc::new(element.clone()));
        let iterator = Type::BufferIterator(Arc::new(element.clone()));
        let (params, result) = match self {
            Self::Iter => (vec![shared], Type::reference(iterator, false)),
            Self::IterMut => (vec![exclusive], Type::reference(iterator, true)),
            Self::SliceIter => (
                vec![Type::reference(slice, false)],
                Type::reference(iterator, false),
            ),
            Self::SliceIterMut => (
                vec![Type::reference(slice, true)],
                Type::reference(iterator, true),
            ),
            Self::IterAdvance => (vec![Type::reference(iterator, false)], Type::Bool),
            Self::IterCurrent => (
                vec![Type::reference(iterator, false)],
                Type::reference(element.clone(), false),
            ),
            Self::IterCurrentMut => (
                vec![Type::reference(iterator, true)],
                Type::reference(element.clone(), true),
            ),
            Self::Allocate => (vec![Type::UInt64], owner),
            Self::Transfer => (vec![exclusive.clone(), exclusive], Type::Unit),
            Self::TakeLast => (vec![exclusive], element.clone()),
            Self::SwapElements => (vec![exclusive, Type::UInt64, Type::UInt64], Type::Unit),
            Self::Len | Self::Capacity => (vec![shared], Type::UInt64),
            Self::CapacityLimit => (vec![], Type::UInt64),
            Self::Load => (vec![shared, Type::UInt64], element.clone()),
            Self::Store => (vec![exclusive, Type::UInt64, element.clone()], Type::Unit),
            Self::InitializeNext => (vec![exclusive, element.clone()], Type::Unit),
            Self::Truncate => (vec![exclusive, Type::UInt64], Type::Unit),
            Self::Replace => (vec![exclusive, owner], Type::Unit),
            Self::Release => (vec![owner], Type::Unit),
            Self::Borrow => (
                vec![shared, Type::UInt64],
                Type::reference(borrowed_element, false),
            ),
            Self::BorrowRef => (
                vec![shared, Type::UInt64],
                Type::reference(element.clone(), false),
            ),
            Self::BorrowMutRef => (
                vec![exclusive, Type::UInt64],
                Type::reference(element.clone(), true),
            ),
            Self::BorrowMut => (
                vec![exclusive, Type::UInt64],
                Type::reference(borrowed_element, true),
            ),
            Self::Slice => (
                vec![shared, Type::UInt64, Type::UInt64],
                Type::reference(slice, false),
            ),
            Self::SliceMut => (
                vec![exclusive, Type::UInt64, Type::UInt64],
                Type::reference(slice, true),
            ),
            Self::ElementLoad => (
                vec![Type::reference(borrowed_element, false)],
                element.clone(),
            ),
            Self::ElementStore => (
                vec![Type::reference(borrowed_element, true), element.clone()],
                Type::Unit,
            ),
            Self::SliceLen => (vec![Type::reference(slice, false)], Type::UInt64),
            Self::SliceLoad => (
                vec![Type::reference(slice, false), Type::UInt64],
                element.clone(),
            ),
            Self::SliceStore => (
                vec![Type::reference(slice, true), Type::UInt64, element.clone()],
                Type::Unit,
            ),
            Self::SliceBorrow => (
                vec![Type::reference(slice, false), Type::UInt64],
                Type::reference(borrowed_element, false),
            ),
            Self::SliceBorrowMut => (
                vec![Type::reference(slice, true), Type::UInt64],
                Type::reference(borrowed_element, true),
            ),
            Self::SliceBorrowRef => (
                vec![Type::reference(slice, false), Type::UInt64],
                Type::reference(element.clone(), false),
            ),
            Self::SliceBorrowMutRef => (
                vec![Type::reference(slice, true), Type::UInt64],
                Type::reference(element.clone(), true),
            ),
            Self::Subslice => (
                vec![
                    Type::reference(slice.clone(), false),
                    Type::UInt64,
                    Type::UInt64,
                ],
                Type::reference(slice, false),
            ),
            Self::SubsliceMut => (
                vec![
                    Type::reference(slice.clone(), true),
                    Type::UInt64,
                    Type::UInt64,
                ],
                Type::reference(slice, true),
            ),
        };
        crate::FunctionSignature::new(params, result)
    }

    pub(crate) fn may_fail(self) -> bool {
        !matches!(
            self,
            Self::Len | Self::Capacity | Self::CapacityLimit | Self::SliceLen | Self::IterAdvance
        )
    }

    // Distinct checked source signatures may share the same raw runtime ABI.
    pub(crate) fn runtime_symbol(self) -> &'static str {
        match self {
            Self::IterMut | Self::SliceIter | Self::SliceIterMut => "__joky_buffer_iter",
            Self::IterCurrentMut => "__joky_buffer_iter_current",
            Self::Borrow
            | Self::BorrowRef
            | Self::BorrowMutRef
            | Self::BorrowMut
            | Self::SliceBorrow
            | Self::SliceBorrowMut
            | Self::SliceBorrowRef
            | Self::SliceBorrowMutRef => "__joky_buffer_load",
            Self::Slice | Self::SliceMut | Self::Subslice | Self::SubsliceMut => {
                "__joky_buffer_slice"
            }
            Self::ElementLoad | Self::ElementStore => "__joky_buffer_element_load",
            Self::SliceLen => "__joky_buffer_len",
            Self::SliceLoad | Self::SliceStore => "__joky_buffer_load",
            _ => self.symbol(),
        }
    }

    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::Iter => "__joky_buffer_iter",
            Self::IterMut => "__joky_buffer_iter_mut",
            Self::SliceIter => "__joky_buffer_slice_iter",
            Self::SliceIterMut => "__joky_buffer_slice_iter_mut",
            Self::IterAdvance => "__joky_buffer_iter_advance",
            Self::IterCurrent => "__joky_buffer_iter_current",
            Self::IterCurrentMut => "__joky_buffer_iter_current_mut",
            Self::Allocate => "__joky_buffer_allocate",
            Self::Transfer => "__joky_buffer_transfer",
            Self::TakeLast => "__joky_buffer_take_last",
            Self::SwapElements => "__joky_buffer_swap_elements",
            Self::Len => "__joky_buffer_len",
            Self::Capacity => "__joky_buffer_capacity",
            Self::CapacityLimit => "__joky_buffer_capacity_limit",
            Self::Load => "__joky_buffer_load",
            Self::Store => "__joky_buffer_store",
            Self::InitializeNext => "__joky_buffer_initialize_next",
            Self::Truncate => "__joky_buffer_truncate",
            Self::Replace => "__joky_buffer_replace",
            Self::Release => "__joky_buffer_release",
            Self::Borrow => "__joky_buffer_borrow",
            Self::BorrowRef => "__joky_buffer_borrow_ref",
            Self::BorrowMutRef => "__joky_buffer_borrow_mut_ref",
            Self::BorrowMut => "__joky_buffer_borrow_mut",
            Self::Slice => "__joky_buffer_slice",
            Self::SliceMut => "__joky_buffer_slice_mut",
            Self::ElementLoad => "__joky_buffer_element_load",
            Self::ElementStore => "__joky_buffer_element_store",
            Self::SliceLen => "__joky_buffer_slice_len",
            Self::SliceLoad => "__joky_buffer_slice_load",
            Self::SliceStore => "__joky_buffer_slice_store",
            Self::SliceBorrow => "__joky_buffer_slice_borrow",
            Self::SliceBorrowMut => "__joky_buffer_slice_borrow_mut",
            Self::SliceBorrowRef => "__joky_buffer_slice_borrow_ref",
            Self::SliceBorrowMutRef => "__joky_buffer_slice_borrow_mut_ref",
            Self::Subslice => "__joky_buffer_subslice",
            Self::SubsliceMut => "__joky_buffer_subslice_mut",
        }
    }
}

impl FunctionPlan {
    pub(super) fn storage_body(
        &mut self,
        f: &FnDef,
        intrinsic: BufferIntrinsic,
    ) -> Result<(), Error> {
        let element = f.buffer_element.as_deref().ok_or_else(|| {
            type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Missing compiler buffer element argument",
            )
        })?;
        let element = self.pointee(element, &self.type_registry)?;
        super::super::types::validate_elements(
            &Type::Buffer(Arc::new(element.clone())),
            &self.nominals,
            &self.type_registry,
        )
        .map_err(|e| e.with_span(f.span))?;
        let element = &element;
        self.type_registry
            .validate(&Type::Buffer(Arc::new(element.clone())))
            .map_err(|e| e.with_span(f.span))?;
        if element.is_owner()
            && !matches!(
                intrinsic,
                BufferIntrinsic::Allocate
                    | BufferIntrinsic::Len
                    | BufferIntrinsic::Capacity
                    | BufferIntrinsic::CapacityLimit
                    | BufferIntrinsic::InitializeNext
                    | BufferIntrinsic::Store
                    | BufferIntrinsic::Truncate
                    | BufferIntrinsic::Replace
                    | BufferIntrinsic::Release
                    | BufferIntrinsic::Transfer
                    | BufferIntrinsic::SwapElements
                    | BufferIntrinsic::TakeLast
                    | BufferIntrinsic::BorrowRef
                    | BufferIntrinsic::BorrowMutRef
                    | BufferIntrinsic::Slice
                    | BufferIntrinsic::SliceMut
                    | BufferIntrinsic::SliceLen
                    | BufferIntrinsic::Subslice
                    | BufferIntrinsic::SubsliceMut
                    | BufferIntrinsic::SliceBorrowRef
                    | BufferIntrinsic::SliceBorrowMutRef
                    | BufferIntrinsic::Iter
                    | BufferIntrinsic::IterMut
                    | BufferIntrinsic::SliceIter
                    | BufferIntrinsic::SliceIterMut
                    | BufferIntrinsic::IterAdvance
                    | BufferIntrinsic::IterCurrent
                    | BufferIntrinsic::IterCurrentMut
            )
        {
            return Err(type_error(&f.span, ErrorCode::TypeMismatch,
                "Owning array elements require borrow_ref/borrow_mut_ref for borrowed access or take_last/pop for moves; Copy reads and Element views are not supported"));
        }
        // Direct references also support reference-free Copy keys. Reference-bearing
        // elements still use Element<T>, whose reads track stored loan contents.
        if matches!(
            intrinsic,
            BufferIntrinsic::BorrowRef
                | BufferIntrinsic::BorrowMutRef
                | BufferIntrinsic::SliceBorrowRef
                | BufferIntrinsic::SliceBorrowMutRef
                | BufferIntrinsic::Iter
                | BufferIntrinsic::IterMut
                | BufferIntrinsic::SliceIter
                | BufferIntrinsic::SliceIterMut
                | BufferIntrinsic::IterCurrent
                | BufferIntrinsic::IterCurrentMut
        ) && !element.is_owner()
            && (!element.is_copy_storage_value() || element.contains_references())
            && !matches!(element, Type::Parameter(_) | Type::Projection(_))
        {
            return Err(type_error(
                &f.span,
                ErrorCode::TypeMismatch,
                "borrow_ref and borrow_mut_ref require reference-free elements",
            ));
        }
        let signature = intrinsic.signature(element);
        if f.suspends
            || f.ret_ty != signature.ret
            || !f.params.iter().map(|p| &p.ty).eq(signature.params.iter())
        {
            return Err(type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Invalid compiler buffer primitive signature",
            ));
        }
        let mut index = 0;
        let args: Vec<Slot> = f
            .params
            .iter()
            .map(|p| {
                let slot = Slot {
                    index,
                    ty: p.ty.clone(),
                };
                index += p.ty.parameter_slots();
                slot
            })
            .collect();
        let dst = if f.ret_ty == Type::Unit {
            None
        } else {
            Some(self.slot(f.ret_ty.clone())?)
        };
        for arg in args.iter().filter(|arg| arg.ty.is_owner()) {
            self.register_owner(arg.clone(), None);
        }
        if let Some(dst) = dst.as_ref().filter(|s| s.ty.is_place()) {
            self.emit(Op::Place(PlaceOp::Allocate(dst.clone())));
        }
        self.emit(Op::Buffer(BufferOp::Storage {
            intrinsic,
            element: element.clone(),
            args,
            dst: dst.clone(),
        }));
        if let Some(value) = dst.as_ref().filter(|value| value.ty.is_owner()) {
            self.register_owner(value.clone(), None);
        }
        self.result = dst;
        self.resolve_cleanups()?;
        Ok(())
    }
}

pub(super) fn check_function(plan: &FunctionPlan, info: &FunctionInfo) -> Result<(), Error> {
    let mut signature = info.signature.clone();
    // Generic reference targets may carry only nominal identity until planning.
    for ty in signature
        .params
        .iter_mut()
        .chain(std::iter::once(&mut signature.ret))
    {
        if let Some(r) = ty.reference_type() {
            *ty = Type::reference(plan.pointee(&r.target, &plan.type_registry)?, r.mutable);
        }
    }
    for ty in signature
        .params
        .iter()
        .chain(std::iter::once(&signature.ret))
    {
        if ty.contains_fixed_array() && (info.suspends || !super::fixed_arrays::eligible(ty)) {
            return Err(type_error(
                &info.span,
                ErrorCode::TypeMismatch,
                format!("FixedArray is not supported in this context: {ty}"),
            ));
        }
        if matches!(
            ty,
            Type::BufferElement(_) | Type::BufferSlice(_) | Type::BufferIterator(_)
        ) {
            return Err(type_error(
                &info.span,
                ErrorCode::TypeMismatch,
                "Element, Slice and Iter are borrow-only targets; use & or &mut",
            ));
        }
        // Owner reference targets retain nominal identity rather than inline
        // fields. Recover concrete element layouts before validating storage.
        let target = ty
            .reference_type()
            .filter(|r| r.target.is_owner())
            .map(|r| plan.pointee(&r.target, &plan.type_registry))
            .transpose()
            .map_err(|e| e.with_span(info.span))?;
        for element in target.as_ref().unwrap_or(ty).buffer_elements() {
            if !element.is_buffer_element() {
                return Err(type_error(
                    &info.span,
                    ErrorCode::TypeMismatch,
                    "Buffer elements require supported Copy data, String, or reference-free owning aggregates",
                ));
            }
        }
    }
    if info.suspends {
        // Associated bindings describe method signatures, not buffers stored in
        // a dyn view. Its synchronous calls are safe; borrowing.rs rejects any
        // view live at a language suspension or concurrency boundary.
        let unsupported = |ty: &Type| {
            ty.dyn_interface().is_none()
                && ty.contains_buffers()
                && !ty.is_suspend_safe_owner()
                && !ty.is_resource_view()
        };
        if signature
            .params
            .iter()
            .chain(std::iter::once(&signature.ret))
            .enumerate()
            .any(|(index, ty)| {
                unsupported(ty)
                    && !(index < signature.params.len()
                        && ty.is_suspend_safe_view()
                        && info.parameter_noescape(index))
            })
            || plan.ops.iter().any(|op| match op {
                Op::Buffer(BufferOp::Storage { args, dst, .. }) => args
                    .iter()
                    .chain(dst)
                    .any(|s| unsupported(&s.ty) && !s.ty.is_buffer_view()),
                Op::Buffer(BufferOp::Move { dst, .. } | BufferOp::End(dst)) => unsupported(&dst.ty),
                Op::Call { args, result, .. } => args.iter().flatten().chain(result).any(|s| {
                    // Borrowed arguments/results may exist inside a synchronous
                    // segment. The loan analysis checks actual suspension edges.
                    unsupported(&s.ty) && !s.ty.is_buffer_view()
                }),
                _ => false,
            })
        {
            return Err(type_error(&info.span, ErrorCode::InvalidCall, "Only reference-free owning values may cross ordinary language suspension; buffer views and owners containing references require synchronous functions"));
        }
    }
    if signature.ret.is_owner()
        && !info.uses_arena
        && !matches!(info.compiler_intrinsic, Some(CompilerIntrinsic::ArenaNew))
        // An empty owning enum initializes its inactive owner lanes to null.
        && !plan.ops.iter().any(|op| {
            matches!(op, Op::Literal(dst, Constant::NullPtr) if dst.ty.is_owner_lane())
        })
        && !signature.params.iter().any(|ty| {
            ty.is_owner()
                || ty
                    .reference_type()
                    .is_some_and(|r| r.mutable && r.target.is_owner())
        })
    {
        return Err(type_error(
            &info.span,
            ErrorCode::InvalidAssignment,
            "A buffer result requires a moved owner input or a fresh task allocation",
        ));
    }
    if signature.ret.is_buffer_view() {
        let mutable = signature.ret.reference_type().unwrap().mutable;
        if !signature.params.iter().any(|ty| {
            ty.reference_type().is_some_and(|r| {
                (ty.is_buffer_view() || r.target.is_place()) && (!mutable || r.mutable)
            })
        }) {
            return Err(type_error(
                &info.span,
                ErrorCode::InvalidAssignment,
                "A returned buffer view requires a compatible borrowed input",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod storage_tests;
