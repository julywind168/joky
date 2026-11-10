//! Explicit arena intrinsics and inference of nonescaping implicit call regions.
use super::*;

impl FunctionPlan {
    pub(super) fn arena_body(&mut self, f: &FnDef) -> Result<bool, Error> {
        let Some(intrinsic @ (CompilerIntrinsic::ArenaNew | CompilerIntrinsic::ArenaAlloc)) =
            &f.compiler_intrinsic
        else {
            return Ok(false);
        };
        let arena = Type::Resource("arena.Arena".into());
        let invalid = || {
            type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Invalid explicit arena intrinsic signature",
            )
        };
        if f.suspends {
            return Err(invalid());
        }
        let dst = self.slot(f.ret_ty.clone())?;
        match intrinsic {
            CompilerIntrinsic::ArenaNew => {
                if !f.params.is_empty() || f.ret_ty != arena {
                    return Err(invalid());
                }
                self.emit(Op::ArenaNew { dst: dst.clone() });
                self.register_owner(dst.clone(), None);
            }
            CompilerIntrinsic::ArenaAlloc => {
                let [region, value] = f.params.as_slice() else {
                    return Err(invalid());
                };
                if region.ty != Type::reference(arena, false)
                    || f.ret_ty != Type::reference(value.ty.clone(), true)
                {
                    return Err(invalid());
                }
                if value.ty.contains_place_storage()
                    || !value.ty.is_copy_storage_value()
                    || value.ty.reference_type().is_some()
                {
                    return Err(type_error(
                        &f.span,
                        ErrorCode::TypeMismatch,
                        "Arena.alloc requires Copy data, not a reference, String or opaque handle",
                    ));
                }
                self.emit(Op::ArenaAlloc {
                    dst: dst.clone(),
                    arena: Slot {
                        index: 0,
                        ty: region.ty.clone(),
                    },
                    value: (value.ty.parameter_slots() != 0).then(|| Slot {
                        index: 1,
                        ty: value.ty.clone(),
                    }),
                    target: value.ty.clone(),
                });
            }
            _ => unreachable!(),
        }
        self.result = Some(dst);
        self.resolve_cleanups()?;
        Ok(true)
    }
}

// Addresses, owner handles and fixed-array places do not cross a region boundary.
fn plain_region_value(ty: &Type) -> bool {
    ty.is_copy_storage_value()
        && !ty.is_place()
        && !ty.contains_references()
        && ty.scalar_types().iter().all(crate::core::instance::scalar)
}

/// Only inspect ordinary memory and scalar operations. In particular, an
/// opaque call or task operation could publish an arena address.
/// Propagate exclusions backwards, including through recursive call cycles.
pub(super) fn infer_call_regions(
    functions: &mut HashMap<String, FunctionInfo>,
    bodies: &HashMap<String, FunctionPlan>,
) {
    use std::collections::HashSet;
    let mut callers: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut excluded = HashSet::new();
    let mut pending = Vec::new();
    for (name, body) in bodies {
        let info = &functions[name];
        let mut safe = !info.suspends
            && info.inferred_retained.as_ref().is_some_and(Vec::is_empty)
            && info.inferred_arena_retained.is_empty();
        for op in &body.ops {
            safe &= match op {
                Op::Call {
                    indirect: None,
                    state: None,
                    name: callee,
                    ..
                } => {
                    callers.entry(callee).or_default().push(name);
                    bodies.contains_key(callee)
                }
                Op::Local { target, .. } | Op::WriteMemory { target, .. } => {
                    plain_region_value(target)
                }
                Op::Copy(..)
                | Op::Reference { .. }
                | Op::Reserve { .. }
                | Op::Project { .. }
                | Op::PatternProject { .. }
                | Op::EndLocal(_)
                | Op::Alloc { .. }
                | Op::ReadMemory { .. }
                | Op::Label(_)
                | Op::Jump(_)
                | Op::Branch { .. }
                | Op::Literal(..)
                | Op::Binary(..)
                | Op::Unary(..)
                | Op::Convert(..) => true,
                _ => false,
            };
        }
        if !safe {
            excluded.insert(name.as_str());
            pending.push(name.as_str());
        }
    }
    while let Some(name) = pending.pop() {
        for &caller in callers.get(name).into_iter().flatten() {
            if excluded.insert(caller) {
                pending.push(caller);
            }
        }
    }
    for name in bodies.keys() {
        let info = functions.get_mut(name).unwrap();
        info.call_region = info.uses_arena
            && !excluded.contains(name.as_str())
            && info
                .signature
                .params
                .iter()
                .chain([&info.signature.ret])
                .all(plain_region_value);
    }
}
