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
    element: Option<Rc<Layout>>,
}

#[derive(Default)]
pub(super) struct Layouts(Vec<(Type, Rc<Layout>)>);
impl Layouts {
    pub(super) fn get(&mut self, ty: &Type) -> Rc<Layout> {
        if let Some((_, layout)) = self.0.iter().find(|(known, _)| known == ty) {
            return layout.clone();
        }
        let shape = ty.memory_layout().unwrap();
        let array = match ty {
            Type::FixedArray(a) | Type::FixedArrayInit(a) => Some(a),
            _ => None,
        };
        let element = array.map_or(ty, |a| &a.element);
        let child = if array.is_some() && element.is_place() {
            Some(self.get(element))
        } else {
            None
        };
        let layout = Rc::new(Layout {
            element: child,
            size: shape.size,
            align: shape.align,
            length: array.map_or(0, |_| ty.fixed_length()),
            stride: element.memory_layout().unwrap().size,
            program: ty.place_copy_program().into(),
            fields: if element.is_place() {
                Vec::new()
            } else {
                element.storage_scalars().unwrap()
            },
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
        PlaceOp::View {
            reference,
            start,
            end,
            dst,
        } => {
            let Value::UInt64(from) = slots[start.index] else {
                unreachable!("checked index")
            };
            let to = end.as_ref().map(|end| {
                let Value::UInt64(to) = slots[end.index] else {
                    unreachable!("checked index")
                };
                to
            });
            let pointer = unsafe {
                crate::runtime::buffer::fixed_view(
                    arena,
                    pointer(&slots[reference.index]),
                    op.layout.length,
                    op.layout.stride,
                    op.layout.align,
                    from,
                    to,
                )?
            };
            slots[dst.index] = Value::Reference(pointer);
        }
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
        PlaceOp::Read { dst, reference, .. } if matches!(dst.ty, Type::FixedArrayInit(_)) => {
            slots[dst.index] = Value::Place(pointer(&slots[reference.index]))
        }
        PlaceOp::Allocate(dst) => {
            let p = places.materialize(arena, dst.index, op.layout.size, op.layout.align)?;
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::Read {
            dst,
            reference,
            offset,
        } => {
            let source = unsafe { pointer(&slots[reference.index]).add(*offset) };
            if dst.ty.is_place() {
                let p = places.materialize(arena, dst.index, op.layout.size, op.layout.align)?;
                unsafe {
                    crate::runtime::place::copy(p.as_ptr(), source, &op.layout.program);
                }
                slots[dst.index] = Value::Place(p.as_ptr());
            } else {
                for ((offset, ty), slot) in op.layout.fields.iter().zip(dst.scalars()) {
                    slots[slot.index] = unsafe { memory::read_scalar(source.add(*offset), ty) };
                }
            }
        }
        PlaceOp::Copy { dst, src } => {
            let source = pointer(&slots[src.index]);
            let p = places.materialize(arena, dst.index, op.layout.size, op.layout.align)?;
            unsafe {
                crate::runtime::place::copy(p.as_ptr(), source, &op.layout.program);
            }
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::Write {
            reference,
            value,
            offset,
        } => unsafe {
            let destination = pointer(&slots[reference.index]).add(*offset);
            if value.ty.is_place() {
                crate::runtime::place::copy(
                    destination,
                    pointer(&slots[value.index]),
                    &op.layout.program,
                );
            } else {
                for ((offset, _), slot) in op.layout.fields.iter().zip(value.scalars()) {
                    memory::write_scalar(destination.add(*offset), &slots[slot.index]);
                }
            }
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
            if let Some(value) = value.as_ref().filter(|s| s.ty.is_place()) {
                let layout = op.layout.element.as_ref().unwrap();
                if matches!(&op.op, PlaceOp::Load { .. }) {
                    let dst = places.materialize(arena, value.index, layout.size, layout.align)?;
                    unsafe {
                        crate::runtime::place::copy(dst.as_ptr(), p, &layout.program);
                    }
                    slots[value.index] = Value::Place(dst.as_ptr());
                } else {
                    unsafe {
                        crate::runtime::place::copy(
                            p,
                            pointer(&slots[value.index]),
                            &layout.program,
                        );
                    }
                }
            } else if let Some(value) = value {
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
