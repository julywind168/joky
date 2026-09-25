//! Continuation machine-entry compilation for the Cranelift backend.

mod calls;
mod setup;
mod tasks;
mod terminators;

use std::collections::HashMap;

use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Signature};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Linkage;

use crate::diagnostic::CodegenError;
use crate::mir::{MirFunction, MirFunctionId, MirStatement};
use crate::sema::{Type, TypeTable};
use joky_runtime_abi::{
    CONTINUATION_FRAME_STORAGE, CONTINUATION_SUSPEND_ARGUMENT_STORAGE,
    CONTINUATION_SUSPEND_RESULT_STORAGE,
};

use self::setup::setup_continuation_entry;
use self::tasks::load_task_value;
use self::terminators::compile_continuation_terminator;
use super::super::super::abi::{abi_types, value_arguments};
use super::super::super::constructors::zero_compiled_value;
use super::super::super::cranelift::{CraneliftBackend, ModuleLifecycle};
use super::super::super::environment::{CompiledValue, FunctionType};
use super::super::super::helpers::codegen_error;
use super::super::super::operators::{
    compile_binary_value, compile_numeric_method, compile_unary_value,
};
use super::super::continuation::{
    continuation_machine_blocks, continuation_protocol, register_continuation_cleanups,
};
use super::super::runtime::{compile_runtime_call, RuntimeCallContext};
use super::super::runtime_ids::RuntimeCallIds;
use super::super::statements::{
    create_task_slot, duplicate_shared_value, emit_handler_request, is_time_sleep_operation,
    HandlerRequestRefs,
};
use super::super::values::*;
use super::refs::{
    declare_continuation_codegen_refs, declare_continuation_entry_refs, ContinuationEntryIds,
};

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    /// Define the ABI-shaped entry used by the scheduler for a continuation.
    /// This stackless slice compiles a side-effect-limited MIR CFG rooted at
    /// `Resume`. It accepts loops and direct calls that cannot suspend.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::codegen::functions) fn compile_continuation_machine_entry(
        &mut self,
        function: &MirFunction,
        continuation: &crate::mir::MirContinuation,
        entry_id: cranelift_module::FuncId,
        func_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
        function_types: &HashMap<MirFunctionId, FunctionType>,
        closure_call_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
        closure_drop_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
        callback_trampoline_ids: &HashMap<
            crate::codegen::environment::CallbackKey,
            cranelift_module::FuncId,
        >,
        continuation_complete_id: cranelift_module::FuncId,
        continuation_is_cancelled_id: cranelift_module::FuncId,
        continuation_frame_pointer_id: cranelift_module::FuncId,
        continuation_spill_pointer_id: cranelift_module::FuncId,
        continuation_result_pointer_id: cranelift_module::FuncId,
        continuation_suspend_result_pointer_id: cranelift_module::FuncId,
        continuation_alloc_suspend_result_id: cranelift_module::FuncId,
        continuation_release_suspend_result_id: cranelift_module::FuncId,
        continuation_clear_suspend_cleanups_id: cranelift_module::FuncId,
        continuation_register_cleanup_id: cranelift_module::FuncId,
        continuation_register_cleanup_region_id: cranelift_module::FuncId,
        continuation_resuspend_suspend_id: cranelift_module::FuncId,
        continuation_resuspend_suspend_payload_id: cranelift_module::FuncId,
        continuation_set_resume_entry_id: cranelift_module::FuncId,
        continuation_set_program_counter_id: cranelift_module::FuncId,
        continuation_root_group_id: cranelift_module::FuncId,
        task_group_new_id: cranelift_module::FuncId,
        task_spawn_heap_id: cranelift_module::FuncId,
        task_group_free_id: cranelift_module::FuncId,
        task_failure_operation_id: cranelift_module::FuncId,
        task_failure_claim_id: cranelift_module::FuncId,
        task_failure_rethrow_id: cranelift_module::FuncId,
        task_join_id: cranelift_module::FuncId,
        task_claim_result_id: cranelift_module::FuncId,
        task_cancel_id: cranelift_module::FuncId,
        task_result_pointer_id: cranelift_module::FuncId,
        task_race_id: cranelift_module::FuncId,
        task_failure_payload_id: cranelift_module::FuncId,
        task_abort_id: cranelift_module::FuncId,
        task_abort_payload_id: cranelift_module::FuncId,
        task_failure_drop_ids: &HashMap<crate::sema::EffectOperationId, cranelift_module::FuncId>,
        handler_begin_payload_id: cranelift_module::FuncId,
        handler_begin_payload_env_id: cranelift_module::FuncId,
        handler_dispatch_id: cranelift_module::FuncId,
        handler_payload_copy_id: cranelift_module::FuncId,
        handler_payload_consume_id: cranelift_module::FuncId,
        handler_free_resumption_id: cranelift_module::FuncId,
        handler_thunk_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
        task_runtime_ids: crate::codegen::cranelift::TaskRuntimeIds,
        task_thunk_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
        task_result_drop_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
        continuation_entry_keys: &HashMap<crate::mir::MirContinuationId, usize>,
        runtime_call_ids: RuntimeCallIds,
        class_drop_ids: &HashMap<usize, cranelift_module::FuncId>,
        continuation_cleanup_drop_ids: &HashMap<Type, cranelift_module::FuncId>,
        types: &TypeTable,
        pointer_type: cranelift_codegen::ir::Type,
        call_conv: cranelift_codegen::isa::CallConv,
        string_literals: &[Box<[u8]>],
    ) -> Result<(), CodegenError> {
        let mut context = self.module.make_context();
        context.func.signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        let entry_refs = declare_continuation_entry_refs(
            &mut self.module,
            &mut context.func,
            ContinuationEntryIds {
                complete: continuation_complete_id,
                is_cancelled: continuation_is_cancelled_id,
                frame_pointer: continuation_frame_pointer_id,
                spill_pointer: continuation_spill_pointer_id,
                result_pointer: continuation_result_pointer_id,
                suspend_result_pointer: continuation_suspend_result_pointer_id,
                alloc_suspend_result: continuation_alloc_suspend_result_id,
                release_suspend_result: continuation_release_suspend_result_id,
                clear_suspend_cleanups: continuation_clear_suspend_cleanups_id,
                register_cleanup: continuation_register_cleanup_id,
                register_cleanup_region: continuation_register_cleanup_region_id,
                resuspend_suspend: continuation_resuspend_suspend_id,
                resuspend_suspend_payload: continuation_resuspend_suspend_payload_id,
                set_resume_entry: continuation_set_resume_entry_id,
                set_program_counter: continuation_set_program_counter_id,
                root_group: continuation_root_group_id,
                group_new: task_group_new_id,
                spawn_heap: task_spawn_heap_id,
                group_free: task_group_free_id,
                failure_operation: task_failure_operation_id,
                failure_claim: task_failure_claim_id,
                failure_rethrow: task_failure_rethrow_id,
                task_join: task_join_id,
                task_claim_result: task_claim_result_id,
                task_cancel: task_cancel_id,
                task_result_pointer: task_result_pointer_id,
                race: task_race_id,
                failure_payload: task_failure_payload_id,
                abort: task_abort_id,
                abort_payload: task_abort_payload_id,
                handler_resumption: super::refs::HandlerResumptionIds {
                    begin_payload: handler_begin_payload_id,
                    begin_payload_env: handler_begin_payload_env_id,
                    dispatch: handler_dispatch_id,
                    payload_copy: handler_payload_copy_id,
                    payload_consume: handler_payload_consume_id,
                    free: handler_free_resumption_id,
                },
            },
            runtime_call_ids,
        );
        let complete_ref = entry_refs.complete;
        let is_cancelled_ref = entry_refs.is_cancelled;
        let result_pointer_ref = entry_refs.result_pointer;
        let alloc_suspend_result_ref = entry_refs.alloc_suspend_result;
        let clear_suspend_cleanups_ref = entry_refs.clear_suspend_cleanups;
        let register_cleanup_ref = entry_refs.register_cleanup;
        let register_cleanup_region_ref = entry_refs.register_cleanup_region;
        let resuspend_suspend_ref = entry_refs.resuspend_suspend;
        let resuspend_suspend_payload_ref = entry_refs.resuspend_suspend_payload;
        let set_resume_entry_ref = entry_refs.set_resume_entry;
        let set_program_counter_ref = entry_refs.set_program_counter;
        let group_new_ref = entry_refs.group_new;
        let spawn_heap_ref = entry_refs.spawn_heap;
        let group_free_ref = entry_refs.group_free;
        let failure_operation_ref = entry_refs.failure_operation;
        let failure_claim_ref = entry_refs.failure_claim;
        let failure_rethrow_ref = entry_refs.failure_rethrow;
        let task_join_ref = entry_refs.task_join;
        let task_claim_result_ref = entry_refs.task_claim_result;
        let task_cancel_ref = entry_refs.task_cancel;
        let task_result_pointer_ref = entry_refs.task_result_pointer;
        let race_ref = entry_refs.race;
        let failure_payload_ref = entry_refs.failure_payload;
        let runtime_call_refs = entry_refs.calls;
        let main_result_id = self
            .module
            .declare_function(
                joky_runtime_abi::symbols::MAIN_RESULT_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![
                        AbiParam::new(cranelift_codegen::ir::types::I32),
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                    ],
                    returns: Vec::new(),
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let main_result_ref = self
            .module
            .declare_func_in_func(main_result_id, &mut context.func);
        let string_from_ref = runtime_call_refs.string_from;
        let dup_ref = runtime_call_refs.dup;
        let allocate_ref = runtime_call_refs.allocate;
        let closure_allocate_ref = runtime_call_refs.closure_allocate;
        let support_refs = declare_continuation_codegen_refs(
            &mut self.module,
            &mut context.func,
            class_drop_ids,
            continuation_cleanup_drop_ids,
            func_ids,
            closure_call_ids,
            closure_drop_ids,
            callback_trampoline_ids,
            task_thunk_ids,
            task_result_drop_ids,
        );
        let failure_drop_refs = task_failure_drop_ids
            .iter()
            .map(|(operation, drop)| {
                (
                    *operation,
                    self.module.declare_func_in_func(*drop, &mut context.func),
                )
            })
            .collect::<HashMap<_, _>>();
        let pending_refs =
            super::super::pending::declare_pending_refs(&mut self.module, &mut context.func)?;
        let allocation_refs =
            crate::codegen::functions::ordinary_continuations::ContinuationAllocationRefs::declare(
                &mut self.module,
                &mut context.func,
                runtime_call_refs.managed_drop,
            )?;
        let handler_refs = super::super::statements::declare_task_runtime_refs(
            &mut self.module,
            &mut context.func,
            task_runtime_ids,
        );
        let handler_thunks = handler_thunk_ids
            .iter()
            .map(|(id, function)| {
                (
                    *id,
                    self.module
                        .declare_func_in_func(*function, &mut context.func),
                )
            })
            .collect();
        let handler_tasks = super::super::statements::TaskCodegenContext {
            refs: handler_refs,
            thunks: HashMap::new(),
            handler_thunks,
            result_drops: HashMap::new(),
            failure_drops: HashMap::new(),
            function_types,
            groups: HashMap::new(),
            tasks: HashMap::new(),
            handler_frames: Vec::new(),
        };
        let materialized_literals = string_literals
            .iter()
            .map(|stored| {
                (
                    self.module.materialize_string(
                        &mut context.func,
                        stored.as_ptr(),
                        stored.len(),
                    ),
                    stored.len(),
                )
            })
            .collect::<Vec<_>>();
        let map_key_refs = self.map_key_refs(&mut context.func);
        let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
        let resume_span = function.blocks[continuation.resume_block.0]
            .statements
            .iter()
            .find_map(|statement| function.statement_span(statement));
        builder.set_srcloc(crate::codegen::cranelift::source_location(resume_span));
        let setup = setup_continuation_entry(
            &mut builder,
            function,
            continuation,
            function_types,
            &support_refs,
            entry_refs,
            types,
            pointer_type,
            pending_refs.take,
        )?;
        let entry = setup.entry;
        let resume_blocks = setup.resume_blocks;
        let blocks = setup.blocks;
        let phi_values = setup.phi_values;
        let continuation_handle = setup.continuation_handle;
        let frame = setup.frame;
        let spill = setup.spill;
        let spill_layout =
            crate::codegen::functions::spills::SpillLayout::new(function, pointer_type, types);
        let task_layout = crate::codegen::functions::task_frame::TaskFrameLayout::new(
            function,
            pointer_type,
            types,
        );
        let mut values = setup.values;
        // A resumed loop can reach a value's original definition again. Track
        // restored values through Cranelift SSA variables so a definition on
        // one path does not replace the entry value on an unrelated path.
        let mut restored_variables = HashMap::new();
        for (&id, value) in &values {
            let components = value_arguments(value.clone());
            let variables = components
                .iter()
                .map(|&component| {
                    let variable = builder.declare_var(builder.func.dfg.value_type(component));
                    builder.def_var(variable, component);
                    variable
                })
                .collect::<Vec<_>>();
            restored_variables.insert(id, variables);
        }
        let mut environments = setup.environments;
        let mut restored_locals = HashMap::new();
        for slot in &continuation.frame_slots {
            let value = environments[continuation.resume_block.0]
                .as_ref()
                .unwrap()
                .lookup_local(slot.local)
                .unwrap();
            let variables = value_arguments(value)
                .into_iter()
                .map(|component| {
                    let variable = builder.declare_var(builder.func.dfg.value_type(component));
                    builder.def_var(variable, component);
                    variable
                })
                .collect::<Vec<_>>();
            restored_locals.insert(slot.local, variables);
        }
        let class_drop_refs = &support_refs.class_drop_refs;
        let continuation_cleanup_drop_refs = &support_refs.continuation_cleanup_drop_refs;
        let task_thunk_refs = &support_refs.task_thunk_refs;
        let task_result_drop_refs = &support_refs.task_result_drop_refs;
        let mut literals = resume_blocks.iter().flat_map(|block_id| {
            function.blocks[block_id.0]
                .statements
                .iter()
                .flat_map(|statement| {
                    let mut strings = Vec::new();
                    match statement {
                        MirStatement::Const { value, .. } => {
                            crate::codegen::cranelift::collect_mir_constant_strings(
                                value,
                                &mut strings,
                            );
                        }
                        MirStatement::HandlerEnter { handlers } => {
                            for handler in handlers {
                                if let Some(value) = &handler.resumable_value {
                                    crate::codegen::cranelift::collect_mir_constant_strings(
                                        value,
                                        &mut strings,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                    strings.into_iter().filter_map(|value| {
                        string_literals
                            .iter()
                            .position(|stored| stored.as_ref() == value.as_bytes())
                            .map(|index| {
                                let (literal, length) = materialized_literals[index];
                                (literal, length)
                            })
                    })
                })
        });

        for block_id in &resume_blocks {
            let block = &function.blocks[block_id.0];
            let cranelift_block = blocks[block_id.0].expect("created continuation CFG block");
            let mut environment =
                environments[block_id.0]
                    .take()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "continuation CFG block has no incoming environment".to_owned(),
                    })?;
            environment.map_key_refs = map_key_refs.clone();
            if block.scoped {
                environment.push_scope();
            }
            builder.switch_to_block(cranelift_block);
            for (&id, variables) in &restored_variables {
                let components = variables
                    .iter()
                    .map(|&var| builder.use_var(var))
                    .collect::<Vec<_>>();
                let value = crate::codegen::abi::value_from_params(
                    &components,
                    function.value_types[id.0],
                    types,
                )
                .map_err(|error| CodegenError::RuntimeError {
                    message: error.to_string(),
                })?;
                values.insert(id, value);
            }
            for (&local, variables) in &restored_locals {
                if environment.lookup_local(local).is_some() {
                    let components = variables
                        .iter()
                        .map(|&var| builder.use_var(var))
                        .collect::<Vec<_>>();
                    let value = crate::codegen::abi::value_from_params(
                        &components,
                        function.locals[local.0].ty,
                        types,
                    )
                    .map_err(|error| CodegenError::RuntimeError {
                        message: error.to_string(),
                    })?;
                    environment.bind_local(local, value);
                }
            }
            values.extend(phi_values[block_id.0].clone());
            let mut resuspended = false;
            for statement in &block.statements {
                builder.set_srcloc(crate::codegen::cranelift::source_location(
                    function.statement_span(statement),
                ));
                match &statement {
                    MirStatement::Resume { .. }
                    | MirStatement::Phi { .. }
                    | MirStatement::Deinit { .. } => {}
                    MirStatement::HandlerEnter { handlers } => {
                        let handler = super::super::statements::emit_handler_enter(
                            &mut builder,
                            function,
                            types,
                            pointer_type,
                            &mut literals,
                            &handler_tasks,
                            runtime_call_refs,
                            handlers,
                            &mut values,
                            &mut environment,
                        )?;
                        builder
                            .ins()
                            .call(pending_refs.own_handler, &[continuation_handle, handler]);
                    }
                    MirStatement::HandlerExit => {
                        builder
                            .ins()
                            .call(pending_refs.exit_handler, &[continuation_handle]);
                    }
                    MirStatement::Store {
                        destination,
                        receiver,
                        access,
                        value,
                    } => {
                        let receiver = values.get(receiver).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resumed Store receiver is missing".into(),
                            }
                        })?;
                        let value = values.get(value).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resumed Store value is missing".into(),
                            }
                        })?;
                        compile_mir_store(
                            &mut builder,
                            receiver,
                            access,
                            value,
                            pointer_type,
                            types,
                            &environment,
                        )?;
                        values.insert(*destination, CompiledValue::Unit);
                    }
                    MirStatement::Unit { destination } => {
                        values.insert(*destination, CompiledValue::Unit);
                    }
                    MirStatement::Const { destination, value } => {
                        let compiled = compile_mir_constant(
                            &mut builder,
                            value,
                            function.value_types[destination.0],
                            pointer_type,
                            types,
                            &mut literals,
                            string_from_ref,
                            runtime_call_refs.list_cons,
                            runtime_call_refs.map_insert,
                            runtime_call_refs.allocate,
                            &environment,
                        )?;
                        values.insert(*destination, compiled);
                    }
                    MirStatement::Read { destination, local } => {
                        let value = compile_mir_read_stackless(
                            &mut builder,
                            *local,
                            pointer_type,
                            &environment,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::BorrowLocal { destination, local } => {
                        let value = compile_mir_read_stackless(
                            &mut builder,
                            *local,
                            pointer_type,
                            &environment,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::TakeLocal { destination, local } => {
                        let value = environment.take_local(*local).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "stackless machine entry cannot take local l{}: it was \
                                     never restored into the continuation environment or was \
                                     already moved",
                                    local.0
                                ),
                            }
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Move { destination, value } => {
                        let value = values.get(value).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume move value missing".to_owned(),
                            }
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Dup { destination, value } => {
                        let value = values.get(value).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume dup value missing".to_owned(),
                            }
                        })?;
                        values.insert(
                            *destination,
                            duplicate_shared_value(&mut builder, value, dup_ref)?,
                        );
                    }
                    MirStatement::Drop { destination, value } => {
                        let value = values.get(value).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume drop value missing".to_owned(),
                            }
                        })?;
                        compile_drop_value(&mut builder, value, &environment, pointer_type, types)?;
                        values.insert(*destination, CompiledValue::Unit);
                    }
                    MirStatement::DropLocal { destination, local } => {
                        let value = environment.take_local(*local).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "stackless machine entry cannot drop local l{}: it was \
                                     never restored into the continuation environment or was \
                                     already moved",
                                    local.0
                                ),
                            }
                        })?;
                        compile_drop_value(&mut builder, value, &environment, pointer_type, types)?;
                        values.insert(*destination, CompiledValue::Unit);
                    }
                    MirStatement::Unary {
                        destination,
                        op,
                        operand,
                    } => {
                        let operand = values.get(operand).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume operand missing".to_owned(),
                            }
                        })?;
                        let value = compile_unary_value(
                            &mut builder,
                            *op,
                            operand,
                            function.value_types[destination.0],
                            pointer_type,
                            runtime_call_refs.panic,
                        )
                        .map_err(|error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Binary {
                        destination,
                        op,
                        left,
                        right,
                    } => {
                        let left = values.get(left).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume left operand missing".to_owned(),
                            }
                        })?;
                        let right = values.get(right).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume right operand missing".to_owned(),
                            }
                        })?;
                        let value = compile_binary_value(
                            &mut builder,
                            *op,
                            left,
                            right,
                            function.value_types[destination.0],
                            pointer_type,
                            runtime_call_refs.panic,
                        )
                        .map_err(|error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Numeric {
                        destination,
                        method,
                        arguments,
                    } => {
                        let compiled = arguments
                            .iter()
                            .map(|argument| {
                                values.get(argument).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "resume numeric operand missing".to_owned(),
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let value = compile_numeric_method(
                            &mut builder,
                            *method,
                            &compiled,
                            function.value_types[destination.0],
                            pointer_type,
                            runtime_call_refs.panic,
                        )
                        .map_err(|error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Project {
                        destination,
                        base,
                        access,
                    } => {
                        let base = values.get(base).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume projection base missing".to_owned(),
                            }
                        })?;
                        let value =
                            compile_mir_project(&mut builder, base, access, pointer_type, types)?;
                        values.insert(*destination, value);
                    }
                    MirStatement::EnumTag { destination, value } => {
                        let value = values.get(value).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume enum tag value missing".to_owned(),
                            }
                        })?;
                        let CompiledValue::Enum { tag, .. } = value else {
                            return Err(CodegenError::RuntimeError {
                                message: "resume enum tag reads a non-enum value".to_owned(),
                            });
                        };
                        values.insert(
                            *destination,
                            CompiledValue::Numeric {
                                value: tag,
                                ty: Type::I32,
                            },
                        );
                    }
                    MirStatement::EnumProject {
                        destination,
                        value,
                        variant,
                        field,
                    } => {
                        let value = values.get(value).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume enum projection value missing".to_owned(),
                            }
                        })?;
                        let value = compile_mir_enum_project(value, *variant, *field, types)?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Tuple {
                        destination,
                        elements,
                    } => {
                        let elements = elements
                            .iter()
                            .map(|element| {
                                values.get(element).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "resume tuple element missing".to_owned(),
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        values.insert(
                            *destination,
                            CompiledValue::Tuple {
                                elements,
                                ty: function.value_types[destination.0],
                            },
                        );
                    }
                    MirStatement::EnumConstruct {
                        destination,
                        enum_id,
                        variant,
                        arguments,
                    } => {
                        let value = compile_mir_enum_construct(
                            &mut builder,
                            *enum_id,
                            *variant,
                            arguments,
                            &values,
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Construct {
                        destination,
                        type_id,
                        fields,
                    } => {
                        let value = compile_mir_construct(
                            &mut builder,
                            *type_id,
                            fields,
                            &values,
                            function.value_types[destination.0],
                            allocate_ref,
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::Bind {
                        local,
                        value,
                        destination,
                    } => {
                        let value = value
                            .map(|value| {
                                values.get(&value).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "resume binding value missing".to_owned(),
                                    }
                                })
                            })
                            .transpose()?;
                        let value = value.unwrap_or(CompiledValue::Unit);
                        if let Some(variables) = restored_locals.get(local) {
                            for (&variable, component) in
                                variables.iter().zip(value_arguments(value.clone()))
                            {
                                builder.def_var(variable, component);
                            }
                        }
                        environment.bind_local(*local, value);
                        values.insert(*destination, CompiledValue::Unit);
                    }
                    MirStatement::Call {
                        destination,
                        function: callee,
                        arguments,
                        continuation: call_continuation,
                        ..
                    } => {
                        if let Some(call_continuation) = call_continuation {
                            calls::compile_resumed_call(
                                &mut builder,
                                function,
                                *call_continuation,
                                Some(*callee),
                                None,
                                None,
                                arguments,
                                *destination,
                                continuation_handle,
                                &mut values,
                                &mut environment,
                                continuation_entry_keys,
                                allocation_refs,
                                pending_refs,
                                entry_refs,
                                continuation_cleanup_drop_refs,
                                types,
                                pointer_type,
                            )?;
                            if let Some(variables) = restored_variables.get(destination) {
                                for (&variable, component) in variables
                                    .iter()
                                    .zip(value_arguments(values[destination].clone()))
                                {
                                    builder.def_var(variable, component);
                                }
                            }
                            continue;
                        }
                        let value = compile_mir_call(
                            &mut builder,
                            callee,
                            arguments,
                            &values,
                            &environment,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::DynamicUpcast { destination, value } => {
                        let result = super::super::values::compile_dynamic_upcast(
                            &mut builder,
                            values[value].clone(),
                            function.value_types[destination.0],
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, result);
                    }
                    MirStatement::DynamicValue {
                        destination,
                        methods,
                        value,
                    } => {
                        let result = super::super::values::compile_dynamic_environment(
                            &mut builder,
                            function.value_types[destination.0],
                            values[value].clone(),
                            function.value_types[value.0],
                            methods,
                            &environment,
                            closure_allocate_ref,
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, result);
                    }
                    MirStatement::FunctionValue {
                        destination,
                        function: closure,
                        captures,
                    } => {
                        let reference = environment
                            .closure_call_refs
                            .get(closure)
                            .ok_or_else(|| CodegenError::RuntimeError {
                                message: "resume closure call glue was not registered".to_owned(),
                            })?
                            .to_owned();
                        let drop_reference = environment
                            .closure_drop_refs
                            .get(closure)
                            .ok_or_else(|| CodegenError::RuntimeError {
                                message: "resume closure drop glue was not registered".to_owned(),
                            })?
                            .to_owned();
                        let closure_type = function.value_types[destination.0];
                        let Type::Function(signature_id) = closure_type else {
                            return Err(CodegenError::RuntimeError {
                                message: "resume closure destination is not a function type"
                                    .to_owned(),
                            });
                        };
                        let user_parameter_count =
                            types.function_type(signature_id).parameters.len();
                        let target_type =
                            environment.function_types.get(closure).ok_or_else(|| {
                                CodegenError::RuntimeError {
                                    message:
                                        "resume closure target function type was not registered"
                                            .to_owned(),
                                }
                            })?;
                        if target_type.parameters.len() < user_parameter_count
                            || target_type.parameters.len() - user_parameter_count != captures.len()
                        {
                            return Err(CodegenError::RuntimeError {
                                message:
                                    "resume closure capture count does not match target signature"
                                        .to_owned(),
                            });
                        }
                        let capture_types = target_type.parameters[..captures.len()].to_vec();
                        let captures = captures
                            .iter()
                            .map(|capture| {
                                values.get(capture).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "resume closure capture was not compiled"
                                            .to_owned(),
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let code = builder.ins().func_addr(pointer_type, reference);
                        let drop_callback = builder.ins().func_addr(pointer_type, drop_reference);
                        let value = compile_closure_environment(
                            &mut builder,
                            code,
                            closure_type,
                            captures,
                            capture_types,
                            drop_callback,
                            closure_allocate_ref,
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::CallIndirect {
                        destination,
                        callee,
                        arguments,
                        continuation: call_continuation,
                        ..
                    } => {
                        let callee = values.get(callee).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume indirect callee was not compiled".to_owned(),
                            }
                        })?;
                        if let Some(call_continuation) = call_continuation {
                            calls::compile_resumed_call(
                                &mut builder,
                                function,
                                *call_continuation,
                                None,
                                Some(callee),
                                None,
                                arguments,
                                *destination,
                                continuation_handle,
                                &mut values,
                                &mut environment,
                                continuation_entry_keys,
                                allocation_refs,
                                pending_refs,
                                entry_refs,
                                continuation_cleanup_drop_refs,
                                types,
                                pointer_type,
                            )?;
                            if let Some(variables) = restored_variables.get(destination) {
                                for (&variable, component) in variables
                                    .iter()
                                    .zip(value_arguments(values[destination].clone()))
                                {
                                    builder.def_var(variable, component);
                                }
                            }
                            continue;
                        }
                        let value = compile_mir_indirect_call(
                            &mut builder,
                            callee,
                            arguments,
                            &values,
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::MethodCall {
                        destination,
                        receiver,
                        receiver_type,
                        method,
                        arguments,
                        continuation: call_continuation,
                    } => {
                        if let Some(call_continuation) = call_continuation {
                            calls::compile_resumed_call(
                                &mut builder,
                                function,
                                *call_continuation,
                                Some(*method),
                                None,
                                Some(*receiver),
                                arguments,
                                *destination,
                                continuation_handle,
                                &mut values,
                                &mut environment,
                                continuation_entry_keys,
                                allocation_refs,
                                pending_refs,
                                entry_refs,
                                continuation_cleanup_drop_refs,
                                types,
                                pointer_type,
                            )?;
                            if let Some(variables) = restored_variables.get(destination) {
                                for (&variable, component) in variables
                                    .iter()
                                    .zip(value_arguments(values[destination].clone()))
                                {
                                    builder.def_var(variable, component);
                                }
                            }
                            continue;
                        }
                        let receiver = values.get(receiver).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume method receiver was not compiled".to_owned(),
                            }
                        })?;
                        let value = compile_mir_method_call(
                            &mut builder,
                            receiver,
                            *receiver_type,
                            *method,
                            arguments,
                            &values,
                            &environment,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::TaskPoll { destination } => {
                        let call = builder.ins().call(is_cancelled_ref, &[continuation_handle]);
                        values.insert(
                            *destination,
                            CompiledValue::Boolean {
                                value: builder.inst_results(call)[0],
                            },
                        );
                    }
                    MirStatement::TaskCancelled { destination } => {
                        let value = zero_compiled_value(
                            &mut builder,
                            function.return_type,
                            pointer_type,
                            types,
                        )
                        .map_err(|error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::TaskFailureOperation { destination, scope } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        let call = builder.ins().call(failure_operation_ref, &[group]);
                        values.insert(
                            *destination,
                            CompiledValue::Numeric {
                                value: builder.inst_results(call)[0],
                                ty: Type::U64,
                            },
                        );
                    }
                    MirStatement::TaskFailurePayload {
                        destination,
                        scope,
                        operation,
                        parameter,
                    } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        let info = types.effects().operation_info(*operation).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "continuation task failure operation is unknown"
                                    .to_owned(),
                            }
                        })?;
                        let ty = *info.parameters.get(*parameter).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "continuation task failure parameter is invalid"
                                    .to_owned(),
                            }
                        })?;
                        let offset = info.parameters[..*parameter]
                            .iter()
                            .map(|ty| abi_types(*ty, pointer_type, types).len() * 8)
                            .sum();
                        let call = builder.ins().call(failure_payload_ref, &[group]);
                        let payload = builder.inst_results(call)[0];
                        let value = load_task_value(
                            &mut builder,
                            payload,
                            offset,
                            ty,
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::TaskFailureClaim { scope } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        builder.ins().call(failure_claim_ref, &[group]);
                    }
                    MirStatement::TaskFailureRethrow { destination, scope } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        builder.ins().call(failure_rethrow_ref, &[group]);
                        let value = zero_compiled_value(
                            &mut builder,
                            function.value_types[destination.0],
                            pointer_type,
                            types,
                        )
                        .map_err(|error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::TaskCreate {
                        function: task_function,
                        task,
                        scope,
                        arguments,
                        ..
                    } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        let thunk =
                            task_thunk_refs.get(task_function).copied().ok_or_else(|| {
                                CodegenError::RuntimeError {
                                    message: "resume task thunk was not registered".to_owned(),
                                }
                            })?;
                        let function_type = function_types.get(task_function).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resume task function type is missing".to_owned(),
                            }
                        })?;
                        let mut flattened = Vec::new();
                        for (parameter, ty) in function_type.parameters.iter().enumerate() {
                            let argument = arguments
                                .iter()
                                .find(|argument| argument.parameter == parameter)
                                .ok_or_else(|| CodegenError::RuntimeError {
                                    message: "resume task capture is missing".to_owned(),
                                })?;
                            let value = values.get(&argument.value).cloned().ok_or_else(|| {
                                CodegenError::RuntimeError {
                                    message: "resume task capture was not compiled".to_owned(),
                                }
                            })?;
                            if super::super::super::abi::value_type(value.clone()) != *ty {
                                return Err(CodegenError::RuntimeError {
                                    message: "resume task capture type mismatch".to_owned(),
                                });
                            }
                            flattened.extend(value_arguments(value));
                        }
                        let (capture_slot, capture_pointer) = if flattened.is_empty() {
                            (None, builder.ins().iconst(pointer_type, 0))
                        } else {
                            let (slot, pointer) =
                                create_task_slot(&mut builder, pointer_type, flattened.len())?;
                            for (index, value) in flattened.iter().enumerate() {
                                builder.ins().stack_store(
                                    pointer_type,
                                    *value,
                                    slot,
                                    (index * 8) as i32,
                                );
                            }
                            (Some(slot), pointer)
                        };
                        let result_size = builder.ins().iconst(
                            pointer_type,
                            (abi_types(function_type.return_type, pointer_type, types).len() * 8)
                                as i64,
                        );
                        let result_drop = task_result_drop_refs
                            .get(task_function)
                            .copied()
                            .map(|drop| builder.ins().func_addr(pointer_type, drop))
                            .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
                        let thunk = builder.ins().func_addr(pointer_type, thunk);
                        let capture_size = builder
                            .ins()
                            .iconst(pointer_type, (flattened.len() * 8) as i64);
                        let spawn = builder.ins().call(
                            spawn_heap_ref,
                            &[
                                group,
                                thunk,
                                capture_pointer,
                                capture_size,
                                result_size,
                                result_drop,
                            ],
                        );
                        let id = builder.inst_results(spawn)[0];
                        environment.bind_task_handle(*task, id);
                        builder.ins().store(
                            MemFlagsData::new(),
                            id,
                            frame,
                            task_layout.task_offset(*task).expect("task frame slot"),
                        );
                        let _ = capture_slot;
                    }
                    MirStatement::TaskWait {
                        continuation: next, ..
                    }
                    | MirStatement::CownAcquire {
                        continuation: next, ..
                    } => {
                        let metadata = &function.continuations[next.0];
                        let cown_arguments = if let MirStatement::CownAcquire {
                            wait_for_change,
                            arguments,
                            destination,
                            ..
                        } = statement
                        {
                            let (handles, count) = super::super::pending::cown_handle_array(
                                &mut builder,
                                pointer_type,
                                arguments,
                                &values,
                            )?;
                            let status = if *wait_for_change {
                                builder.ins().iconst(cranelift_codegen::ir::types::I8, 1)
                            } else {
                                let call =
                                    builder.ins().call(pending_refs.cown_try, &[handles, count]);
                                builder.inst_results(call)[0]
                            };
                            let slow = builder.create_block();
                            let ready = blocks[metadata.resume_block.0]
                                .expect("Cown fast-path resume block");
                            builder.ins().brif(status, slow, &[], ready, &[]);
                            if environments[metadata.resume_block.0].is_none() {
                                environments[metadata.resume_block.0] = Some(environment.clone());
                            }
                            builder.switch_to_block(slow);
                            builder.seal_block(slow);
                            values.insert(*destination, CompiledValue::Unit);
                            Some((handles, count, status, *wait_for_change))
                        } else {
                            None
                        };
                        let local_layout =
                            crate::codegen::functions::task_frame::LocalFrameLayout::new(
                                function,
                                pointer_type,
                                types,
                            );
                        for slot in &metadata.frame_slots {
                            let mut offset = local_layout.offset(slot.local) as i32;
                            let value = slot
                                .value
                                .and_then(|id| values.get(&id).cloned())
                                .or_else(|| environment.lookup_local(slot.local))
                                .ok_or_else(|| CodegenError::RuntimeError {
                                    message: format!("task wait local {} is missing", slot.local.0),
                                })?;
                            for component in value_arguments(value) {
                                builder
                                    .ins()
                                    .store(MemFlagsData::new(), component, frame, offset);
                                offset += 8;
                            }
                        }
                        for scope in &task_layout.scopes {
                            let group = environment
                                .task_group(*scope)
                                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
                            builder.ins().store(
                                MemFlagsData::new(),
                                group,
                                frame,
                                task_layout.scope_offset(*scope).unwrap(),
                            );
                        }
                        for task in &task_layout.tasks {
                            let id = environment
                                .task_handle(*task)
                                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
                            builder.ins().store(
                                MemFlagsData::new(),
                                id,
                                frame,
                                task_layout.task_offset(*task).unwrap(),
                            );
                        }
                        spill_layout.save(&mut builder, metadata, spill, &values)?;
                        let key = builder
                            .ins()
                            .iconst(pointer_type, continuation_entry_keys[next] as i64);
                        builder
                            .ins()
                            .call(set_resume_entry_ref, &[continuation_handle, key]);
                        let pc = builder
                            .ins()
                            .iconst(pointer_type, metadata.resume_block.0 as i64);
                        builder
                            .ins()
                            .call(set_program_counter_ref, &[continuation_handle, pc]);
                        let parent = builder
                            .ins()
                            .call(pending_refs.parent, &[continuation_handle]);
                        let parent = builder.inst_results(parent)[0];
                        let call = if let Some((handles, count, status, wait_for_change)) =
                            cown_arguments
                        {
                            // All owned live values are now saved; cancellation
                            // can run the normal activation cleanup exactly once.
                            let start = builder.create_block();
                            let failed = builder.create_block();
                            let contended = builder.ins().icmp_imm_u(
                                cranelift_codegen::ir::condcodes::IntCC::Equal,
                                status,
                                1,
                            );
                            builder.ins().brif(contended, start, &[], failed, &[]);
                            builder.switch_to_block(failed);
                            builder
                                .ins()
                                .call(pending_refs.fail_chain, &[continuation_handle, status]);
                            builder.ins().return_(&[]);
                            builder.switch_to_block(start);
                            builder.seal_block(failed);
                            builder.seal_block(start);
                            builder.ins().call(
                                if wait_for_change {
                                    pending_refs.cown_wait
                                } else {
                                    pending_refs.cown_acquire
                                },
                                &[continuation_handle, handles, count, parent],
                            )
                        } else {
                            let MirStatement::TaskWait { scope, race, .. } = statement else {
                                unreachable!()
                            };
                            let group =
                                environment.task_group(*scope).expect("verified wait scope");
                            let mode = builder.ins().iconst(pointer_type, i64::from(*race));
                            builder.ins().call(
                                pending_refs.task_wait,
                                &[continuation_handle, group, mode, parent],
                            )
                        };
                        let status = builder.inst_results(call)[0];
                        let failed = builder.ins().icmp_imm_u(
                            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                            status,
                            1,
                        );
                        let cancel = builder.create_block();
                        let done = builder.create_block();
                        builder.ins().brif(failed, cancel, &[], done, &[]);
                        builder.switch_to_block(cancel);
                        builder
                            .ins()
                            .call(pending_refs.fail_chain, &[continuation_handle, status]);
                        builder.ins().jump(done, &[]);
                        builder.switch_to_block(done);
                        builder.seal_block(cancel);
                        builder.seal_block(done);
                        builder.ins().return_(&[]);
                        resuspended = true;
                        break;
                    }
                    MirStatement::TaskJoin {
                        destination,
                        scope,
                        task,
                    } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        let handle = environment.task_handle(*task).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "continuation task join references an unknown task"
                                    .to_owned(),
                            }
                        })?;
                        builder.ins().call(task_join_ref, &[group, handle]);
                        let call = builder
                            .ins()
                            .call(task_result_pointer_ref, &[group, handle]);
                        let result = builder.inst_results(call)[0];
                        let value = load_task_value(
                            &mut builder,
                            result,
                            0,
                            function.value_types[destination.0],
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::TaskClaimResult { scope, task } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        let handle = environment.task_handle(*task).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "continuation task claim references an unknown task"
                                    .to_owned(),
                            }
                        })?;
                        builder.ins().call(task_claim_result_ref, &[group, handle]);
                    }
                    MirStatement::TaskCancel { scope, task } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        let handle = environment.task_handle(*task).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "continuation task cancel references an unknown task"
                                    .to_owned(),
                            }
                        })?;
                        builder.ins().call(task_cancel_ref, &[group, handle]);
                    }
                    MirStatement::RaceStart { .. } => {}
                    MirStatement::RaceSelect {
                        destination,
                        scope,
                        tasks,
                    } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} has no runtime group handle",
                                    scope.0
                                ),
                            }
                        })?;
                        let ids = tasks
                            .iter()
                            .map(|task| {
                                environment.task_handle(*task).ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "continuation race references an unknown task"
                                            .to_owned(),
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let (ids_slot, ids_pointer) =
                            create_task_slot(&mut builder, pointer_type, ids.len())?;
                        for (index, id) in ids.iter().enumerate() {
                            builder.ins().stack_store(
                                pointer_type,
                                *id,
                                ids_slot,
                                (index * 8) as i32,
                            );
                        }
                        let ty = function.value_types[destination.0];
                        let component_count = abi_types(ty, pointer_type, types).len();
                        let (result_slot, result_pointer) =
                            create_task_slot(&mut builder, pointer_type, component_count)?;
                        // When every arm aborts, race leaves the output untouched.
                        let zero = zero_compiled_value(&mut builder, ty, pointer_type, types)
                            .map_err(|error| CodegenError::RuntimeError {
                                message: error.to_string(),
                            })?;
                        for (index, component) in value_arguments(zero).iter().enumerate() {
                            builder.ins().stack_store(
                                pointer_type,
                                *component,
                                result_slot,
                                (index * 8) as i32,
                            );
                        }
                        let count = builder.ins().iconst(pointer_type, ids.len() as i64);
                        let size = builder
                            .ins()
                            .iconst(pointer_type, (component_count * 8) as i64);
                        builder
                            .ins()
                            .call(race_ref, &[group, ids_pointer, count, result_pointer, size]);
                        let value = load_task_value(
                            &mut builder,
                            result_pointer,
                            0,
                            ty,
                            pointer_type,
                            types,
                        )?;
                        values.insert(*destination, value);
                    }
                    MirStatement::ScopeEnter { scope, region } => {
                        if scope.0 != 0 {
                            let region = builder.ins().iconst(pointer_type, i64::from(*region));
                            let call = builder.ins().call(group_new_ref, &[region]);
                            let group = builder.inst_results(call)[0];
                            environment.bind_task_group(*scope, group);
                            builder.ins().store(
                                MemFlagsData::new(),
                                group,
                                frame,
                                task_layout.scope_offset(*scope).expect("scope frame slot"),
                            );
                        }
                    }
                    MirStatement::ScopeExit { scope } => {
                        let group = environment.task_group(*scope).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: format!(
                                    "continuation task scope s{} exits before entering",
                                    scope.0
                                ),
                            }
                        })?;
                        if let Some(offset) = task_layout.scope_offset(*scope) {
                            let zero = builder.ins().iconst(pointer_type, 0);
                            builder
                                .ins()
                                .store(MemFlagsData::new(), zero, frame, offset);
                        }
                        builder.ins().call(group_free_ref, &[group]);
                        environment.forget_task_group(*scope, function);
                    }
                    MirStatement::Suspend {
                        destination,
                        operation,
                        continuation: next_continuation,
                        arguments,
                    } => {
                        let metadata = function
                            .continuations
                            .iter()
                            .find(|item| item.id == *next_continuation)
                            .ok_or_else(|| CodegenError::RuntimeError {
                                message: "resumed suspend continuation metadata is missing"
                                    .to_owned(),
                            })?;
                        let info = types.effects().operation_info(*operation).ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "resumed suspend operation was not resolved".to_owned(),
                            }
                        })?;
                        if continuation_protocol(function, *next_continuation)?
                            != crate::mir::MirContinuationKind::Suspending
                        {
                            return Err(CodegenError::RuntimeError {
                                message: "resumed Suspend requires a suspending continuation"
                                    .to_owned(),
                            });
                        }
                        // The current machine handle is reused when this
                        // resume tail reaches another suspension. Replace the
                        // previous operation's cleanup descriptors and result
                        // layout before publishing the next request.
                        builder
                            .ins()
                            .call(clear_suspend_cleanups_ref, &[continuation_handle]);
                        // The machine handle is reused across suspensions. A
                        // later continuation can introduce frame locals that
                        // were absent from the original entry, so refresh
                        // their owned-word cleanup descriptors here.
                        let local_layout =
                            crate::codegen::functions::task_frame::LocalFrameLayout::new(
                                function,
                                pointer_type,
                                types,
                            );
                        for slot in metadata.frame_slots.iter().rev() {
                            register_continuation_cleanups(
                                &mut builder,
                                continuation_handle,
                                CONTINUATION_FRAME_STORAGE,
                                slot.ty,
                                local_layout.offset(slot.local),
                                pointer_type,
                                types,
                                register_cleanup_ref,
                                register_cleanup_region_ref,
                                runtime_call_refs.managed_drop,
                                class_drop_refs,
                                continuation_cleanup_drop_refs,
                            )?;
                        }
                        let result_size = abi_types(info.return_type, pointer_type, types)
                            .len()
                            .saturating_mul(8);
                        if result_size != 0 {
                            let size = builder.ins().iconst(pointer_type, result_size as i64);
                            builder
                                .ins()
                                .call(alloc_suspend_result_ref, &[continuation_handle, size]);
                        }
                        register_continuation_cleanups(
                            &mut builder,
                            continuation_handle,
                            CONTINUATION_SUSPEND_RESULT_STORAGE,
                            info.return_type,
                            0,
                            pointer_type,
                            types,
                            register_cleanup_ref,
                            register_cleanup_region_ref,
                            runtime_call_refs.managed_drop,
                            class_drop_refs,
                            continuation_cleanup_drop_refs,
                        )?;
                        let mut argument_offset = 0_usize;
                        for (parameter_type, borrowed) in
                            info.parameters.iter().zip(&info.parameter_borrows)
                        {
                            if *borrowed {
                                argument_offset +=
                                    abi_types(*parameter_type, pointer_type, types).len() * 8;
                                continue;
                            }
                            register_continuation_cleanups(
                                &mut builder,
                                continuation_handle,
                                CONTINUATION_SUSPEND_ARGUMENT_STORAGE,
                                *parameter_type,
                                argument_offset,
                                pointer_type,
                                types,
                                register_cleanup_ref,
                                register_cleanup_region_ref,
                                runtime_call_refs.managed_drop,
                                class_drop_refs,
                                continuation_cleanup_drop_refs,
                            )?;
                            argument_offset +=
                                abi_types(*parameter_type, pointer_type, types).len() * 8;
                        }
                        let mut argument_slot = None;
                        let duration = if is_time_sleep_operation(types, *operation) {
                            let [argument] = arguments.as_slice() else {
                                return Err(CodegenError::RuntimeError {
                                    message:
                                        "resumed suspending operation expects one Duration argument"
                                            .to_owned(),
                                });
                            };
                            let CompiledValue::Numeric {
                                value: duration,
                                ty,
                            } = values.get(&argument.value).cloned().ok_or_else(|| {
                                CodegenError::RuntimeError {
                                    message: "resumed suspension duration was not compiled"
                                        .to_owned(),
                                }
                            })?
                            else {
                                return Err(CodegenError::RuntimeError {
                                    message: "resumed suspension duration is not numeric"
                                        .to_owned(),
                                });
                            };
                            if ty != Type::Duration {
                                return Err(CodegenError::RuntimeError {
                                    message: "resumed suspension has an invalid duration type"
                                        .to_owned(),
                                });
                            }
                            Some(duration)
                        } else {
                            if arguments.len() != info.parameters.len() {
                                return Err(CodegenError::RuntimeError {
                                    message: "resumed suspending operation argument count mismatch"
                                        .to_owned(),
                                });
                            }
                            let mut flattened = Vec::new();
                            for (argument, parameter_type) in arguments.iter().zip(&info.parameters)
                            {
                                let value =
                                    values.get(&argument.value).cloned().ok_or_else(|| {
                                        CodegenError::RuntimeError {
                                            message: "resumed suspend argument was not compiled"
                                                .to_owned(),
                                        }
                                    })?;
                                let components = value_arguments(value);
                                let expected = abi_types(*parameter_type, pointer_type, types);
                                if components.len() != expected.len() {
                                    return Err(CodegenError::RuntimeError {
                                        message: "resumed suspend argument ABI shape mismatch"
                                            .to_owned(),
                                    });
                                }
                                flattened.extend(components);
                            }
                            let (slot, pointer) = if flattened.is_empty() {
                                (None, builder.ins().iconst(pointer_type, 0))
                            } else {
                                let (slot, pointer) =
                                    create_task_slot(&mut builder, pointer_type, flattened.len())?;
                                for (index, value) in flattened.iter().enumerate() {
                                    builder.ins().store(
                                        MemFlagsData::new(),
                                        *value,
                                        pointer,
                                        (index * 8) as i32,
                                    );
                                }
                                (Some(slot), pointer)
                            };
                            argument_slot = Some((slot, pointer, flattened.len() * 8));
                            None
                        };
                        let local_layout =
                            crate::codegen::functions::task_frame::LocalFrameLayout::new(
                                function,
                                pointer_type,
                                types,
                            );
                        for slot in &metadata.frame_slots {
                            let mut frame_offset = local_layout.offset(slot.local) as i32;
                            let value = slot
                                .value
                                .and_then(|value| values.get(&value).cloned())
                                .or_else(|| environment.lookup_local(slot.local))
                                .ok_or_else(|| CodegenError::RuntimeError {
                                    message: format!(
                                        "resumed continuation frame local {} was not compiled",
                                        slot.local.0
                                    ),
                                })?;
                            let components = value_arguments(value);
                            let component_types = abi_types(slot.ty, pointer_type, types);
                            if components.len() != component_types.len() {
                                return Err(CodegenError::RuntimeError {
                                    message: "resumed continuation frame ABI shape mismatch"
                                        .to_owned(),
                                });
                            }
                            for component in components {
                                builder.ins().store(
                                    MemFlagsData::new(),
                                    component,
                                    frame,
                                    frame_offset,
                                );
                                frame_offset += 8;
                            }
                        }
                        spill_layout.save(&mut builder, metadata, spill, &values)?;
                        let entry_key =
                            continuation_entry_keys
                                .get(next_continuation)
                                .ok_or_else(|| CodegenError::RuntimeError {
                                    message: "resumed continuation has no machine entry key"
                                        .to_owned(),
                                })?;
                        let entry_key = builder.ins().iconst(pointer_type, *entry_key as i64);
                        builder
                            .ins()
                            .call(set_resume_entry_ref, &[continuation_handle, entry_key]);
                        let program_counter = builder
                            .ins()
                            .iconst(pointer_type, metadata.resume_block.0 as i64);
                        builder.ins().call(
                            set_program_counter_ref,
                            &[continuation_handle, program_counter],
                        );
                        let operation = builder.ins().iconst(
                            pointer_type,
                            ((operation.effect.0 as u64) << 32 | operation.operation as u64) as i64,
                        );
                        if function_types[&function.id].pending_abi {
                            let parent = builder
                                .ins()
                                .call(pending_refs.parent, &[continuation_handle]);
                            let parent = builder.inst_results(parent)[0];
                            let call = if let Some(duration) = duration {
                                builder.ins().call(
                                    pending_refs.timer,
                                    &[continuation_handle, operation, duration, parent],
                                )
                            } else {
                                let (_, pointer, size) =
                                    argument_slot.expect("provider argument layout");
                                let size = builder.ins().iconst(pointer_type, size as i64);
                                builder.ins().call(
                                    pending_refs.provider,
                                    &[continuation_handle, operation, pointer, size, parent],
                                )
                            };
                            let status = builder.inst_results(call)[0];
                            let failed = builder.ins().icmp_imm_u(
                                cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                                status,
                                1,
                            );
                            let cancel = builder.create_block();
                            let done = builder.create_block();
                            builder.ins().brif(failed, cancel, &[], done, &[]);
                            builder.switch_to_block(cancel);
                            builder
                                .ins()
                                .call(pending_refs.fail_chain, &[continuation_handle, status]);
                            builder.ins().jump(done, &[]);
                            builder.switch_to_block(done);
                            builder.seal_block(cancel);
                            builder.seal_block(done);
                        } else if let Some(duration) = duration {
                            builder.ins().call(
                                resuspend_suspend_ref,
                                &[continuation_handle, operation, duration],
                            );
                        } else if let Some((slot, pointer, size)) = argument_slot {
                            let _ = slot;
                            let size = builder.ins().iconst(pointer_type, size as i64);
                            builder.ins().call(
                                resuspend_suspend_payload_ref,
                                &[continuation_handle, operation, pointer, size],
                            );
                        }
                        if info.return_type == Type::Unit {
                            values.insert(*destination, CompiledValue::Unit);
                        }
                        builder.ins().return_(&[]);
                        resuspended = true;
                        break;
                    }
                    MirStatement::HandlerRequest {
                        destination,
                        operation,
                        arguments,
                        ..
                    }
                    | MirStatement::ResumableRequest {
                        destination,
                        operation,
                        arguments,
                        ..
                    } => {
                        emit_handler_request(
                            &mut builder,
                            &HandlerRequestRefs {
                                function,
                                types,
                                pointer_type,
                                dup: dup_ref,
                                managed_drop: runtime_call_refs.managed_drop,
                                begin_resumption: entry_refs.handler_begin_payload,
                                begin_resumption_env: entry_refs.handler_begin_payload_env,
                                dispatch: entry_refs.handler_dispatch,
                                payload_copy: entry_refs.handler_payload_copy,
                                payload_consume: entry_refs.handler_payload_consume,
                                free_resumption: entry_refs.handler_free_resumption,
                            },
                            *destination,
                            *operation,
                            arguments,
                            &mut values,
                            &mut environment,
                        )?;
                    }
                    MirStatement::TaskAbort {
                        destination,
                        operation,
                        arguments,
                    } => {
                        let drop_payload_ref = failure_drop_refs.get(operation).copied();
                        let encoded_operation =
                            ((operation.effect.0 as u64) << 32) | operation.operation as u64;
                        let encoded_operation = builder
                            .ins()
                            .iconst(cranelift_codegen::ir::types::I64, encoded_operation as i64);
                        if arguments.is_empty() {
                            builder.ins().call(entry_refs.abort, &[encoded_operation]);
                        } else {
                            let mut flattened = Vec::new();
                            for argument in arguments {
                                let value = values.get(argument).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "resume abort argument was not compiled"
                                            .to_owned(),
                                    }
                                })?;
                                flattened.extend(value_arguments(value));
                            }
                            let (slot, pointer) =
                                create_task_slot(&mut builder, pointer_type, flattened.len())?;
                            for (index, value) in flattened.iter().enumerate() {
                                builder.ins().stack_store(
                                    pointer_type,
                                    *value,
                                    slot,
                                    (index * 8) as i32,
                                );
                            }
                            let size = builder
                                .ins()
                                .iconst(pointer_type, (flattened.len() * 8) as i64);
                            let drop_payload = drop_payload_ref
                                .map(|drop| builder.ins().func_addr(pointer_type, drop))
                                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
                            builder.ins().call(
                                entry_refs.abort_payload,
                                &[encoded_operation, pointer, size, drop_payload],
                            );
                        }
                        let value = crate::codegen::constructors::zero_compiled_value(
                            &mut builder,
                            function.value_types[destination.0],
                            pointer_type,
                            types,
                        )
                        .map_err(|error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        })?;
                        values.insert(*destination, value);
                    }
                    MirStatement::RuntimeCall {
                        destination,
                        intrinsic,
                        arguments,
                    } => {
                        compile_runtime_call(
                            &mut builder,
                            &mut RuntimeCallContext {
                                types,
                                pointer_type,
                                refs: runtime_call_refs,
                            },
                            destination,
                            intrinsic,
                            arguments,
                            &mut values,
                            &mut environment,
                        )?;
                    }
                }
                if let Some(destination) = statement.destination() {
                    if let Some(variables) = restored_variables.get(&destination) {
                        if let Some(value) = values.get(&destination) {
                            for (&variable, component) in
                                variables.iter().zip(value_arguments(value.clone()))
                            {
                                builder.def_var(variable, component);
                            }
                        }
                    }
                }
            }
            if resuspended {
                if block.scoped {
                    environment.pop_scope();
                }
                continue;
            }
            let terminator =
                block
                    .terminator
                    .as_ref()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "continuation machine block has no terminator".to_owned(),
                    })?;
            compile_continuation_terminator(
                &mut builder,
                terminator,
                &values,
                &mut environments,
                &blocks,
                &environment,
                function,
                continuation,
                continuation_handle,
                frame,
                complete_ref,
                result_pointer_ref,
                main_result_ref,
                pointer_type,
                types,
            )?;
            if block.scoped {
                environment.pop_scope();
            }
        }
        builder.seal_block(entry);
        for block_id in continuation_machine_blocks(function, continuation.resume_block) {
            builder.seal_block(blocks[block_id.0].expect("created continuation CFG block"));
        }
        builder.seal_all_blocks();
        builder.finalize(self.module.target_config());
        self.enqueue_machine_function(entry_id, context, function, Some(continuation.id.0));
        Ok(())
    }
}
