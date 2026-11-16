//! Symbolic storage for constant arrays and aggregates containing arrays.
//! Offsets describe compiler layouts; cells contain values, never host pointers.
use super::*;
use std::{cell::RefCell, collections::BTreeMap};

struct Cell {
    ty: Type,
    value: Constant,
}

#[derive(Default)]
pub(super) struct Storage {
    cells: BTreeMap<usize, Cell>,
    next: usize,
}

pub(super) enum Data {
    // Includes enum tags and inactive lanes; inactive String lanes are NullPtr.
    Scalars(Vec<Constant>),
    Storage(Storage),
}

impl Data {
    pub(super) fn string_bytes(&self) -> usize {
        fn bytes(value: &Constant) -> usize {
            match value {
                Constant::String(text) => text.len(),
                _ => 0,
            }
        }
        match self {
            Self::Scalars(values) => values.iter().map(bytes).sum(),
            Self::Storage(storage) => storage.cells.values().map(|cell| bytes(&cell.value)).sum(),
        }
    }

    pub(super) fn materialize_storage(
        &self,
        plan: &mut FunctionPlan,
        dst: &Slot,
    ) -> Result<(), Error> {
        let Self::Storage(storage) = self else {
            unreachable!()
        };
        // Allocation registers a zeroed, typed place before any fallible text
        // construction. Tags precede their payloads in byte-offset order.
        plan.emit(Op::Place(PlaceOp::Allocate(dst.clone())));
        for (&offset, cell) in &storage.cells {
            if matches!(cell.value, Constant::NullPtr) {
                continue;
            }
            let value = plan.slot(cell.ty.clone())?;
            if let Constant::String(text) = &cell.value {
                plan.emit(Op::Text {
                    dst: value.clone(),
                    source: TextSource::Literal(text.clone()),
                });
                plan.register_owner(value.clone(), None);
            } else {
                plan.emit(Op::Literal(value.clone(), cell.value.clone()));
            }
            plan.place_write(dst, offset, Some(value));
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct StorageEvaluator {
    places: HashMap<usize, Rc<RefCell<Storage>>>,
}

impl StorageEvaluator {
    fn alias(&mut self, dst: &Slot, src: &Slot) {
        self.places
            .insert(dst.index, self.places[&src.index].clone());
    }

    // Reads preserve Copy fields and transfer string owners, matching place
    // reads/writes. Zeroed inactive enum fields have their typed zero value.
    fn read(&self, base: &Slot, offset: usize, ty: &Type) -> Storage {
        let size = ty.storage_layout().unwrap().size;
        let mut source = self.places[&base.index].borrow_mut();
        let mut result = Storage::default();
        for (&position, cell) in source.cells.range_mut(offset..offset + size) {
            let value = if matches!(cell.value, Constant::String(_)) {
                std::mem::replace(&mut cell.value, Constant::NullPtr)
            } else {
                cell.value.clone()
            };
            result.cells.insert(
                position - offset,
                Cell {
                    ty: cell.ty.clone(),
                    value,
                },
            );
        }
        result
    }

    fn value(&self, value: &Slot, slots: &mut [Option<Constant>]) -> Storage {
        if value.ty.is_place() {
            return self.read(value, 0, &value.ty);
        }
        let cells = value
            .ty
            .storage_scalars()
            .unwrap()
            .into_iter()
            .zip(value.scalars())
            .map(|((offset, ty), slot)| {
                let value = if ty.is_owner_lane() {
                    slots[slot.index].take()
                } else {
                    slots[slot.index].clone()
                }
                .expect("evaluated storage input");
                (offset, Cell { ty, value })
            })
            .collect();
        Storage { cells, next: 0 }
    }

    fn write(&self, base: &Slot, offset: usize, value: &Slot, slots: &mut [Option<Constant>]) {
        let data = self.value(value, slots);
        let size = value.ty.storage_layout().unwrap().size;
        let mut destination = self.places[&base.index].borrow_mut();
        destination
            .cells
            .retain(|&position, _| position < offset || position >= offset + size);
        destination.cells.extend(
            data.cells
                .into_iter()
                .map(|(position, cell)| (offset + position, cell)),
        );
    }

    pub(super) fn result(&self, value: &Slot) -> Data {
        Data::Storage(self.read(value, 0, &value.ty))
    }

    pub(super) fn execute(
        &mut self,
        op: &Op,
        slots: &mut [Option<Constant>],
        span: Span,
    ) -> Result<bool, Error> {
        match op {
            Op::Place(PlaceOp::Allocate(dst) | PlaceOp::Begin(dst)) => {
                // Also validate and bound intermediate storage discarded by a
                // projection. Unsupported inactive payloads must not reach the
                // scalar storage evaluator merely because the result is small.
                let ty = match &dst.ty {
                    Type::FixedArrayInit(shape) => Type::FixedArray(shape.clone()),
                    ty => ty.clone(),
                };
                if !members::constant_type_supported(&ty) {
                    return Err(type_error(
                        &span,
                        ErrorCode::TypeMismatch,
                        "Constant storage requires supported constant element types",
                    ));
                }
                value_nodes(&ty, span)?;
                self.places
                    .insert(dst.index, Rc::new(RefCell::new(Storage::default())));
            }
            Op::Place(PlaceOp::Alias { dst, src })
            | Op::Buffer(BufferOp::Borrow { dst, src, .. })
                if src.ty.is_place() =>
            {
                self.alias(dst, src)
            }
            Op::Local {
                dst,
                value: Some(value),
                target,
            } if target.is_place() => self.alias(dst, value),
            Op::Reference { dst, src, .. } if self.places.contains_key(&src.index) => {
                self.alias(dst, src)
            }
            Op::Place(PlaceOp::MoveInit { dst, src, .. }) => {
                let data = self
                    .places
                    .remove(&src.index)
                    .expect("constant initializer storage");
                self.places.insert(dst.index, data);
            }
            Op::Place(PlaceOp::Copy { dst, src }) => {
                if dst.index != src.index {
                    let data = self.read(src, 0, &src.ty);
                    self.places.insert(dst.index, Rc::new(RefCell::new(data)));
                }
            }
            Op::Place(PlaceOp::Write {
                reference,
                value,
                offset,
            }) => self.write(reference, *offset, value, slots),
            Op::Place(PlaceOp::WriteNext { reference, value }) => {
                let shape = reference.ty.fixed_shape();
                let stride = shape.element.storage_layout().unwrap().size;
                let next = self.places[&reference.index].borrow().next;
                if let Some(value) = value {
                    self.write(reference, next * stride, value, slots);
                }
                self.places[&reference.index].borrow_mut().next += 1;
            }
            Op::Place(PlaceOp::Read {
                dst,
                reference,
                offset,
            }) => {
                let data = self.read(reference, *offset, &dst.ty);
                if dst.ty.is_place() {
                    self.places.insert(dst.index, Rc::new(RefCell::new(data)));
                } else {
                    for ((offset, ty), slot) in dst
                        .ty
                        .storage_scalars()
                        .unwrap()
                        .into_iter()
                        .zip(dst.scalars())
                    {
                        slots[slot.index] = Some(
                            data.cells
                                .get(&offset)
                                .map_or_else(|| zero(&ty), |cell| cell.value.clone()),
                        );
                    }
                }
            }
            Op::Place(PlaceOp::End(value)) | Op::Buffer(BufferOp::End(value))
                if value.ty.is_place() =>
            {
                self.places.remove(&value.index);
            }
            Op::EndLocal(value) if self.places.contains_key(&value.index) => {
                self.places.remove(&value.index);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

fn zero(ty: &Type) -> Constant {
    match ty {
        Type::Int8 => Constant::Int8(0),
        Type::Int16 => Constant::Int16(0),
        Type::Int32 => Constant::Int32(0),
        Type::Int64 => Constant::Int64(0),
        Type::UInt8 => Constant::UInt8(0),
        Type::UInt16 => Constant::UInt16(0),
        Type::UInt32 => Constant::UInt32(0),
        Type::UInt64 => Constant::UInt64(0),
        Type::Rune => Constant::Rune(0),
        Type::Bool => Constant::Bool(false),
        Type::Float32 => Constant::Float32(0.0),
        Type::Float64 => Constant::Float64(0.0),
        Type::String => Constant::NullPtr,
        _ => unreachable!("constant scalar storage"),
    }
}
