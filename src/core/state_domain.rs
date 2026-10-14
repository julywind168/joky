//! Conservative, compiler-proved entries for hook-free owning state domains.
use super::ir::{BufferOp, CleanupOp, FunctionInfo, FunctionPlan, Op};
use crate::Type;
use std::collections::{HashMap, HashSet};

fn state_root(ty: &Type, body: &FunctionPlan) -> bool {
    matches!(ty, Type::Record(record)
        if record.template.is_none()
            && record.parameters.is_empty()
            && record.arguments.is_empty()
            && body.domain_storage(ty))
}
pub(crate) fn access_count(info: &FunctionInfo) -> usize {
    info.signature
        .params
        .iter()
        .take_while(|ty| ty.reference_type().is_some())
        .count()
}
/// Synchronous access to independently stored scalar records.
pub(crate) fn pair_eligible(info: &FunctionInfo) -> bool {
    let count = access_count(info);
    !info.suspends
        && super::instance::scalar(&info.signature.ret)
        && count >= 2
        && info.signature.params[..count]
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
        && info.signature.params[count..]
            .iter()
            .all(super::instance::scalar)
}

/// Bounded pure-value inputs/results, with the existing root restrictions.
pub(crate) fn value_eligible(info: &FunctionInfo) -> bool {
    let count = access_count(info);
    !info.suspends
        && (1..=64).contains(&count)
        && super::script::schema(&info.signature.ret).is_some()
        && info.signature.params.len() <= count + 1
        && info.signature.params[count..]
            .iter()
            .all(|ty| super::script::schema(ty).is_some())
        && info.signature.params[..count]
            .iter()
            .enumerate()
            .all(|(i, ty)| {
                info.parameter_noescape(i)
                    && ty.reference_type().is_some_and(|r| {
                        matches!(&r.target, Type::Record(record) if record.template.is_none()
                    && record.parameters.is_empty() && record.arguments.is_empty()
                    && (count == 1 || record.fields.iter().all(|f| super::instance::scalar(&f.ty))))
                    })
            })
}

/// Storage hooks and the entire owning graph are checked by `infer`.
fn migration_record(ty: &Type) -> bool {
    matches!(ty, Type::Record(r) if r.template.is_none() && r.parameters.is_empty()
        && r.arguments.is_empty() && r.closure.is_none())
}
pub(crate) fn migration_eligible(info: &FunctionInfo) -> bool {
    !info.suspends
        && info.parameter_noescape(0)
        && info.signature.params.len() == 1
        && info.signature.params[0]
            .reference_type()
            .is_some_and(|r| !r.mutable && migration_record(&r.target))
        && migration_record(&info.signature.ret)
}

/// A step may mutate only the independent candidate; neither borrow may escape.
pub(crate) fn migration_step_eligible(info: &FunctionInfo) -> bool {
    !info.suspends
        && info.signature.params.len() == 3
        && info.signature.ret == Type::Bool
        && info.signature.params[2] == Type::UInt64
        && [true, false].into_iter().enumerate().all(|(i, mutable)| {
            info.parameter_noescape(i)
                && info.signature.params[i]
                    .reference_type()
                    .is_some_and(|r| r.mutable == mutable && migration_record(&r.target))
        })
}

fn storage_op(op: &Op, body: &FunctionPlan) -> bool {
    match op {
        Op::Text { .. }
        | Op::Assert { .. }
        | Op::Panic { .. }
        | Op::ReadOwner { .. }
        | Op::StoreOwner { .. } => true,
        Op::Alloc { target, .. } => super::instance::scalar(target),
        Op::Cleanup(CleanupOp::Register {
            hooks, elements, ..
        }) => hooks.is_empty() && elements.iter().all(|e| e.hooks.is_empty()),
        Op::Buffer(BufferOp::Storage { element, .. }) => body.domain_storage(element),
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
                    storage_op(op, body)
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
            && info.signature.params[..access_count(info)]
                .iter()
                .all(|ty| state_root(&ty.reference_type().unwrap().target, &bodies[name]));
        let values = safe.contains(name)
            && value_eligible(info)
            && info.signature.params[..access_count(info)]
                .iter()
                .all(|ty| state_root(&ty.reference_type().unwrap().target, &bodies[name]));
        // Reuse the typed lane bridge, without changing persistent-instance eligibility.
        let migration = safe.contains(name)
            && migration_eligible(info)
            && state_root(&info.signature.ret, &bodies[name])
            && state_root(
                &info.signature.params[0].reference_type().unwrap().target,
                &bodies[name],
            );
        let step = safe.contains(name)
            && migration_step_eligible(info)
            && info.signature.params[..2]
                .iter()
                .all(|ty| state_root(&ty.reference_type().unwrap().target, &bodies[name]));
        info.instance_entry |= pair || migration || step || values;
        info.domain_entry = values
            || step
            || pair
            || migration
            || safe.contains(name)
                && ((super::instance::constructor(info)
                    && state_root(&info.signature.ret, &bodies[name]))
                    || super::instance::receiver(info)
                        .is_some_and(|ty| state_root(ty, &bodies[name])));
    }
}
