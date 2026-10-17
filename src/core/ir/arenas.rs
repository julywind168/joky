//! Explicit arena construction and allocation retain compiler-owned identities.
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
                if !value.ty.is_copy_storage_value() || value.ty.reference_type().is_some() {
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
