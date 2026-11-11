//! Closed synchronous native admission. New IR operations are denied by default.
//! This is deliberately narrower than the language: accepting an owner also
//! accepts its failure cleanup, so an unknown release cannot be deferred to exit.
use crate::core::ir::{BufferOp, CleanupOp, ModulePlan, Op, PlaceOp, TextSource};
use crate::core::types::walk::{TypeEdges, TypeWalk};
use crate::{Error, ErrorCode, ErrorPhase, Span, Type};
use std::collections::{BTreeSet, HashMap, HashSet};

fn reject(name: &str, span: Span, reason: impl std::fmt::Display) -> Error {
    Error::new_with_code(
        ErrorPhase::Compile,
        ErrorCode::InvalidCall,
        format!("Strict native stack admission rejected `{name}`: {reason}"),
    )
    .with_span(span)
}

fn storage(ty: &Type) -> bool {
    TypeWalk::new(ty, TypeEdges::Fields).all(|ty| match ty {
        Type::Resource(_)
        | Type::Named(_)
        | Type::Applied(_)
        | Type::Parameter(_)
        | Type::Projection(_)
        | Type::Buffer(_)
        | Type::BufferElement(_)
        | Type::BufferSlice(_)
        | Type::BufferIterator(_)
        | Type::BufferPairIterator(_)
        | Type::BufferEntry(_) => false,
        Type::Record(r) => !r.indirect && r.closure.is_none(),
        Type::Enum(e) => !e.indirect && !e.compact,
        Type::Function(f) => !f.suspends,
        // Dyn values are borrowed. Concrete entries and their signatures are
        // qualified below and their actual addresses checked before every call.
        Type::FixedArray(_)
        | Type::FixedArrayInit(_)
        | Type::Reference(_)
        | Type::DynTrait(_)
        | Type::Tuple(_)
        | Type::String
        | Type::Ptr
        | Type::Bool
        | Type::Unit
        | Type::Int8
        | Type::Int16
        | Type::Int32
        | Type::Int64
        | Type::UInt8
        | Type::UInt16
        | Type::UInt32
        | Type::UInt64
        | Type::Rune
        | Type::Float32
        | Type::Float64 => true,
    })
}

pub(super) fn check(
    plan: &ModulePlan,
    reachable: &HashSet<String>,
) -> Result<HashMap<String, Vec<String>>, Error> {
    let mut names: Vec<_> = plan
        .bodies
        .keys()
        .filter(|name| reachable.contains(*name))
        .collect();
    names.sort();
    let mut targets = Vec::new();
    for name in &names {
        for op in &plan.bodies[*name].ops {
            match op {
                Op::FunctionRef(slot, target) => {
                    if let Type::Function(function) = &slot.ty {
                        targets.push((function.signature.clone(), target.clone()));
                    }
                }
                Op::DynTable { table, .. } => {
                    targets.extend(
                        table
                            .interface
                            .methods
                            .iter()
                            .zip(&table.methods)
                            .map(|(method, target)| (method.signature.clone(), target.clone())),
                    );
                }
                _ => {}
            }
        }
    }
    let mut indirect_targets = HashMap::new();
    for name in names {
        let info = &plan.functions[name];
        if info.suspends || info.script_entry || info.parallel_entry {
            return Err(reject(
                name,
                info.span,
                "CPS, persistent state and worker entries have no synchronous budget",
            ));
        }
        if !info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
            .all(storage)
        {
            return Err(reject(
                name,
                info.span,
                "parameter/result storage has an unbudgeted ownership or callback dependency",
            ));
        }
        let body = &plan.bodies[name];
        for (op, &span) in body.ops.iter_spanned() {
            // Every concrete dependency must belong to this immutable module.
            // A matching host name or language signature is never evidence.
            for dependency in op.dependencies() {
                if !plan.bodies.contains_key(dependency) {
                    return Err(reject(
                        name,
                        span,
                        format!("callee `{dependency}` is an unbudgeted host or imported entry"),
                    ));
                }
            }
            let mut types = Vec::new();
            match op {
                Op::Label(_) | Op::Jump(_) => {}
                Op::Branch { condition, .. } => types.push(&condition.ty),
                Op::Literal(slot, _) | Op::EndLocal(slot) => types.push(&slot.ty),
                Op::Copy(dst, src)
                | Op::Convert(dst, src)
                | Op::Unary(dst, _, src)
                | Op::Reference { dst, src, .. }
                | Op::Reserve { dst, src }
                | Op::Project { dst, src, .. }
                | Op::PatternProject { dst, src, .. } => {
                    types.extend([&dst.ty, &src.ty]);
                }
                Op::Binary(dst, _, left, right) => {
                    types.extend([&dst.ty, &left.ty, &right.ty]);
                }
                Op::Text {
                    dst,
                    source: TextSource::Literal(_),
                } => types.push(&dst.ty),
                Op::Text {
                    dst,
                    source: TextSource::Clone(src),
                } => types.extend([&dst.ty, &src.ty]),
                Op::Local { dst, value, target } => {
                    types.extend([&dst.ty, target]);
                    types.extend(value.iter().map(|value| &value.ty));
                }
                Op::ReadMemory { dst, reference, .. } => {
                    types.push(&reference.ty);
                    types.extend(dst.iter().map(|value| &value.ty));
                }
                Op::WriteMemory {
                    reference,
                    value,
                    target,
                    ..
                } => {
                    types.extend([&reference.ty, target]);
                    types.extend(value.iter().map(|value| &value.ty));
                }
                Op::Cleanup(CleanupOp::Register { value, place, .. }) => {
                    types.push(&value.ty);
                    types.extend(place.iter().map(|value| &value.ty));
                }
                Op::Buffer(BufferOp::Move { dst, src } | BufferOp::Borrow { dst, src, .. }) => {
                    types.extend([&dst.ty, &src.ty]);
                }
                Op::Buffer(BufferOp::End(value) | BufferOp::Access { src: value, .. }) => {
                    types.push(&value.ty)
                }
                Op::Place(
                    place @ (PlaceOp::Swap { .. }
                    | PlaceOp::Alias { .. }
                    | PlaceOp::Read { .. }
                    | PlaceOp::Write { .. }
                    | PlaceOp::Allocate(_)
                    | PlaceOp::Copy { .. }
                    | PlaceOp::MoveInit { .. }
                    | PlaceOp::Begin(_)
                    | PlaceOp::WriteNext { .. }
                    | PlaceOp::End(_)),
                ) => types.push(place.ty()),
                Op::FunctionRef(slot, _) if matches!(slot.ty, Type::Function(_)) => {
                    types.push(&slot.ty)
                }
                Op::DynTable { table, .. } => types.push(&table.self_type),
                Op::DynMethod { dst, .. } => types.push(&dst.ty),
                Op::Call {
                    name: callee,
                    args,
                    result,
                    indirect,
                    state: None,
                } => {
                    let callee_info = &plan.functions[callee];
                    types.extend(callee_info.signature.params.iter());
                    types.push(&callee_info.signature.ret);
                    types.extend(args.iter().flatten().map(|arg| &arg.ty));
                    types.extend(result.iter().map(|result| &result.ty));
                    if let Some(slot) = indirect {
                        let Type::Function(function) = &slot.ty else {
                            return Err(reject(
                                name,
                                span,
                                "indirect call lacks a checked language function type",
                            ));
                        };
                        let candidates: BTreeSet<_> = targets
                            .iter()
                            .filter(|(signature, target)| {
                                signature == &function.signature && plan.bodies.contains_key(target)
                            })
                            .map(|(_, name)| name.clone())
                            .collect();
                        if candidates.is_empty() {
                            return Err(reject(
                                name,
                                span,
                                "indirect call has no admitted targets in this module",
                            ));
                        }
                        indirect_targets.insert(callee.clone(), candidates.into_iter().collect());
                    }
                }
                _ => {
                    return Err(reject(
                        name,
                        span,
                        "runtime operation has no strict stack budget",
                    ))
                }
            }
            if types.into_iter().any(|ty| !storage(ty)) {
                return Err(reject(
                    name,
                    span,
                    "value storage has an unbudgeted ownership or callback dependency",
                ));
            }
        }
    }
    Ok(indirect_targets)
}
