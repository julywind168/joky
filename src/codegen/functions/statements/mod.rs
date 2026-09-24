//! MIR statement compilation for the Cranelift backend.

mod compiler;
mod constants;
mod context;
mod continuations;
mod handlers;
mod ownership;
mod pending_calls;
pub(super) use pending_calls::free_native_reservations;
mod tasks;

pub(super) use compiler::compile_mir_statement;
use constants::compile_payload_constant;
pub(super) use context::{
    declare_task_runtime_refs, CompiledTask, RuntimeCallRefs, RuntimeRefs, StatementContext,
    TaskCodegenContext,
};
pub(super) use continuations::{compile_resumable_request, compile_resume, compile_suspend};
pub(super) use continuations::{emit_handler_request, HandlerRequestRefs};
pub(super) use handlers::{compile_handler_enter, emit_handler_enter};
pub(in crate::codegen) use ownership::duplicate_shared_value;
pub(super) use tasks::{compile_task_failure, compile_task_lifecycle};

use std::collections::HashMap;

use cranelift_codegen::ir::{types, InstBuilder, StackSlot, StackSlotData, StackSlotKind, Value};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirConstant, MirScopeId, MirStatement, MirValueId};
use crate::sema::{Type, TypeTable};
use joky_runtime_abi::{
    CONTINUATION_SUSPEND_ARGUMENT_STORAGE, CONTINUATION_SUSPEND_RESULT_STORAGE,
    TASK_CONTEXT_CANCELLATION_OFFSET, TASK_CONTEXT_CANCELLED_RESULT_OFFSET,
    TASK_CONTEXT_CAPTURES_OFFSET, TASK_CONTEXT_CONTINUATION_OFFSET,
    TASK_CONTEXT_DROP_RESULT_OFFSET, TASK_CONTEXT_RESULT_INITIALIZED_OFFSET,
    TASK_CONTEXT_RESULT_OFFSET, TASK_CONTEXT_RESULT_SIZE_OFFSET, TASK_CONTEXT_SIZE,
};

use super::super::abi::{
    abi_types, managed_pointer_offsets, managed_pointer_types, value_arguments, value_from_params,
    value_type,
};
use super::super::environment::{CompiledValue, Environment};
use super::super::operators::{compile_binary_value, compile_numeric_method, compile_unary_value};
use super::continuation::register_continuation_cleanups;
use super::runtime::{compile_runtime_call, RuntimeCallContext};
use super::values::*;

/// Return whether an effect operation is the canonical standard-library
/// `time.sleep` operation. Timer lowering is intentionally tied to this
/// operation identity instead of accepting every `@suspends(Duration)` shape.
pub(super) fn is_time_sleep_operation(
    types: &TypeTable,
    operation: crate::sema::EffectOperationId,
) -> bool {
    let Some(effect) = types.effects().by_name("time") else {
        return false;
    };
    let Some(canonical) = types.effects().operation_by_name(effect, "sleep") else {
        return false;
    };
    if canonical != operation {
        return false;
    }
    let Some(info) = types.effects().operation_info(operation) else {
        return false;
    };
    info.suspends && info.parameters == [Type::Duration] && info.return_type == Type::Unit
}

pub(super) fn create_task_slot(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: cranelift_codegen::ir::Type,
    components: usize,
) -> Result<(StackSlot, Value), CodegenError> {
    let size = components
        .max(1)
        .checked_mul(8)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "task ABI representation is too large".to_owned(),
        })?;
    let slot =
        builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 3));
    let pointer = builder.ins().stack_addr(pointer_type, slot, 0);
    Ok((slot, pointer))
}

pub(super) fn payload_word(
    builder: &mut FunctionBuilder<'_>,
    value: Value,
    value_type: cranelift_codegen::ir::Type,
) -> Value {
    match value_type {
        types::I64 => value,
        types::I8 | types::I16 | types::I32 => builder.ins().uextend(types::I64, value),
        types::F32 => {
            let bits = builder.ins().bitcast(
                types::I32,
                cranelift_codegen::ir::MemFlagsData::new(),
                value,
            );
            builder.ins().uextend(types::I64, bits)
        }
        types::F64 => builder.ins().bitcast(
            types::I64,
            cranelift_codegen::ir::MemFlagsData::new(),
            value,
        ),
        _ => value,
    }
}

fn task_group(context: &TaskCodegenContext<'_>, scope: MirScopeId) -> Result<Value, CodegenError> {
    context
        .groups
        .get(&scope)
        .copied()
        .ok_or_else(|| CodegenError::RuntimeError {
            message: format!(
                "task scope s{} is not active during code generation",
                scope.0
            ),
        })
}

fn task_result(
    builder: &mut FunctionBuilder<'_>,
    task: &CompiledTask,
    pointer_type: cranelift_codegen::ir::Type,
    type_table: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    let values = abi_types(task.result_type, pointer_type, type_table)
        .into_iter()
        .enumerate()
        .map(|(index, ty)| {
            builder.ins().load(
                ty,
                cranelift_codegen::ir::MemFlagsData::new(),
                task.result_pointer,
                (index * 8) as i32,
            )
        })
        .collect::<Vec<_>>();
    value_from_params(&values, task.result_type, type_table).map_err(|error| {
        CodegenError::RuntimeError {
            message: error.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::is_time_sleep_operation;
    use crate::{sema, syntax};

    #[test]
    fn timer_dispatch_only_matches_canonical_time_sleep() {
        let program = syntax::parse_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Timer { @suspends fn wait(duration: Duration) -> Unit }\n\
             fn main() effects { time, Timer } { time.sleep(1ms); Timer.wait(1ms) }",
        )
        .expect("timer predicate source should parse");
        let types =
            sema::check_program(&program).expect("timer predicate source should type check");
        let time = types.effects().by_name("time").expect("time effect");
        let timer = types.effects().by_name("Timer").expect("custom effect");
        let time_sleep = types
            .effects()
            .operation_by_name(time, "sleep")
            .expect("time.sleep operation");
        let timer_wait = types
            .effects()
            .operation_by_name(timer, "wait")
            .expect("Timer.wait operation");

        assert!(is_time_sleep_operation(&types, time_sleep));
        assert!(!is_time_sleep_operation(&types, timer_wait));
    }
}
