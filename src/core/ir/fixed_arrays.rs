//! Compact physical values and trusted fixed-array construction operations.
use super::*;

#[derive(Clone)]
pub(crate) enum PlaceOp {
    View {
        reference: Slot,
        start: Slot,
        end: Option<Slot>,
        dst: Slot,
    },
    Swap {
        left: Slot,
        right: Slot,
        target: Type,
        temporary: usize,
    },
    Alias {
        dst: Slot,
        src: Slot,
    },
    Read {
        offset: usize,
        dst: Slot,
        reference: Slot,
    },
    Write {
        offset: usize,
        reference: Slot,
        value: Slot,
    },
    Allocate(Slot),
    Copy {
        dst: Slot,
        src: Slot,
    },
    MoveInit {
        dst: Slot,
        src: Slot,
        publish: bool,
    },
    Begin(Slot),
    WriteNext {
        reference: Slot,
        value: Option<Slot>,
    },
    Load {
        reference: Slot,
        index: Slot,
        dst: Option<Slot>,
    },
    Store {
        reference: Slot,
        index: Slot,
        value: Option<Slot>,
    },
    End(Slot),
}

pub(super) fn intrinsic(value: &Option<CompilerIntrinsic>) -> bool {
    matches!(
        value,
        Some(
            CompilerIntrinsic::FixedArrayBegin
                | CompilerIntrinsic::FixedArrayWriteNext
                | CompilerIntrinsic::FixedArrayFinish
                | CompilerIntrinsic::FixedArrayLoad
                | CompilerIntrinsic::FixedArrayStore
                | CompilerIntrinsic::FixedArrayBorrow
                | CompilerIntrinsic::FixedArraySlice
        )
    )
}

pub(super) fn eligible(ty: &Type) -> bool {
    let target = ty.reference_type().map_or(ty, |r| &r.target);
    match target {
        Type::Function(f) => {
            !f.suspends
                && f.signature
                    .params
                    .iter()
                    .chain(std::iter::once(&f.signature.ret))
                    .all(|t| !t.contains_fixed_array() || eligible(t))
        }
        Type::FixedArrayInit(_) => true,
        Type::Buffer(t)
        | Type::BufferSlice(t)
        | Type::BufferElement(t)
        | Type::BufferIterator(t) => t.is_place() && eligible(t),
        Type::Record(r) if r.array => r
            .arguments
            .iter()
            .filter_map(|a| a.as_type())
            .all(|t| t.is_place() && eligible(t)),
        _ => {
            matches!(
                target,
                Type::FixedArray(_) | Type::Record(_) | Type::Tuple(_) | Type::Enum(_)
            ) && target.is_fixed_array_data()
        }
    }
}

impl FunctionPlan {
    pub(super) fn place_read(
        &mut self,
        base: &Slot,
        offset: usize,
        ty: Type,
    ) -> Result<Option<Slot>, Error> {
        if ty == Type::Unit {
            return Ok(None);
        }
        let dst = self.slot(ty)?;
        self.emit(Op::Place(PlaceOp::Read {
            dst: dst.clone(),
            reference: base.clone(),
            offset,
        }));
        Ok(Some(dst))
    }
    pub(super) fn place_write(&mut self, base: &Slot, offset: usize, value: Option<Slot>) {
        if let Some(value) = value {
            self.emit(Op::Place(PlaceOp::Write {
                reference: base.clone(),
                value,
                offset,
            }));
        }
    }
    pub(super) fn place_child(&mut self, base: &Slot, child: usize) -> Result<Option<Slot>, Error> {
        let (offset, ty) = base.ty.place_child(child);
        self.place_read(base, offset, ty)
    }

    pub(super) fn check_fixed_array(&self, ty: &Type) -> Result<(), Error> {
        if ty.contains_fixed_array() && (!eligible(ty) || self.suspending) {
            return Err(type_error(&self.current_span, ErrorCode::TypeMismatch,
                format!("FixedArray is not supported in this context: {ty}; requires supported synchronous array values or views")));
        }
        Ok(())
    }

    pub(super) fn fixed_array_call(
        &mut self,
        info: &FunctionInfo,
        values: Vec<Option<Slot>>,
    ) -> Result<Flow, Error> {
        let mut offset = 0;
        let values: Vec<_> = info
            .signature
            .params
            .iter()
            .map(|ty| {
                let value = values.get(offset).and_then(Clone::clone).map(|slot| Slot {
                    index: slot.index,
                    ty: ty.clone(),
                });
                offset += ty.parameter_slots();
                value
            })
            .collect();
        let result = if info.signature.ret == Type::Unit {
            None
        } else {
            Some(self.slot(info.signature.ret.clone())?)
        };
        let op = match info.compiler_intrinsic.as_ref().unwrap() {
            CompilerIntrinsic::FixedArrayBorrow | CompilerIntrinsic::FixedArraySlice => {
                PlaceOp::View {
                    reference: values[0].clone().unwrap(),
                    start: values[1].clone().unwrap(),
                    end: values.get(2).cloned().flatten(),
                    dst: result.clone().unwrap(),
                }
            }
            CompilerIntrinsic::FixedArrayBegin => PlaceOp::Begin(result.clone().unwrap()),
            CompilerIntrinsic::FixedArrayFinish => PlaceOp::MoveInit {
                dst: result.clone().unwrap(),
                src: values[0].clone().unwrap(),
                publish: true,
            },
            CompilerIntrinsic::FixedArrayWriteNext => PlaceOp::WriteNext {
                reference: values[0].clone().unwrap(),
                value: values[1].clone(),
            },
            CompilerIntrinsic::FixedArrayLoad => PlaceOp::Load {
                reference: values[0].clone().unwrap(),
                index: values[1].clone().unwrap(),
                dst: result.clone(),
            },
            CompilerIntrinsic::FixedArrayStore => PlaceOp::Store {
                reference: values[0].clone().unwrap(),
                index: values[1].clone().unwrap(),
                value: values[2].clone(),
            },
            _ => unreachable!(),
        };
        self.emit(Op::Place(op));
        Ok(Flow::Value(result))
    }
}

impl Type {
    pub(crate) fn place_child(&self, index: usize) -> (usize, Type) {
        use crate::core::types::memory_layout::MemoryLayoutShape as Shape;
        let layout = self.memory_layout().unwrap();
        match (self, &layout.shape) {
            (Self::Record(r), Shape::Record(fields)) => {
                (fields[index].offset, r.fields[index].ty.clone())
            }
            (Self::Tuple(t), Shape::Tuple(fields)) => {
                (fields[index].offset, t.elements[index].clone())
            }
            (Self::Enum(e), Shape::Enum { variants, .. }) => (
                variants[index].payload.map_or(0, |p| p.offset),
                e.variants[index].payload.clone().unwrap_or(Self::Unit),
            ),
            _ => unreachable!("checked aggregate projection"),
        }
    }

    /// One descriptor subtree per element shape, independent of array length.
    pub(crate) fn place_copy_program(&self) -> Vec<crate::runtime::place::CopyNode> {
        use crate::core::types::memory_layout::MemoryLayoutShape as Shape;
        use crate::runtime::place::CopyNode;
        fn build(ty: &Type, offset: usize, nodes: &mut Vec<CopyNode>) {
            let layout = ty.memory_layout().expect("validated concrete place layout");
            let index = nodes.len();
            nodes.push(CopyNode {
                offset: offset as u64,
                kind: 0,
                count: layout.size as u64,
                stride: 0,
                end: 0,
            });
            match (&layout.shape, ty) {
                (
                    Shape::Array { length, stride, .. },
                    Type::FixedArray(a) | Type::FixedArrayInit(a),
                ) => {
                    nodes[index].kind = 2;
                    nodes[index].count = *length;
                    nodes[index].stride = *stride as u64;
                    build(&a.element, 0, nodes);
                }
                (Shape::Record(fields), Type::Record(r)) => {
                    nodes[index].kind = 1;
                    for (field, shape) in r.fields.iter().zip(fields) {
                        build(&field.ty, shape.offset, nodes);
                    }
                }
                (Shape::Tuple(fields), Type::Tuple(t)) => {
                    nodes[index].kind = 1;
                    for (field, shape) in t.elements.iter().zip(fields) {
                        build(field, shape.offset, nodes);
                    }
                }
                (Shape::Enum { variants, .. }, Type::Enum(e)) => {
                    nodes[index].kind = 3;
                    nodes[index].count = variants.len() as u64;
                    for (variant, shape) in e.variants.iter().zip(variants) {
                        build(
                            variant.payload.as_ref().unwrap_or(&Type::Unit),
                            shape.payload.map_or(0, |p| p.offset),
                            nodes,
                        );
                    }
                }
                _ => {}
            }
            nodes[index].end = nodes.len() as u64;
        }
        let mut nodes = Vec::new();
        build(self, 0, &mut nodes);
        nodes
    }
}

impl PlaceOp {
    pub(crate) fn ty(&self) -> &Type {
        match self {
            Self::Swap { target, .. } => target,
            Self::Alias { src, .. } | Self::Copy { src, .. } => &src.ty,
            Self::Read { dst, .. }
            | Self::Allocate(dst)
            | Self::Begin(dst)
            | Self::MoveInit { dst, .. }
            | Self::End(dst) => &dst.ty,
            Self::Write { value, .. } => &value.ty,
            Self::View { reference, .. }
            | Self::WriteNext { reference, .. }
            | Self::Load { reference, .. }
            | Self::Store { reference, .. } => &reference.ty.reference_type().unwrap().target,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::ConstArgument;
    #[test]
    fn fixed_array_f2a_descriptor_and_ir_size_do_not_expand_elements() {
        use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
        let registry = TypeRegistry::default();
        let mut sizes = Vec::new();
        for n in [1, 65536] {
            let ty = registry
                .fixed_array(Type::UInt8, ConstArgument::Value(n))
                .unwrap();
            assert!(ty.scalar_types().is_empty());
            assert!(ty.storage_scalars().is_err());
            assert_eq!(ty.place_copy_program().len(), 2);
            let source = format!("import joky/fixed_array as fixed fn check() -> UInt64 {{ let a = fixed.repeat::<UInt8,{n}>(7); a.len() }}");
            let module = crate::parser::parse(&source).unwrap();
            let prepared =
                PreparedModule::new(&module, &registry, &BindingRegistry::default()).unwrap();
            let plan = prepared.plan(&Default::default()).unwrap();
            sizes.push(
                plan.bodies
                    .values()
                    .map(|body| body.ops.len())
                    .sum::<usize>(),
            );
        }
        assert_eq!(sizes[0], sizes[1]);
    }
    #[test]
    fn fixed_array_f2b_aggregate_descriptors_and_ir_remain_compact() {
        use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
        let registry = TypeRegistry::default();
        let mut sizes = Vec::new();
        for n in [1, 8192] {
            let source = format!("import joky/fixed_array as fixed.{{type FixedArray}} struct Packet {{ tag:UInt8, data:FixedArray<FixedArray<UInt8,{n}>,2> }} fn pack()->Packet {{Packet{{tag:1,data:fixed.repeat::<FixedArray<UInt8,{n}>,2>(fixed.repeat::<UInt8,{n}>(7))}}}} fn check()->UInt64 {{match Some(pack()){{Some(Packet{{tag:_,data:a}})=>a.len(),None=>0}}}}");
            let module = crate::parser::parse(&source).unwrap();
            let prepared =
                PreparedModule::new(&module, &registry, &BindingRegistry::default()).unwrap();
            let plan = prepared.plan(&Default::default()).unwrap();
            let ty = &plan.functions["pack"].signature.ret;
            assert!(ty.scalar_types().is_empty());
            assert_eq!(ty.value_lanes().len(), 1);
            assert!(ty.storage_scalars().is_err());
            sizes.push((
                ty.place_copy_program().len(),
                plan.bodies.values().map(|b| b.ops.len()).sum::<usize>(),
                plan.bodies.values().map(|b| b.slot_count).sum::<usize>(),
            ));
        }
        assert_eq!(sizes[0], sizes[1]);
    }
}
