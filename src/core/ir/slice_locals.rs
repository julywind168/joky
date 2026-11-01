//! Reuse slice descriptors only when their identity cannot leave an activation
//! or survive the next execution of their construction site.
use super::*;
use crate::parser::ast::BufferIntrinsic;
use std::collections::HashSet;

type Set = HashSet<usize>;

#[derive(Clone)]
enum Construction {
    Buffer {
        intrinsic: BufferIntrinsic,
        element: Type,
    },
    Fixed {
        reference: Type,
    },
}

#[derive(Clone)]
struct Factory {
    construction: Construction,
    arguments: Vec<usize>,
    span: Span,
}

fn is_slice(intrinsic: BufferIntrinsic) -> bool {
    matches!(
        intrinsic,
        BufferIntrinsic::Slice
            | BufferIntrinsic::SliceMut
            | BufferIntrinsic::Subslice
            | BufferIntrinsic::SubsliceMut
    )
}

// Recognize transparent forwarding bodies by their operations, not library
// names. No control flow, storage, side effects or arbitrary calls are erased.
fn factory(
    info: &FunctionInfo,
    body: &FunctionPlan,
    known: &HashMap<String, Factory>,
) -> Option<Factory> {
    if info.suspends || body.ops.len() > 32 {
        return None;
    }
    let mut aliases = HashMap::new();
    for (index, ty) in info.signature.params.iter().enumerate() {
        if ty.parameter_slots() != 1 || ty.is_owner() {
            return None;
        }
        aliases.insert(index, index);
    }
    let mut result = None;
    for (op, span) in body.ops.iter().zip(&body.op_spans) {
        match op {
            Op::Copy(dst, src) | Op::Reference { dst, src, .. } => {
                if dst.ty.value_lanes().len() != 1 || src.ty.value_lanes().len() != 1 {
                    return None;
                }
                aliases.insert(dst.index, *aliases.get(&src.index)?);
            }
            Op::Buffer(BufferOp::Storage {
                intrinsic,
                element,
                args,
                dst: Some(dst),
                ..
            }) if is_slice(*intrinsic) && result.is_none() => {
                result = Some(Factory {
                    construction: Construction::Buffer {
                        intrinsic: *intrinsic,
                        element: element.clone(),
                    },
                    arguments: args
                        .iter()
                        .map(|s| aliases.get(&s.index).copied())
                        .collect::<Option<_>>()?,
                    span: *span,
                });
                aliases.insert(dst.index, usize::MAX);
            }
            Op::Place(PlaceOp::View {
                reference,
                start,
                end: Some(end),
                dst,
                ..
            }) if result.is_none() => {
                result = Some(Factory {
                    construction: Construction::Fixed {
                        reference: reference.ty.clone(),
                    },
                    arguments: [reference, start, end]
                        .into_iter()
                        .map(|s| aliases.get(&s.index).copied())
                        .collect::<Option<_>>()?,
                    span: *span,
                });
                aliases.insert(dst.index, usize::MAX);
            }
            Op::Call {
                indirect: None,
                name,
                args,
                result: Some(dst),
                state: None,
            } if result.is_none() => {
                let callee = known.get(name)?;
                let arguments = callee
                    .arguments
                    .iter()
                    .map(|&i| aliases.get(&args.get(i)?.as_ref()?.index).copied())
                    .collect::<Option<_>>()?;
                result = Some(Factory {
                    arguments,
                    ..callee.clone()
                });
                aliases.insert(dst.index, usize::MAX);
            }
            _ => return None,
        }
    }
    (aliases.get(&body.result.as_ref()?.index) == Some(&usize::MAX))
        .then_some(result)
        .flatten()
}

// Failed/budget-exhausted proofs keep task-arena allocation. Optimizations must
// not introduce a new rejection or unbounded compile-time work.
fn liveness(body: &FunctionPlan) -> Option<Vec<Set>> {
    let n = body.ops.len();
    let operands: Vec<_> = body.ops.iter().map(borrowing::operands).collect();
    let mut labels = vec![n; body.label_count];
    for (pc, op) in body.ops.iter().enumerate() {
        if let Op::Label(label) = op {
            labels[*label] = pc;
        }
    }
    let mut live = vec![Set::new(); n + 1];
    if let Some(result) = &body.result {
        live[n].extend(result.scalars().map(|s| s.index));
    }
    let mut budget = 2_000_000usize;
    loop {
        let mut changed = false;
        for pc in (0..n).rev() {
            let next = match &body.ops[pc] {
                Op::Panic { .. } => vec![],
                Op::Jump(label) => vec![labels[*label]],
                Op::Branch {
                    then_label,
                    else_label,
                    ..
                } => {
                    vec![labels[*then_label], labels[*else_label]]
                }
                _ => vec![pc + 1],
            };
            budget = budget.checked_sub(
                1 + next.iter().map(|&pc| live[pc].len()).sum::<usize>()
                    + operands[pc].0.len()
                    + operands[pc].1.len(),
            )?;
            let mut input: Set = next
                .iter()
                .flat_map(|&pc| live[pc].iter().copied())
                .collect();
            for dst in &operands[pc].1 {
                input.remove(&dst.index);
            }
            input.extend(operands[pc].0.iter().map(|s| s.index));
            if input != live[pc] {
                live[pc] = input;
                changed = true;
            }
        }
        if !changed {
            return Some(live);
        }
    }
}

fn aliases(body: &FunctionPlan, dst: &Slot) -> Option<Set> {
    let mut aliases = Set::from([dst.index]);
    let mut budget = 2_000_000usize;
    loop {
        let mut changed = false;
        for op in &body.ops {
            budget = budget.checked_sub(1)?;
            let edge = match op {
                Op::Copy(dst, src)
                | Op::Reference { dst, src, .. }
                | Op::Reserve { dst, src }
                | Op::Buffer(BufferOp::Borrow {
                    dst,
                    src,
                    place: None,
                    ..
                }) => Some((src, dst)),
                // A full-range subslice can reuse its input descriptor.
                Op::Buffer(BufferOp::Storage {
                    intrinsic,
                    args,
                    dst: Some(dst),
                    ..
                }) if is_slice(*intrinsic) => Some((&args[0], dst)),
                _ => None,
            };
            if let Some((src, dst)) = edge {
                for (src, dst) in src.scalars().zip(dst.scalars()) {
                    if aliases.contains(&src.index) {
                        changed |= aliases.insert(dst.index);
                    }
                }
            }
        }
        if !changed {
            return Some(aliases);
        }
    }
}

fn nonescaping(
    body: &FunctionPlan,
    functions: &HashMap<String, FunctionInfo>,
    aliases: &Set,
) -> bool {
    if body
        .result
        .as_ref()
        .is_some_and(|s| s.scalars().any(|s| aliases.contains(&s.index)))
    {
        return false;
    }
    body.ops.iter().all(|op| {
        if !borrowing::operands(op)
            .0
            .iter()
            .any(|s| aliases.contains(&s.index))
        {
            return true;
        }
        match op {
            Op::Copy(..)
            | Op::Reference { .. }
            | Op::Reserve { .. }
            | Op::Buffer(BufferOp::Borrow { place: None, .. } | BufferOp::Access { .. }) => true,
            Op::Buffer(BufferOp::Storage {
                intrinsic, args, ..
            }) => {
                (is_slice(*intrinsic)
                    || matches!(
                        intrinsic,
                        BufferIntrinsic::SliceLen
                            | BufferIntrinsic::SliceLoad
                            | BufferIntrinsic::SliceStore
                            | BufferIntrinsic::SliceBorrow
                            | BufferIntrinsic::SliceBorrowMut
                            | BufferIntrinsic::SliceBorrowRef
                            | BufferIntrinsic::SliceBorrowMutRef
                    ))
                    && !args
                        .iter()
                        .skip(1)
                        .any(|s| s.scalars().any(|s| aliases.contains(&s.index)))
            }
            Op::Call {
                indirect: None,
                name,
                args,
                ..
            } => {
                let info = &functions[name];
                !info.uses_concurrency
                    && borrowing::call_arguments(info, name, args).into_iter().all(
                        |(index, arg)| {
                            !arg.as_ref()
                                .is_some_and(|s| s.scalars().any(|s| aliases.contains(&s.index)))
                                || index.is_some_and(|index| info.parameter_noescape(index))
                        },
                    )
            }
            _ => false,
        }
    })
}

pub(super) fn lower(
    functions: &HashMap<String, FunctionInfo>,
    bodies: &mut HashMap<String, FunctionPlan>,
) {
    let mut factories = HashMap::new();
    // Standard forwarding layers are shallow. Deeper/cyclic wrappers retain
    // their existing behavior instead of requiring general-purpose inlining.
    for _ in 0..8 {
        let found: Vec<_> = bodies
            .iter()
            .filter(|(name, _)| !factories.contains_key(*name))
            .filter_map(|(name, body)| {
                factory(&functions[name], body, &factories).map(|f| (name.clone(), f))
            })
            .collect();
        if found.is_empty() {
            break;
        }
        factories.extend(found);
    }
    for body in bodies.values_mut() {
        for (op, span) in body.ops.iter_mut().zip(&mut body.op_spans) {
            if let Op::Call {
                indirect: None,
                name,
                args,
                result,
                state: None,
            } = op
            {
                if let Some(factory) = factories.get(name) {
                    let args: Vec<_> = factory
                        .arguments
                        .iter()
                        .map(|&i| args[i].clone().unwrap())
                        .collect();
                    *op = match &factory.construction {
                        Construction::Buffer { intrinsic, element } => {
                            Op::Buffer(BufferOp::Storage {
                                intrinsic: *intrinsic,
                                element: element.clone(),
                                args,
                                dst: result.clone(),
                                scratch: None,
                            })
                        }
                        Construction::Fixed { reference } => Op::Place(PlaceOp::View {
                            reference: Slot {
                                index: args[0].index,
                                ty: reference.clone(),
                            },
                            start: args[1].clone(),
                            end: Some(args[2].clone()),
                            dst: result.clone().unwrap(),
                            scratch: None,
                        }),
                    };
                    *span = factory.span;
                }
            }
        }
        let candidates: Vec<_> = body
            .ops
            .iter()
            .enumerate()
            .filter_map(|(pc, op)| match op {
                Op::Buffer(BufferOp::Storage {
                    intrinsic,
                    dst: Some(dst),
                    ..
                }) if is_slice(*intrinsic) => Some((pc, dst.clone())),
                Op::Place(PlaceOp::View {
                    end: Some(_), dst, ..
                }) => Some((pc, dst.clone())),
                _ => None,
            })
            .take(64)
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let Some(live) = liveness(body) else {
            continue;
        };
        let safe: Set = candidates
            .into_iter()
            .filter_map(|(pc, dst)| {
                let aliases = aliases(body, &dst)?;
                (live[pc].is_disjoint(&aliases) && nonescaping(body, functions, &aliases))
                    .then_some(pc)
            })
            .collect();
        if safe.is_empty() {
            continue;
        }

        // Share the existing local-storage paths in all backends. This private
        // Copy tuple is raw space, never a source-visible Buffer value or owner.
        let bytes = std::mem::size_of::<crate::runtime::buffer::Buffer>();
        assert!(
            std::mem::align_of::<crate::runtime::buffer::Buffer>() <= std::mem::align_of::<u64>()
        );
        let Ok(target) = crate::core::types::tuple_type(
            vec![Type::UInt64; bytes.div_ceil(8)],
            &body.type_registry,
        ) else {
            continue;
        };
        let local_bytes = body.ops.iter().try_fold(0usize, |total, op| {
            let local = match op {
                Op::Local { target, .. } if !target.is_place() => Some(target),
                _ => None,
            };
            local
                .into_iter()
                .chain(op.cleanup_storage().map(|(_, ty)| ty))
                .try_fold(total, |total, ty| {
                    let layout = ty.storage_layout().ok()?;
                    total.checked_add(layout.size.max(1) + layout.align.max(8) - 1)
                })
        });
        let Some(mut remaining) = local_bytes.and_then(|n| (64 * 1024usize).checked_sub(n)) else {
            continue;
        };
        let mut ops = Vec::new();
        let mut spans = Vec::new();
        for (pc, (mut op, span)) in std::mem::take(&mut body.ops)
            .into_iter()
            .zip(std::mem::take(&mut body.op_spans))
            .enumerate()
        {
            if safe.contains(&pc) && remaining >= bytes.div_ceil(8) * 8 + 7 {
                if let Ok(storage) = body.slot(Type::reference(target.clone(), true)) {
                    remaining -= bytes.div_ceil(8) * 8 + 7;
                    ops.push(Op::Local {
                        dst: storage.clone(),
                        value: None,
                        target: target.clone(),
                    });
                    spans.push(span);
                    match &mut op {
                        Op::Buffer(BufferOp::Storage { scratch, .. })
                        | Op::Place(PlaceOp::View { scratch, .. }) => *scratch = Some(storage),
                        _ => unreachable!("proved slice construction"),
                    }
                }
            }
            ops.push(op);
            spans.push(span);
        }
        body.ops = ops;
        body.op_spans = spans;
    }
}
