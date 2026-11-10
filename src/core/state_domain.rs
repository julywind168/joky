//! Conservative, compiler-proved entries for owning state domains.
use super::ir::{BufferOp, FunctionInfo, FunctionPlan, Op};
use crate::Type;
use std::collections::{HashMap, HashSet};

/// Only the standard Option of a directory key participates in optional paths.
pub(crate) fn optional_key(ty: &Type) -> Option<&Type> {
    let Type::Enum(e) = ty else { return None };
    if e.name != "joky/option.Option" {
        return None;
    }
    let key = e.arguments.first()?.as_type()?;
    matches!(key, Type::UInt64 | Type::String).then_some(key)
}

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
/// Synchronous access to independently owned, compiler-checked state records.
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
                        )
                    })
            })
        && info.signature.params[count..]
            .iter()
            .all(super::instance::scalar)
}

/// Bounded value inputs/results, including String leaves, with the existing root restrictions.
pub(crate) fn value_eligible(info: &FunctionInfo) -> bool {
    let count = access_count(info);
    !info.suspends
        && (1..=64).contains(&count)
        && super::script::domain_schema(&info.signature.ret).is_some()
        && info.signature.params.len() <= count + 1
        && info.signature.params[count..]
            .iter()
            .all(|ty| super::script::domain_schema(ty).is_some())
        && info.signature.params[..count]
            .iter()
            .enumerate()
            .all(|(i, ty)| {
                info.parameter_noescape(i)
                    && ty.reference_type().is_some_and(|r| {
                        matches!(&r.target, Type::Record(record) if record.template.is_none()
                    && record.parameters.is_empty() && record.arguments.is_empty()
                    && record.closure.is_none())
                    })
            })
}

/// Domain-only constructors accept one bounded value packet.
/// The persistent-instance API retains its scalar argument contract.
pub(crate) fn constructor_eligible(info: &FunctionInfo) -> bool {
    !info.suspends
        && matches!(info.signature.ret, Type::Record(_))
        && info.signature.params.len() == 1
        && super::script::domain_schema(&info.signature.params[0]).is_some()
}

/// The entire owning graph are checked by `infer`.
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
        Op::Cleanup(_) => true,
        Op::Buffer(BufferOp::Storage { element, .. }) => {
            body.domain_storage(element) || body.domain_value(element)
        }
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
    let safe = safe_bodies(functions, bodies, false);
    let transactions = safe_bodies(functions, bodies, true);
    for (name, info) in functions {
        info.domain_request_entry = info.suspends
            && info.signature.params.len() == 1
            && bodies.get(name).is_some_and(|body| {
                body.domain_value(&info.signature.params[0])
                    && body.domain_value(&info.signature.ret)
            });
        info.script_entry &= info.domain_request_entry;
        let pair = safe.contains(name)
            && pair_eligible(info)
            && info.signature.params[..access_count(info)]
                .iter()
                .all(|ty| state_root(&ty.reference_type().unwrap().target, &bodies[name]));
        let values = (safe.contains(name)
            || name.starts_with("$when/") && transactions.contains(name))
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
        let constructor = safe.contains(name)
            && constructor_eligible(info)
            && state_root(&info.signature.ret, &bodies[name]);
        info.instance_entry |= pair || migration || step || values || constructor;
        info.domain_entry = constructor
            || values
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

fn safe_bodies(
    functions: &HashMap<String, FunctionInfo>,
    bodies: &HashMap<String, FunctionPlan>,
    transactional: bool,
) -> HashSet<String> {
    let mut safe: HashSet<_> = bodies
        .iter()
        .filter(|(name, body)| {
            !functions[*name].suspends
                && body.ops.iter().all(|op| {
                    (transactional && matches!(op, Op::OutboxEmit { .. }))
                        || storage_op(op, body)
                        || matches!(
                            op,
                            // Closure markers only track static move/borrow
                            // responsibility. Actual calls are proved below.
                            Op::Closure(_)
                                | Op::Copy(..)
                                | Op::Reference { .. }
                                | Op::Reserve { .. }
                                | Op::Project { .. }
                                | Op::PatternProject { .. }
                                | Op::EntryProject { .. }
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
    safe
}
