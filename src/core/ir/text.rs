//! Trusted UTF-8 storage bridges. Their identities are assigned by the embedded library.
use super::*;

impl FunctionPlan {
    pub(super) fn text_body(&mut self, f: &FnDef) -> Result<bool, Error> {
        let Some(intrinsic) = &f.compiler_intrinsic else {
            return Ok(false);
        };
        if !matches!(
            intrinsic,
            CompilerIntrinsic::TextClone
                | CompilerIntrinsic::TextFreeze
                | CompilerIntrinsic::TextIntoBytes
                | CompilerIntrinsic::TextBytes
        ) {
            return Ok(false);
        }
        let [parameter] = f.params.as_slice() else {
            return Err(type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Invalid text bridge signature",
            ));
        };
        let bytes_owner = |ty: &Type| matches!(ty.scalar_types(), [Type::Buffer(element)] if **element == Type::UInt8);
        let valid = !f.suspends
            && match intrinsic {
                CompilerIntrinsic::TextClone => {
                    parameter.ty == Type::reference(Type::String, false) && f.ret_ty == Type::String
                }
                CompilerIntrinsic::TextFreeze => {
                    bytes_owner(&parameter.ty) && f.ret_ty == Type::String
                }
                CompilerIntrinsic::TextIntoBytes => {
                    parameter.ty == Type::String && bytes_owner(&f.ret_ty)
                }
                CompilerIntrinsic::TextBytes => {
                    parameter.ty == Type::reference(Type::String, false)
                        && f.ret_ty
                            == Type::reference(Type::BufferSlice(Arc::new(Type::UInt8)), false)
                }
                _ => unreachable!(),
            };
        if !valid {
            return Err(type_error(
                &f.span,
                ErrorCode::InvalidCall,
                "Invalid text bridge signature",
            ));
        }
        let src = Slot {
            index: 0,
            ty: parameter.ty.clone(),
        };
        let dst = self.slot(f.ret_ty.clone())?;
        match intrinsic {
            CompilerIntrinsic::TextClone => {
                self.emit(Op::Text {
                    dst: dst.clone(),
                    source: TextSource::Clone(src),
                });
                self.register_owner(dst.clone(), None);
            }
            CompilerIntrinsic::TextFreeze | CompilerIntrinsic::TextIntoBytes => {
                self.register_owner(src.clone(), None);
                self.emit(Op::Buffer(BufferOp::Move {
                    dst: dst.clone(),
                    src,
                }));
            }
            CompilerIntrinsic::TextBytes => self.emit(Op::Reference {
                dst: dst.clone(),
                src,
                moving: false,
            }),
            _ => unreachable!(),
        }
        self.result = Some(dst);
        self.resolve_cleanups()?;
        Ok(true)
    }
}
