//! Typed Script and domain calls use private, frame-owned value packets.
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
            CompilerIntrinsic::ScriptPack | CompilerIntrinsic::DomainValuePack => {
                &info.signature.params[0]
            }
            CompilerIntrinsic::ScriptUnpack | CompilerIntrinsic::DomainValueUnpack => {
                &info.signature.ret
            }
            _ => return Ok(None),
        };
        let domain = matches!(
            kind,
            CompilerIntrinsic::DomainValuePack | CompilerIntrinsic::DomainValueUnpack
        );
        let checking = self.generics.borrow().checking;
        let schema = if checking {
            String::new()
        } else {
            let contract = crate::core::script::domain_schema(ty).filter(|_| self.domain_value(ty));
            contract.ok_or_else(|| type_error(&span, ErrorCode::InvalidCall, if domain {
                "Domain contracts require bounded values without Drop: scalars, String, Array, tuples, concrete records or enums (at most 64 contract members, 64 lanes and 8 aggregate levels)"
            } else {
                "Script contracts require bounded values without Drop: scalars, String, Array, tuples, concrete records or enums (at most 64 contract members, 64 lanes and 8 aggregate levels)"
            }))?
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
        let packing = matches!(
            kind,
            CompilerIntrinsic::ScriptPack | CompilerIntrinsic::DomainValuePack
        );
        if packing {
            if let Some(value) = values[0].as_ref().filter(|v| v.ty.is_owner()) {
                self.register_owner(value.clone(), None);
            }
        }
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
                scratch: None,
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
                reservation: false,
                dst: reference.clone(),
                src,
                place: None,
            }));
            reference
        } else {
            values[0].clone().unwrap()
        };
        if checking && !packing {
            // Decoding produces independent owners even before their concrete
            // schema and number of lanes are known.
            for (index, lane) in dst.iter().flat_map(Slot::scalars).enumerate() {
                if lane.ty.is_owner() {
                    self.emit(Op::DomainOwner {
                        owner: lane.ty.clone(),
                        packet: packet.clone(),
                        index,
                        value: lane.clone(),
                        decode: true,
                    });
                    self.register_owner(lane, None);
                }
            }
        }
        let lanes: Vec<_> = if checking {
            Vec::new()
        } else if packing {
            values[0].iter().flat_map(Slot::scalars).collect()
        } else {
            dst.iter().flat_map(Slot::scalars).collect()
        };
        for (index, lane) in lanes.iter().cloned().enumerate() {
            let bits = self.slot(Type::UInt64)?;
            if packing {
                if lane.ty.is_owner_lane() {
                    self.emit(Op::Literal(bits.clone(), Constant::UInt64(0)));
                } else {
                    self.emit(Op::DomainBits {
                        dst: bits.clone(),
                        src: lane,
                        decode: false,
                    });
                }
                self.emit(Op::Buffer(BufferOp::Storage {
                    scratch: None,
                    intrinsic: BufferIntrinsic::InitializeNext,
                    element: Type::UInt64,
                    args: vec![packet.clone(), bits],
                    dst: None,
                }));
            } else if lane.ty.is_owner_lane() {
                self.emit(Op::Literal(lane, Constant::UInt64(0)));
            } else {
                let at = self.slot(Type::UInt64)?;
                self.emit(Op::Literal(at.clone(), Constant::UInt64(index as u64)));
                self.emit(Op::Buffer(BufferOp::Storage {
                    scratch: None,
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
        for (index, lane) in lanes
            .into_iter()
            .enumerate()
            .filter(|(_, lane)| lane.ty.is_owner_lane())
        {
            self.emit(Op::DomainOwner {
                owner: super::domain_schema::freeze(self, &lane.ty)?,
                packet: packet.clone(),
                index,
                value: lane.clone(),
                decode: !packing,
            });
            if !packing {
                self.register_owner(lane, None);
            }
        }
        if packing {
            if let Some(value) = values[0].as_ref().filter(|v| v.ty.is_owner()) {
                self.emit(Op::EndLocal(value.clone()));
            }
        } else if let Some(value) = dst.as_ref().filter(|v| v.ty.is_owner()) {
            self.register_owner(value.clone(), None);
        }
        Ok(Some(Flow::Value(dst)))
    }
}
