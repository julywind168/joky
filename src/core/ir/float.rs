//! Trusted IEEE float primitives bound by identity to embedded definitions.
use super::*;

impl FunctionPlan {
    pub(super) fn float_body(&mut self, f: &FnDef) -> Result<bool, Error> {
        let (kind, to_text) = match &f.compiler_intrinsic {
            Some(CompilerIntrinsic::Float(kind)) => (Some(*kind), false),
            Some(CompilerIntrinsic::FloatToText) => (None, true),
            Some(CompilerIntrinsic::IntFromFloat) => (None, false),
            _ => return Ok(false),
        };
        let [parameter] = f.params.as_slice() else {
            return Err(type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Invalid float primitive signature",
            ));
        };
        let src = Slot {
            index: 0,
            ty: parameter.ty.clone(),
        };
        let valid = !f.suspends
            && if to_text {
                parameter.ty.is_float() && f.ret_ty == Type::String
            } else if let Some(kind) = kind {
                match kind {
                    FloatKind::FromText32 => {
                        parameter.ty == Type::reference(Type::String, false)
                            && f.ret_ty == Type::Float32
                    }
                    FloatKind::FromText => {
                        parameter.ty == Type::reference(Type::String, false)
                            && f.ret_ty == Type::Float64
                    }
                    FloatKind::Sqrt
                    | FloatKind::Floor
                    | FloatKind::Ceil
                    | FloatKind::Trunc
                    | FloatKind::RoundTiesEven => {
                        parameter.ty.is_float() && parameter.ty == f.ret_ty
                    }
                    FloatKind::ToBits => matches!(
                        (&parameter.ty, &f.ret_ty),
                        (Type::Float64, Type::UInt64) | (Type::Float32, Type::UInt32)
                    ),
                    FloatKind::FromBits => matches!(
                        (&parameter.ty, &f.ret_ty),
                        (Type::UInt64, Type::Float64) | (Type::UInt32, Type::Float32)
                    ),
                }
            } else {
                parameter.ty.is_float() && f.ret_ty.integer().is_some()
            };
        if !valid {
            return Err(type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Invalid float primitive signature",
            ));
        }
        let dst = self.slot(f.ret_ty.clone())?;
        if to_text {
            // Register cleanup only after the producing operation, matching
            // the text bridges; the register op reads the result slot.
            self.emit(Op::Text {
                dst: dst.clone(),
                source: TextSource::Float(src),
            });
            self.register_owner(dst.clone(), None);
        } else if let Some(kind) = kind {
            self.emit(Op::Float {
                dst: dst.clone(),
                src,
                kind,
            });
        } else {
            self.emit(Op::Convert(dst.clone(), src));
        }
        self.result = Some(dst);
        self.resolve_cleanups()?;
        Ok(true)
    }
}
