//! Explicit interpreter place operations. Cloning a VM register never copies a
//! language array: language copies always execute this typed copy program.
use super::*;
use crate::core::ir::PlaceOp;
use crate::runtime::{arena::Arena, place::CopyNode};

pub(super) struct Instruction {
    op: PlaceOp,
    layout: Arc<Layout>,
}

pub(super) struct Layout {
    size: usize,
    align: usize,
    length: u64,
    stride: usize,
    pub(super) program: Arc<[CopyNode]>,
    fields: Vec<(usize, Type)>,
    element: Option<Arc<Layout>>,
}

#[derive(Default)]
pub(super) struct Layouts(Vec<(Type, Arc<Layout>)>);
impl Layouts {
    pub(super) fn get(&mut self, ty: &Type) -> Arc<Layout> {
        if let Some((_, layout)) = self.0.iter().find(|(known, _)| known == ty) {
            return layout.clone();
        }
        let shape = ty.storage_layout().unwrap();
        let array = match ty {
            Type::FixedArray(a) | Type::FixedArrayInit(a) => Some(a),
            _ => None,
        };
        let element = array.map_or(ty, |a| &a.element);
        let child = if array.is_some() {
            Some(self.get(element))
        } else {
            None
        };
        let layout = Arc::new(Layout {
            element: child,
            size: shape.size,
            align: shape.align,
            length: array.map_or(0, |_| ty.fixed_length()),
            stride: element.storage_layout().unwrap().size,
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
    cleanups: &mut crate::runtime::cleanup::Cleanups,
) -> Result<(), Error> {
    let places = &mut cleanups.places;
    match &op.op {
        PlaceOp::View {
            reference,
            start,
            end,
            dst,
            scratch,
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
                let base = pointer(&slots[reference.index]);
                if let Some(scratch) = scratch {
                    crate::runtime::buffer::fixed_view_local(
                        std::ptr::NonNull::new(pointer(&slots[scratch.index]).cast()).unwrap(),
                        base,
                        op.layout.length,
                        op.layout.stride,
                        op.layout.align,
                        from,
                        to.expect("local descriptor only for slices"),
                    )?
                } else {
                    crate::runtime::buffer::fixed_view(
                        arena,
                        base,
                        op.layout.length,
                        op.layout.stride,
                        op.layout.align,
                        from,
                        to,
                    )?
                }
            };
            let element = &reference.ty.fixed_shape().element;
            let pointer = if end.is_none() && element.is_owner() && !element.is_inline_owner() {
                unsafe { pointer.cast::<*mut u8>().read_unaligned() }
            } else {
                pointer
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
                places.end(arena, *temporary)?;
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
            unsafe {
                crate::runtime::place::dispose(arena, p.as_ptr(), &op.layout.program)?;
            }
            places.describe(arena, dst.index, &op.layout.program)?;
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
                places.describe(arena, dst.index, &op.layout.program)?;
                unsafe {
                    crate::runtime::place::replace(arena, p.as_ptr(), source, &op.layout.program)?;
                }
                slots[dst.index] = Value::Place(p.as_ptr());
            } else {
                for ((offset, ty), slot) in op.layout.fields.iter().zip(dst.scalars()) {
                    slots[slot.index] = unsafe { memory::read_scalar(source.add(*offset), ty) };
                    if ty.is_owner_lane() {
                        unsafe {
                            source.add(*offset).cast::<usize>().write_unaligned(0);
                        }
                    }
                }
            }
        }
        PlaceOp::Copy { dst, src } => {
            let source = pointer(&slots[src.index]);
            let p = places.materialize(arena, dst.index, op.layout.size, op.layout.align)?;
            places.describe(arena, dst.index, &op.layout.program)?;
            unsafe {
                crate::runtime::place::replace(arena, p.as_ptr(), source, &op.layout.program)?;
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
                crate::runtime::place::replace(
                    arena,
                    destination,
                    pointer(&slots[value.index]),
                    &op.layout.program,
                )?;
            } else {
                crate::runtime::place::dispose(arena, destination, &op.layout.program)?;
                for ((offset, _), slot) in op.layout.fields.iter().zip(value.scalars()) {
                    memory::write_scalar(destination.add(*offset), &slots[slot.index]);
                }
            }
            if value.ty.is_owner() && !value.ty.is_place() {
                cleanups.take(value.owner_key());
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
            places.describe(arena, dst.index, &op.layout.program)?;
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::MoveInit { dst, src, publish } => {
            let p = places.move_value(arena, dst.index, pointer(&slots[src.index]), *publish)?;
            places.describe(arena, dst.index, &op.layout.program)?;
            slots[dst.index] = Value::Place(p.as_ptr());
        }
        PlaceOp::End(slot) => unsafe {
            places.end(arena, slot.index)?;
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
            if matches!(&op.op, PlaceOp::Store { .. }) {
                unsafe {
                    crate::runtime::place::dispose(
                        arena,
                        p,
                        &op.layout.element.as_ref().unwrap().program,
                    )?;
                }
            }
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
            if !matches!(&op.op, PlaceOp::Load { .. }) {
                if let Some(value) = value
                    .as_ref()
                    .filter(|v| v.ty.is_owner() && !v.ty.is_place())
                {
                    cleanups.take(value.owner_key());
                }
            }
            if matches!(&op.op, PlaceOp::WriteNext { .. }) {
                cleanups.places.commit_pointer(base)?;
            }
        }
    }
    Ok(())
}
