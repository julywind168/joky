//! Conservative, compiler-proved entries for flat owning state domains.
use super::ir::{BufferOp, CleanupOp, FunctionInfo, FunctionPlan, Op};
use crate::Type;
use std::collections::{HashMap, HashSet};

fn state_field(ty: &Type, array: Option<u64>) -> bool {
    super::instance::scalar(ty)
        || *ty == Type::String
        || matches!(ty, Type::Record(r)
        if array == Some(r.id) && r.template.is_some()
            && r.arguments.len() == 1 && super::instance::scalar(&r.arguments[0])
            && r.fields.len() == 1
            && matches!(&r.fields[0].ty, Type::Buffer(element) if **element == r.arguments[0]))
}
fn state_root(ty: &Type, body: &FunctionPlan) -> bool {
    let array = body.domain_array_identity();
    matches!(ty, Type::Record(record)
        if record.template.is_none()
            && record.parameters.is_empty()
            && record.arguments.is_empty()
            && record.closure.is_none()
            && !body.has_custom_drop(ty)
            && record.fields.iter().all(|field| state_field(&field.ty, array) && !body.has_custom_drop(&field.ty)))
}
/// First multi-domain slice: two independently stored scalar records.
pub(crate) fn pair_eligible(info: &FunctionInfo) -> bool {
    !info.suspends
        && super::instance::scalar(&info.signature.ret)
        && info.signature.params.len() >= 2
        && info.signature.params[..2]
            .iter()
            .enumerate()
            .all(|(i, ty)| {
                info.parameter_noescape(i)
                    && ty.reference_type().is_some_and(|r| {
                        matches!(&r.target, Type::Record(record)
                    if record.template.is_none() && record.parameters.is_empty()
                    && record.arguments.is_empty() && record.closure.is_none()
                    && record.fields.iter().all(|f| super::instance::scalar(&f.ty)))
                    })
            })
        && info.signature.params[2..]
            .iter()
            .all(super::instance::scalar)
}

fn storage_op(op: &Op) -> bool {
    match op {
        Op::Text { .. } | Op::Require { .. } | Op::ReadOwner { .. } | Op::StoreOwner { .. } => true,
        Op::Alloc { target, .. } => super::instance::scalar(target),
        Op::Cleanup(CleanupOp::Register {
            hooks, elements, ..
        }) => hooks.is_empty() && elements.iter().all(|e| e.hooks.is_empty()),
        Op::Buffer(BufferOp::Storage { element, .. }) => super::instance::scalar(element),
        Op::Buffer(_) => true,
        _ => false,
    }
}

/// Start with locally safe bodies, then remove callers of any unproved entry.
/// This admits recursive synchronous helpers without trusting extern declarations,
/// indirect targets, cleanup callbacks or a user-written effect annotation.
pub(crate) fn infer(
    functions: &mut HashMap<String, FunctionInfo>,
    bodies: &HashMap<String, FunctionPlan>,
) {
    let mut safe: HashSet<_> = bodies
        .iter()
        .filter(|(name, body)| {
            !functions[*name].suspends
                && !body.is_destructor
                && body.ops.iter().all(|op| {
                    storage_op(op)
                        || matches!(
                            op,
                            Op::Copy(..)
                                | Op::Reference { .. }
                                | Op::Reserve { .. }
                                | Op::Project { .. }
                                | Op::PatternProject { .. }
                                | Op::Local { .. }
                                | Op::EndLocal(_)
                                | Op::ReadMemory { .. }
                                | Op::WriteMemory { .. }
                                | Op::Label(_)
                                | Op::Jump(_)
                                | Op::Branch { .. }
                                | Op::Literal(..)
                                | Op::Binary(..)
                                | Op::Unary(..)
                                | Op::Convert(..)
                                | Op::Float { .. }
                                | Op::Call {
                                    indirect: None,
                                    state: None,
                                    ..
                                }
                        )
                })
        })
        .map(|(name, _)| name.clone())
        .collect();
    loop {
        let rejected: Vec<_> = safe
            .iter()
            .filter(|name| {
                bodies[*name]
                    .ops
                    .iter()
                    .any(|op| matches!(op, Op::Call { name, .. } if !safe.contains(name)))
            })
            .cloned()
            .collect();
        if rejected.is_empty() {
            break;
        }
        for name in rejected {
            safe.remove(&name);
        }
    }
    for (name, info) in functions {
        let pair = safe.contains(name)
            && pair_eligible(info)
            && info.signature.params[..2]
                .iter()
                .all(|ty| state_root(&ty.reference_type().unwrap().target, &bodies[name]));
        // Reuse the typed lane bridge, without changing persistent-instance eligibility.
        info.instance_entry |= pair;
        info.domain_entry = pair
            || safe.contains(name)
                && ((super::instance::constructor(info)
                    && state_root(&info.signature.ret, &bodies[name]))
                    || super::instance::receiver(info)
                        .is_some_and(|ty| state_root(ty, &bodies[name])));
    }
}
