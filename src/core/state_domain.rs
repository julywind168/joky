//! Conservative, compiler-proved entries for scalar state domains.
use super::ir::{FunctionInfo, FunctionPlan, Op};
use crate::Type;
use std::collections::{HashMap, HashSet};

pub(crate) fn scalar_root(ty: &Type) -> bool {
    matches!(ty, Type::Record(record)
        if record.template.is_none()
            && record.parameters.is_empty()
            && record.arguments.is_empty()
            && record.closure.is_none()
            && record.fields.iter().all(|field| super::instance::scalar(&field.ty)))
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
                    matches!(
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
        info.domain_entry = safe.contains(name)
            && ((super::instance::constructor(info) && scalar_root(&info.signature.ret))
                || super::instance::receiver(info).is_some_and(scalar_root));
    }
}
