//! Typed Script calls use a private, frame-owned scalar packet.
use super::*;
use crate::parser::ast::BufferIntrinsic;

impl FunctionPlan {
    pub(super) fn script_call(
        &mut self,
        info: &FunctionInfo,
        values: &[Option<Slot>],
        span: Span,
    ) -> Result<Option<Flow>, Error> {
        let Some(kind) = &info.compiler_intrinsic else {
            return Ok(None);
        };
        let ty = match kind {
            CompilerIntrinsic::ScriptSchema(ty) => ty,
            CompilerIntrinsic::ScriptPack => &info.signature.params[0],
            CompilerIntrinsic::ScriptUnpack => &info.signature.ret,
            _ => return Ok(None),
        };
        let checking = self.generics.borrow().checking;
        let schema = if checking {
            String::new()
        } else {
            crate::core::script::schema(ty).filter(|_| !self.has_custom_drop(ty)).ok_or_else(|| type_error(&span, ErrorCode::InvalidCall,
                "Script contracts require scalars or flat scalar records without Drop (at most 64 fields)"))?
        };
        if matches!(kind, CompilerIntrinsic::ScriptSchema(_)) {
            let dst = self.slot(Type::String)?;
            self.emit(Op::Text {
                dst: dst.clone(),
                source: TextSource::Literal(schema),
            });
            self.register_owner(dst.clone(), None);
            return Ok(Some(Flow::Value(Some(dst))));
        }
        let packing = matches!(kind, CompilerIntrinsic::ScriptPack);
        let dst = if packing {
            Some(self.slot(info.signature.ret.clone())?)
        } else if *ty == Type::Unit {
            None
        } else {
            Some(self.slot(ty.clone())?)
        };
        if packing {
            let count = self.slot(Type::UInt64)?;
            self.emit(Op::Literal(
                count.clone(),
                Constant::UInt64(crate::core::script::MAX_LANES as u64),
            ));
            self.emit(Op::Buffer(BufferOp::Storage {
                intrinsic: BufferIntrinsic::Allocate,
                element: Type::UInt64,
                args: vec![count],
                dst: dst.as_ref().and_then(|s| s.scalars().next()),
            }));
            self.register_owner(dst.clone().unwrap(), None);
        }
        let packet = if packing {
            let src = dst.as_ref().unwrap().scalars().next().unwrap();
            let reference = self.slot(Type::reference(src.ty.clone(), true))?;
            self.emit(Op::Buffer(BufferOp::Borrow {
                dst: reference.clone(),
                src,
                place: None,
            }));
            reference
        } else {
            values[0].clone().unwrap()
        };
        let lanes: Vec<_> = if checking {
            Vec::new()
        } else if packing {
            values[0].iter().flat_map(Slot::scalars).collect()
        } else {
            dst.iter().flat_map(Slot::scalars).collect()
        };
        for (index, lane) in lanes.into_iter().enumerate() {
            let bits = self.slot(Type::UInt64)?;
            if packing {
                self.emit(Op::DomainBits {
                    dst: bits.clone(),
                    src: lane,
                    decode: false,
                });
                self.emit(Op::Buffer(BufferOp::Storage {
                    intrinsic: BufferIntrinsic::InitializeNext,
                    element: Type::UInt64,
                    args: vec![packet.clone(), bits],
                    dst: None,
                }));
            } else {
                let at = self.slot(Type::UInt64)?;
                self.emit(Op::Literal(at.clone(), Constant::UInt64(index as u64)));
                self.emit(Op::Buffer(BufferOp::Storage {
                    intrinsic: BufferIntrinsic::Load,
                    element: Type::UInt64,
                    args: vec![values[0].clone().unwrap(), at],
                    dst: Some(bits.clone()),
                }));
                self.emit(Op::DomainBits {
                    dst: lane,
                    src: bits,
                    decode: true,
                });
            }
        }
        Ok(Some(Flow::Value(dst)))
    }
}
