//! Explicit interpreter place operations. Cloning a VM register never copies a
//! language array: language copies always execute this typed copy program.
use super::*;
use crate::core::ir::PlaceOp;
use crate::runtime::{
    arena::Arena,
    place::{CopyNode, Places},
};

pub(super) struct Instruction {
    op: PlaceOp,
    layout: Rc<Layout>,
}

pub(super) struct Layout {
    size: usize,
    align: usize,
    length: u64,
    stride: usize,
    pub(super) program: Rc<[CopyNode]>,
    fields: Vec<(usize, Type)>,
}

#[derive(Default)]
pub(super) struct Layouts(Vec<(Type, Rc<Layout>)>);
impl Layouts {
    pub(super) fn get(&mut self, ty: &Type) -> Rc<Layout> {
        if let Some((_, layout)) = self.0.iter().find(|(known, _)| known == ty) {
            return layout.clone();
        }
        let shape = ty.memory_layout().unwrap();
        let element = &ty.fixed_shape().element;
        let layout = Rc::new(Layout {
            size: shape.size,
            align: shape.align,
            length: ty.fixed_length(),
            stride: element.memory_layout().unwrap().size,
            program: ty.place_copy_program().into(),
            fields: element.storage_scalars().unwrap(),
        });
        self.0.push((ty.clone(), layout.clone()));
        layout
    }
}
pub(super) fn lower(op: &PlaceOp, layouts: &mut Layouts) -> Instruction {
    Instruction {
        op: op.clone(),
        layout: layouts.get(op.ty()),
    }
}

pub(super) fn pointer(value: &Value) -> *mut u8 {
    match value {
        Value::Place(pointer) | Value::Reference(pointer) => *pointer,
        _ => unreachable!("checked place register"),
    }
}

pub(super) fn execute(
    op: &Instruction,
    slots: &mut [Value],
    arena: &mut Arena,
    places: &mut Places,
) -> Result<(), Error> {
    match &op.op {
        PlaceOp::Swap {
            left,
            right,
            temporary,
            ..
        } => {
            let temp = places.materialize(arena, *temporary, op.layout.size, op.layout.align)?;
            unsafe {
                crate::runtime::place::copy(
                    temp.as_ptr(),
                    pointer(&slots[left.index]),
                    &op.layout.program,
                );
                crate::runtime::place::copy(
                    pointer(&slots[left.index]),
                    pointer(&slots[right.index]),
                    &op.layout.program,
                );
                crate::runtime::place::copy(
                    pointer(&slots[right.index]),
                    temp.as_ptr(),
                    &op.layout.program,
                );
                places.end(arena, *temporary);
            }
        }
        PlaceOp::Alias { dst, src } => {
            slots[dst.index] = Value::Reference(pointer(&slots[src.index]))
        }
        PlaceOp::Read { dst, reference } if matches!(dst.ty, Type::FixedArrayInit(_)) => {
            slots[dst.index] = Value::Place(pointer(&slots[reference.index]))
        }
        PlaceOp::Allocate(dst) => {
            let p = places.materialize(arena, dst.index, op.layout.size, op.layout.align)?;
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::Copy { dst, src }
        | PlaceOp::Read {
            dst,
            reference: src,
        } => {
            let source = pointer(&slots[src.index]);
            let p = places.materialize(arena, dst.index, op.layout.size, op.layout.align)?;
            unsafe {
                crate::runtime::place::copy(p.as_ptr(), source, &op.layout.program);
            }
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::Write { reference, value } => unsafe {
            crate::runtime::place::copy(
                pointer(&slots[reference.index]),
                pointer(&slots[value.index]),
                &op.layout.program,
            );
        },
        PlaceOp::Begin(dst) => {
            let p = places.begin(
                arena,
                dst.index,
                op.layout.size,
                op.layout.align,
                op.layout.length,
            )?;
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::MoveInit { dst, src, publish } => {
            let p = places.move_value(arena, dst.index, pointer(&slots[src.index]), *publish)?;
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::End(slot) => unsafe {
            places.end(arena, slot.index);
        },
        PlaceOp::WriteNext { reference, value }
        | PlaceOp::Store {
            reference, value, ..
        }
        | PlaceOp::Load {
            reference,
            dst: value,
            ..
        } => {
            let base = pointer(&slots[reference.index]);
            let p = if matches!(&op.op, PlaceOp::WriteNext { .. }) {
                places.next(base, op.layout.stride)?
            } else {
                let index = match &op.op {
                    PlaceOp::Store { index, .. } | PlaceOp::Load { index, .. } => index,
                    _ => unreachable!(),
                };
                let Value::UInt64(index) = slots[index.index] else {
                    unreachable!("checked index")
                };
                unsafe { base.add(index as usize * op.layout.stride) }
            };
            if let Some(value) = value {
                for ((offset, ty), slot) in op.layout.fields.iter().zip(value.scalars()) {
                    unsafe {
                        if matches!(&op.op, PlaceOp::Load { .. }) {
                            slots[slot.index] = memory::read_scalar(p.add(*offset), ty);
                        } else {
                            memory::write_scalar(p.add(*offset), &slots[slot.index]);
                        }
                    }
                }
            }
            if matches!(&op.op, PlaceOp::WriteNext { .. }) {
                places.commit_pointer(base)?;
            }
        }
    }
    Ok(())
}
