//! Stage 7a.1: buffer capabilities are checked before storage execution opens.
use super::*;

pub(crate) enum BufferOp {
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
    pub(super) fn has_buffers(&self) -> bool {
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

impl ModulePlan {
    /// Planning tests exercise ownership now; no backend may execute incomplete storage.
    pub(crate) fn check_executable(&self) -> Result<(), Error> {
        for (name, info) in &self.functions {
            if info
                .signature
                .params
                .iter()
                .chain(std::iter::once(&info.signature.ret))
                .any(Type::contains_buffers)
                || self.bodies.get(name).is_some_and(FunctionPlan::has_buffers)
            {
                return Err(type_error(&info.span, ErrorCode::InvalidCall,
                    "Buffer execution is not available until the storage implementation is complete"));
            }
        }
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
            if **element != Type::Int32 {
                return Err(type_error(
                    &info.span,
                    ErrorCode::TypeMismatch,
                    "Executable buffer signatures require concrete Int32 elements",
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
