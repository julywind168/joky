use std::collections::HashMap;

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;

use crate::codegen::abi::{abi_types, managed_pointer_offsets, value_arguments, value_from_params};
use crate::codegen::environment::CompiledValue;
use crate::diagnostic::CodegenError;
use crate::mir::{MirContinuation, MirFunction, MirValueId};
use crate::sema::TypeTable;

/// All entries of one function share a stable typed spill layout. Reusing a
/// handle at a later suspension cannot reinterpret an earlier cleanup slot.
pub(super) struct SpillLayout {
    pub(super) slots: Vec<(MirValueId, usize)>,
    pub(super) size: usize,
}

impl SpillLayout {
    pub(super) fn new(
        function: &MirFunction,
        pointer_type: cranelift_codegen::ir::Type,
        types: &TypeTable,
    ) -> Self {
        let mut values = function
            .continuations
            .iter()
            .flat_map(|continuation| continuation.spill_values.iter().copied())
            .collect::<Vec<_>>();
        values.sort_by_key(|value| value.0);
        values.dedup();
        let mut size = 0;
        let slots = values
            .into_iter()
            .map(|value| {
                let offset = size;
                size += abi_types(function.value_types[value.0], pointer_type, types).len() * 8;
                (value, offset)
            })
            .collect();
        Self { slots, size }
    }

    pub(super) fn save(
        &self,
        builder: &mut FunctionBuilder<'_>,
        continuation: &MirContinuation,
        pointer: Value,
        values: &HashMap<MirValueId, CompiledValue>,
    ) -> Result<(), CodegenError> {
        for (value, offset) in &self.slots {
            if !continuation.spill_values.contains(value)
                || continuation
                    .frame_slots
                    .iter()
                    .any(|slot| slot.value == Some(*value))
            {
                continue;
            }
            // Values also saved as locals have one owner in the frame. Their
            // spill slot stays zero so cancellation cannot release them twice.
            let compiled =
                values
                    .get(value)
                    .cloned()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: format!("continuation spill value %{} was not compiled", value.0),
                    })?;
            for (index, component) in value_arguments(compiled).into_iter().enumerate() {
                builder.ins().store(
                    MemFlagsData::new(),
                    component,
                    pointer,
                    (offset + index * 8) as i32,
                );
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Spill restoration combines the shared layout with per-continuation liveness, SSA values and target types."
    )]
    pub(super) fn restore(
        &self,
        builder: &mut FunctionBuilder<'_>,
        function: &MirFunction,
        continuation: &MirContinuation,
        pointer: Value,
        values: &mut HashMap<MirValueId, CompiledValue>,
        pointer_type: cranelift_codegen::ir::Type,
        types: &TypeTable,
    ) -> Result<(), CodegenError> {
        for (value, offset) in &self.slots {
            if !continuation.spill_values.contains(value) {
                continue;
            }
            let ty = function.value_types[value.0];
            if !values.contains_key(value) {
                let components = abi_types(ty, pointer_type, types)
                    .into_iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        builder.ins().load(
                            ty,
                            MemFlagsData::new(),
                            pointer,
                            (offset + index * 8) as i32,
                        )
                    })
                    .collect::<Vec<_>>();
                let compiled = value_from_params(&components, ty, types).map_err(|error| {
                    CodegenError::RuntimeError {
                        message: format!("continuation spill restore failed: {error}"),
                    }
                })?;
                values.insert(*value, compiled);
            }
            for offset in managed_pointer_offsets(ty, *offset, types) {
                let zero = builder.ins().iconst(pointer_type, 0);
                builder
                    .ins()
                    .store(MemFlagsData::new(), zero, pointer, offset as i32);
            }
        }
        Ok(())
    }
}
