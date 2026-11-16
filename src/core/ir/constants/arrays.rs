//! Array templates use compiler storage operations, never execute library functions.
use super::storage::{Lane, Storage, StorageEvaluator};
use super::*;
use crate::parser::ast::BufferIntrinsic as BI;
use std::{cell::RefCell, rc::Rc};

pub(super) struct Array {
    pub(super) element: Type,
    pub(super) values: Vec<Storage>,
    pub(super) nodes: usize,
    pub(super) bytes: usize,
}

impl Array {
    pub(super) fn materialize(&self, plan: &mut FunctionPlan, dst: &Slot) -> Result<(), Error> {
        let length = plan.pattern_u64(self.values.len() as u64)?;
        plan.emit(Op::Buffer(BufferOp::Storage {
            intrinsic: BI::Allocate,
            element: self.element.clone(),
            args: vec![length],
            dst: Some(dst.clone()),
            scratch: None,
        }));
        plan.register_owner(dst.clone(), None);
        let root = plan.sequence_value_reference(dst, true)?;
        for data in &self.values {
            let value = plan.slot(self.element.clone())?;
            data.materialize(plan, &value)?;
            plan.emit(Op::Buffer(BufferOp::Storage {
                intrinsic: BI::InitializeNext,
                element: self.element.clone(),
                args: vec![root.clone(), value],
                dst: None,
                scratch: None,
            }));
        }
        Ok(())
    }

    fn push(&mut self, value: Storage, span: Span) -> Result<(), Error> {
        let nodes = value_nodes(&self.element, span)? + value.dynamic_nodes();
        if self.nodes + nodes + 1 > MAX_VALUE_NODES {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                "Constant aggregate values are limited to 4096 components",
            ));
        }
        let bytes = value.string_bytes();
        if self.bytes + bytes > MAX_EXPRESSION_STRING_BYTES {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                "Constant expressions are limited to 16 MiB of expanded string data",
            ));
        }
        self.nodes += nodes;
        self.bytes += bytes;
        self.values.push(value);
        Ok(())
    }
}

impl FunctionPlan {
    /// Only array literal lowering calls this path in a constant plan. Source
    /// calls are rejected by the expression whitelist before planning begins.
    pub(in crate::core::ir) fn constant_array_operation(
        &mut self,
        name: &str,
        element: &Type,
        mut args: Vec<Option<Slot>>,
        types: &TypeRegistry,
        span: Span,
    ) -> Result<Option<Slot>, Error> {
        match name {
            "with_capacity" => {
                let ty = self.dynamic_array_type(element.clone(), types, span)?;
                self.sequence_storage(
                    BI::Allocate,
                    element,
                    args.into_iter().flatten().collect(),
                    ty,
                )
            }
            "reserve" => Ok(None), // Symbolic storage grows only after bounded element writes.
            "push" => {
                let value = args
                    .pop()
                    .unwrap()
                    .map_or_else(|| self.slot(Type::Unit), Ok)?;
                let reference = args.pop().unwrap().unwrap();
                self.sequence_storage(
                    BI::InitializeNext,
                    element,
                    vec![reference, value],
                    Type::Unit,
                )
            }
            _ => unreachable!("compiler array literal helper"),
        }
    }
}

impl StorageEvaluator {
    fn array(&self, slot: &Slot, slots: &[Option<Lane>]) -> Option<Rc<RefCell<Array>>> {
        if let Some(array) = self.arrays.get(&slot.index) {
            return Some(array.clone());
        }
        if let Some(Lane::Array(array)) = &slots[slot.index] {
            return Some(array.clone());
        }
        let place = self.places.get(&slot.index)?;
        let storage = place.storage.borrow();
        match &storage.cells.get(&place.offset)?.value {
            Lane::Array(array) => Some(array.clone()),
            _ => None,
        }
    }

    pub(super) fn execute_array(
        &mut self,
        op: &Op,
        slots: &mut [Option<Lane>],
        span: Span,
    ) -> Result<bool, Error> {
        match op {
            Op::Buffer(BufferOp::Borrow { dst, src, .. }) if !src.ty.is_place() => {
                let Some(array) = self.array(src, slots) else {
                    return Ok(false);
                };
                self.arrays.insert(dst.index, array);
                return Ok(true);
            }
            Op::Reference { dst, src, .. } if self.arrays.contains_key(&src.index) => {
                self.arrays
                    .insert(dst.index, self.arrays[&src.index].clone());
                return Ok(true);
            }
            _ => {}
        }
        let Op::Buffer(BufferOp::Storage {
            intrinsic,
            element,
            args,
            dst,
            ..
        }) = op
        else {
            return Ok(false);
        };
        if *intrinsic == BI::Allocate {
            if !members::constant_type_supported(element) {
                return Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Constant arrays require supported constant element types",
                ));
            }
            value_nodes(element, span)?;
            slots[dst.as_ref().unwrap().index] = Some(Lane::Array(Rc::new(RefCell::new(Array {
                element: element.clone(),
                values: Vec::new(),
                nodes: 0,
                bytes: 0,
            }))));
            return Ok(true);
        }
        let Some(array) = self.array(&args[0], slots) else {
            return Ok(false);
        };
        let index = |slot: &Slot| match slots[slot.index].as_ref().unwrap().scalar() {
            Constant::UInt64(index) => *index,
            _ => unreachable!("checked constant array index"),
        };
        let checked = |index: u64| {
            let length = array.borrow().values.len();
            if index >= length as u64 {
                Err(type_error(
                    &span,
                    ErrorCode::InvalidState,
                    format!("Array constant index {index} is out of bounds for length {length}"),
                ))
            } else {
                Ok(index as usize)
            }
        };
        match intrinsic {
            BI::Len => {
                slots[dst.as_ref().unwrap().index] =
                    Some(Constant::UInt64(array.borrow().values.len() as u64).into());
            }
            BI::InitializeNext => {
                let data = self.value(&args[1], slots);
                array.borrow_mut().push(data, span)?;
            }
            BI::SwapElements => {
                let left = checked(index(&args[1]))?;
                let right = checked(index(&args[2]))?;
                array.borrow_mut().values.swap(left, right);
            }
            BI::TakeLast => {
                let mut array = array.borrow_mut();
                let value = array.values.pop().expect("checked constant spread length");
                array.nodes -= value_nodes(element, span)? + value.dynamic_nodes();
                array.bytes -= value.string_bytes();
                if let Some(dst) = dst {
                    self.assign(dst, value, slots);
                }
            }
            BI::Borrow | BI::BorrowMut | BI::Load => {
                let index = checked(index(&args[1]))?;
                // Snapshot typed cells for borrowed reads. Ordinary checking
                // only permits Copy values to leave a source-level borrow.
                let place = array.borrow().values[index].copy_cells();
                let dst = dst.as_ref().expect("array element reference or value");
                if *intrinsic == BI::Load {
                    self.assign(dst, place, slots);
                } else {
                    self.places.insert(dst.index, place.into());
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
