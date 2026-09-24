use super::*;

pub(super) fn method_key(receiver: Type, name: &str) -> String {
    match receiver {
        Type::Struct(id) => format!("struct_{id}_{name}"),
        Type::Class(id) => format!("class_{id}_{name}"),
        _ => unreachable!("method key requires a struct/class receiver"),
    }
}

pub(super) fn resolve_field_access(
    access: &FieldAccess,
    base: Type,
    types: &CheckedTypes,
) -> Result<MirFieldAccess, Diagnostic> {
    match access {
        FieldAccess::Index(index) => Ok(MirFieldAccess::Index(*index)),
        FieldAccess::Name(name) => {
            let index = match base {
                Type::Struct(id) => types
                    .struct_fields(id)
                    .iter()
                    .position(|(field, _)| field == name),
                Type::Class(id) => types
                    .class_fields(id)
                    .iter()
                    .position(|(field, _)| field == name),
                _ => None,
            }
            .ok_or_else(|| Diagnostic::codegen("MIR field was not resolved"))?;
            Ok(MirFieldAccess::Index(index))
        }
    }
}

pub(super) fn resolve_argument_index(
    label: Option<&str>,
    names: &[String],
    used: &mut [bool],
    next_positional: &mut usize,
) -> Result<usize, Diagnostic> {
    let index = if let Some(label) = label {
        names
            .iter()
            .position(|name| name == label)
            .ok_or_else(|| Diagnostic::codegen("MIR argument label was not resolved"))?
    } else {
        while *next_positional < used.len() && used[*next_positional] {
            *next_positional += 1;
        }
        if *next_positional >= names.len() {
            return Err(Diagnostic::codegen("MIR argument index is out of bounds"));
        }
        let index = *next_positional;
        *next_positional += 1;
        index
    };
    if index >= names.len() || used[index] {
        return Err(Diagnostic::codegen(
            "MIR argument index is duplicated or out of bounds",
        ));
    }
    used[index] = true;
    Ok(index)
}

#[derive(Debug)]
pub(super) struct LoopFrame {
    pub(super) continue_block: MirBlockId,
    pub(super) break_block: MirBlockId,
    pub(super) breaks: Vec<(MirBlockId, Option<MirValueId>)>,
}

#[derive(Clone)]
pub(super) struct HandlerTarget {
    pub(super) arm: MirHandlerArm,
    pub(super) parameter_values: Vec<MirValueId>,
    /// The arm is serviced by the runtime handler frame rather than by a
    /// statically-built CFG edge.
    pub(super) runtime_dispatch: bool,
}

#[derive(Clone)]
pub(super) struct ResumableHandlerTarget {
    pub(super) parameters: Vec<CorePattern>,
    pub(super) value: CoreExpr,
    pub(super) aborts: bool,
    pub(super) abort_target: Option<MirBlockId>,
    pub(super) runtime_value: Option<MirConstant>,
    pub(super) runtime_parameter: Option<usize>,
    pub(super) runtime_resume_block: Option<MirBlockId>,
    pub(super) runtime_transform: Option<MirResumableTransform>,
    pub(super) runtime_function: Option<MirFunctionId>,
    pub(super) runtime_captures: Vec<MirValueId>,
}

pub(super) fn resumable_scalar_transform(
    value: &CoreExpr,
    parameters: &[CorePattern],
) -> Option<MirResumableTransform> {
    let value = match &value.kind {
        CoreExprKind::Call {
            callee, arguments, ..
        } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "resume")
            && arguments.len() == 1 =>
        {
            &arguments[0].value
        }
        _ => value,
    };
    let CoreExprKind::Binary { op, left, right } = &value.kind else {
        return None;
    };
    if !matches!(
        op,
        crate::syntax::BinaryOp::Add
            | crate::syntax::BinaryOp::Subtract
            | crate::syntax::BinaryOp::Multiply
    ) {
        return None;
    }
    let (parameter, literal) = match (&left.kind, &right.kind) {
        (CoreExprKind::Name(name), CoreExprKind::Integer(value)) => (name, *value),
        (CoreExprKind::Integer(value), CoreExprKind::Name(name))
            if matches!(
                op,
                crate::syntax::BinaryOp::Add | crate::syntax::BinaryOp::Multiply
            ) =>
        {
            (name, *value)
        }
        _ => return None,
    };
    let parameter = parameters.iter().position(
        |pattern| matches!(pattern, CorePattern::Binding { name, .. } if name == parameter),
    )?;
    Some(MirResumableTransform {
        parameter,
        operator: *op,
        immediate: literal as i64,
    })
}

pub(super) fn resumable_forward_parameter(
    value: &CoreExpr,
    parameters: &[CorePattern],
    types: &CheckedTypes,
) -> Option<usize> {
    // HIR strips the `resume(...)` marker and stores its sole value directly;
    // accept the call form as well for callers constructing CoreExpr by hand.
    let name = match &value.kind {
        CoreExprKind::Name(name) => name,
        CoreExprKind::Call {
            callee, arguments, ..
        } if matches!(&callee.kind, CoreExprKind::Name(name) if name == "resume")
            && arguments.len() == 1 =>
        {
            let CoreExprKind::Name(name) = &arguments[0].value.kind else {
                return None;
            };
            name
        }
        _ => return None,
    };
    let index = parameters.iter().position(
        |pattern| matches!(pattern, CorePattern::Binding { name: binding, .. } if binding == name),
    )?;
    // Forwarding retains the request's flattened words. Only Copy values are
    // safe without an ownership descriptor or pointer duplication protocol.
    let _ = types;
    Some(index)
}
