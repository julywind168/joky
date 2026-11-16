//! Symbolic storage for constant arrays and aggregates containing arrays.
//! Offsets describe compiler layouts; cells contain values, never host pointers.
use super::*;
use std::{cell::RefCell, collections::BTreeMap};

#[derive(Clone)]
pub(super) struct Cell {
    pub(super) ty: Type,
    pub(super) value: Lane,
}

#[derive(Default)]
pub(super) struct Storage {
    pub(super) cells: BTreeMap<usize, Cell>,
    next: usize,
}

pub(super) enum Data {
    // Includes enum tags and inactive lanes; inactive String/Array lanes are NullPtr.
    Scalars(Vec<Lane>),
    Storage(Storage),
}

#[derive(Clone)]
pub(super) enum Lane {
    Scalar(Constant),
    Array(Rc<RefCell<super::arrays::Array>>),
}

impl From<Constant> for Lane {
    fn from(value: Constant) -> Self {
        Self::Scalar(value)
    }
}

impl Lane {
    pub(super) fn scalar(&self) -> &Constant {
        match self {
            Self::Scalar(value) => value,
            Self::Array(_) => unreachable!("checked scalar constant"),
        }
    }
    fn string_bytes(&self) -> usize {
        match self {
            Self::Scalar(Constant::String(text)) => text.len(),
            Self::Array(array) => array.borrow().bytes,
            _ => 0,
        }
    }
    fn dynamic_nodes(&self) -> usize {
        match self {
            Self::Array(array) => array.borrow().nodes,
            _ => 0,
        }
    }
    fn materialize(&self, plan: &mut FunctionPlan, dst: &Slot) -> Result<(), Error> {
        match self {
            Self::Scalar(Constant::String(text)) => {
                plan.emit(Op::Text {
                    dst: dst.clone(),
                    source: TextSource::Literal(text.clone()),
                });
                plan.register_owner(dst.clone(), None);
            }
            Self::Scalar(value) => plan.emit(Op::Literal(dst.clone(), value.clone())),
            Self::Array(array) => array.borrow().materialize(plan, dst)?,
        }
        Ok(())
    }
}

impl Data {
    pub(super) fn string_bytes(&self) -> usize {
        match self {
            Self::Scalars(values) => values.iter().map(Lane::string_bytes).sum(),
            Self::Storage(storage) => storage.string_bytes(),
        }
    }
    pub(super) fn dynamic_nodes(&self) -> usize {
        match self {
            Self::Scalars(values) => values.iter().map(Lane::dynamic_nodes).sum(),
            Self::Storage(storage) => storage.dynamic_nodes(),
        }
    }
    pub(super) fn materialize(&self, plan: &mut FunctionPlan, dst: &Slot) -> Result<(), Error> {
        match self {
            Self::Storage(storage) => storage.materialize(plan, dst),
            Self::Scalars(values) => materialize_lanes(plan, dst, values.iter()),
        }
    }
}

fn materialize_lanes<'a>(
    plan: &mut FunctionPlan,
    dst: &Slot,
    values: impl Iterator<Item = &'a Lane>,
) -> Result<(), Error> {
    for (field, value) in dst.scalars().zip(values) {
        if field.ty.is_owner_lane() && dst.ty != field.ty {
            // Register each owner before constructing the next fallible field.
            let temporary = plan.slot(field.ty.clone())?;
            value.materialize(plan, &temporary)?;
            plan.emit(Op::Copy(field, temporary));
        } else {
            value.materialize(plan, &field)?;
        }
    }
    if dst.ty.is_owner() && !dst.ty.is_owner_lane() {
        plan.register_owner(dst.clone(), None);
    }
    Ok(())
}

impl Storage {
    pub(super) fn copy_cells(&self) -> Self {
        Self {
            cells: self.cells.clone(),
            next: self.next,
        }
    }
    pub(super) fn string_bytes(&self) -> usize {
        self.cells.values().map(|c| c.value.string_bytes()).sum()
    }
    pub(super) fn dynamic_nodes(&self) -> usize {
        self.cells
            .values()
            .map(|cell| cell.value.dynamic_nodes())
            .sum()
    }
    pub(super) fn materialize(&self, plan: &mut FunctionPlan, dst: &Slot) -> Result<(), Error> {
        if !dst.ty.is_place() {
            return materialize_lanes(plan, dst, self.cells.values().map(|cell| &cell.value));
        }
        // Typed zeroed storage is registered before any fallible owner allocation.
        plan.emit(Op::Place(PlaceOp::Allocate(dst.clone())));
        for (&offset, cell) in &self.cells {
            if matches!(cell.value, Lane::Scalar(Constant::NullPtr)) {
                continue;
            }
            let value = plan.slot(cell.ty.clone())?;
            cell.value.materialize(plan, &value)?;
            plan.place_write(dst, offset, Some(value));
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct StorageEvaluator {
    pub(super) places: HashMap<usize, Place>,
    pub(super) arrays: HashMap<usize, Rc<RefCell<super::arrays::Array>>>,
}

#[derive(Clone)]
pub(super) struct Place {
    pub(super) storage: Rc<RefCell<Storage>>,
    pub(super) offset: usize,
}

impl From<Storage> for Place {
    fn from(storage: Storage) -> Self {
        Self {
            storage: Rc::new(RefCell::new(storage)),
            offset: 0,
        }
    }
}

impl StorageEvaluator {
    fn alias(&mut self, dst: &Slot, src: &Slot) {
        self.places
            .insert(dst.index, self.places[&src.index].clone());
    }

    // Reads preserve Copy fields and transfer String/Array owners, matching place
    // reads/writes. Zeroed inactive enum fields have their typed zero value.
    pub(super) fn read(&self, base: &Slot, offset: usize, ty: &Type) -> Storage {
        let size = ty.storage_layout().unwrap().size;
        let place = &self.places[&base.index];
        let offset = place.offset + offset;
        let mut source = place.storage.borrow_mut();
        let mut result = Storage::default();
        for (&position, cell) in source.cells.range_mut(offset..offset + size) {
            let value = if matches!(
                cell.value,
                Lane::Scalar(Constant::String(_)) | Lane::Array(_)
            ) {
                std::mem::replace(&mut cell.value, Constant::NullPtr.into())
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

    fn project(&mut self, dst: &Slot, src: &Slot, offset: usize) {
        let mut place = self.places[&src.index].clone();
        place.offset += offset;
        self.places.insert(dst.index, place);
    }

    pub(super) fn load(
        &mut self,
        dst: &Slot,
        reference: &Slot,
        offset: usize,
        slots: &mut [Option<Lane>],
    ) {
        self.assign(dst, self.read(reference, offset, &dst.ty), slots);
    }

    pub(super) fn assign(&mut self, dst: &Slot, mut data: Storage, slots: &mut [Option<Lane>]) {
        if dst.ty.is_place() {
            self.places.insert(dst.index, data.into());
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
                        .remove(&offset)
                        .map_or_else(|| zero(&ty).into(), |cell| cell.value),
                );
            }
        }
    }

    pub(super) fn value(&self, value: &Slot, slots: &mut [Option<Lane>]) -> Storage {
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

    fn write(&self, base: &Slot, offset: usize, value: &Slot, slots: &mut [Option<Lane>]) {
        let data = self.value(value, slots);
        let size = value.ty.storage_layout().unwrap().size;
        let place = &self.places[&base.index];
        let offset = place.offset + offset;
        let mut destination = place.storage.borrow_mut();
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
        slots: &mut [Option<Lane>],
        span: Span,
    ) -> Result<bool, Error> {
        if self.execute_array(op, slots, span)? {
            return Ok(true);
        }
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
                self.places.insert(dst.index, Storage::default().into());
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
            Op::PatternProject {
                dst, src, offset, ..
            } if self.places.contains_key(&src.index) => self.project(dst, src, *offset),
            Op::Project { dst, src, path, .. } if self.places.contains_key(&src.index) => {
                let offset = src
                    .ty
                    .reference_type()
                    .unwrap()
                    .target
                    .storage_field_path(path)
                    .unwrap()
                    .0;
                self.project(dst, src, offset)
            }
            Op::Place(PlaceOp::View {
                reference,
                start,
                end: None,
                dst,
                ..
            }) => {
                let Some(Lane::Scalar(Constant::UInt64(index))) = slots[start.index] else {
                    unreachable!("checked constant index")
                };
                let ty = &reference.ty.reference_type().unwrap().target;
                let length = ty.fixed_length();
                if index >= length {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidState,
                        format!("FixedArray constant index {index} is out of bounds for length {length}"),
                    ));
                }
                let stride = ty.fixed_shape().element.storage_layout().unwrap().size;
                self.project(dst, reference, index as usize * stride);
            }
            Op::ReadMemory {
                dst,
                reference,
                path,
            } if self.places.contains_key(&reference.index) => {
                if let Some(dst) = dst {
                    let offset = reference
                        .ty
                        .reference_type()
                        .unwrap()
                        .target
                        .storage_field_path(path)
                        .unwrap()
                        .0;
                    self.load(dst, reference, offset, slots);
                }
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
                    self.places.insert(dst.index, data.into());
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
                let next = self.places[&reference.index].storage.borrow().next;
                if let Some(value) = value {
                    self.write(reference, next * stride, value, slots);
                }
                self.places[&reference.index].storage.borrow_mut().next += 1;
            }
            Op::Place(PlaceOp::Read {
                dst,
                reference,
                offset,
            }) => {
                self.load(dst, reference, *offset, slots);
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
        Type::String | Type::Buffer(_) => Constant::NullPtr,
        _ => unreachable!("constant scalar storage"),
    }
}
