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
    Move {
        dst: Slot,
        src: Slot,
    },
    Borrow {
        dst: Slot,
        src: Slot,
    },
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
    pub(crate) const ALL: [Self; 9] = [
        Self::Allocate,
        Self::Len,
        Self::Capacity,
        Self::CapacityLimit,
        Self::Load,
        Self::Store,
        Self::InitializeNext,
        Self::Truncate,
        Self::Replace,
    ];

    pub(crate) fn signature(self, element: &Type) -> crate::FunctionSignature {
        let owner = Type::Buffer(Arc::new(element.clone()));
        let shared = Type::reference(owner.clone(), false);
        let exclusive = Type::reference(owner.clone(), true);
        let (params, result) = match self {
            Self::Allocate => (vec![Type::UInt64], owner),
            Self::Len | Self::Capacity => (vec![shared], Type::UInt64),
            Self::CapacityLimit => (vec![], Type::UInt64),
            Self::Load => (vec![shared, Type::UInt64], element.clone()),
            Self::Store => (vec![exclusive, Type::UInt64, element.clone()], Type::Unit),
            Self::InitializeNext => (vec![exclusive, element.clone()], Type::Unit),
            Self::Truncate => (vec![exclusive, Type::UInt64], Type::Unit),
            Self::Replace => (vec![exclusive, owner], Type::Unit),
        };
        crate::FunctionSignature::new(params, result)
    }

    pub(crate) fn may_fail(self) -> bool {
        !matches!(self, Self::Len | Self::Capacity | Self::CapacityLimit)
    }

    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::Allocate => "__joky_buffer_allocate",
            Self::Len => "__joky_buffer_len",
            Self::Capacity => "__joky_buffer_capacity",
            Self::CapacityLimit => "__joky_buffer_capacity_limit",
            Self::Load => "__joky_buffer_load",
            Self::Store => "__joky_buffer_store",
            Self::InitializeNext => "__joky_buffer_initialize_next",
            Self::Truncate => "__joky_buffer_truncate",
            Self::Replace => "__joky_buffer_replace",
        }
    }
}

impl FunctionPlan {
    pub(super) fn storage_body(
        &mut self,
        f: &FnDef,
        intrinsic: BufferIntrinsic,
    ) -> Result<(), Error> {
        let element = f.buffer_element.as_ref().ok_or_else(|| {
            type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Missing compiler buffer element argument",
            )
        })?;
        self.type_registry
            .validate(&Type::Buffer(Arc::new(element.clone())))
            .map_err(|e| e.with_span(f.span))?;
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
        let args = f
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
        self.emit(Op::Buffer(BufferOp::Storage {
            intrinsic,
            element: element.clone(),
            args,
            dst: dst.clone(),
        }));
        self.result = dst;
        Ok(())
    }
}

pub(super) fn check_function(plan: &FunctionPlan, info: &FunctionInfo) -> Result<(), Error> {
    let signature = &info.signature;
    for ty in signature
        .params
        .iter()
        .chain(std::iter::once(&signature.ret))
    {
        let target = ty.reference_type().map_or(ty, |r| &r.target);
        if let Type::Buffer(element) = target {
            if !element.is_buffer_element() {
                return Err(type_error(
                    &info.span,
                    ErrorCode::TypeMismatch,
                    "Buffer elements require reference-free Copy data with a supported memory layout",
                ));
            }
        }
    }
    if info.suspends
        && (plan.has_buffers()
            || signature
                .params
                .iter()
                .chain(std::iter::once(&signature.ret))
                .any(Type::contains_buffers))
    {
        return Err(type_error(
            &info.span,
            ErrorCode::InvalidCall,
            "Buffer owners and views are only supported in synchronous functions",
        ));
    }
    if signature.ret.is_buffer()
        && !info.uses_arena
        && !signature.params.iter().any(Type::is_buffer)
    {
        return Err(type_error(
            &info.span,
            ErrorCode::InvalidAssignment,
            "A buffer result requires a moved owner input or a fresh task allocation",
        ));
    }
    if signature.ret.is_buffer_view() {
        let mutable = signature.ret.reference_type().unwrap().mutable;
        if !signature
            .params
            .iter()
            .any(|ty| ty.is_buffer_view() && (!mutable || ty.reference_type().unwrap().mutable))
        {
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
