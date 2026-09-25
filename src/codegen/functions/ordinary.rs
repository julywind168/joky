//! Ordinary MIR function compilation for the Cranelift backend.

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlagsData, Signature};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Linkage;

use super::super::abi::{
    abi_types, append_result_params, result_arguments, value_arguments, value_from_params,
};
use super::super::constructors::zero_compiled_value;
use super::super::cranelift::{CraneliftBackend, ModuleLifecycle};
use super::super::environment::{CompiledValue, Environment, UserFunctionRef};
use super::super::helpers::codegen_error;
use super::context::CompileContext;
use super::continuation::{block_is_reachable, function_uses_root_task_scope};
use super::ordinary_continuations::initialize_continuations;
use super::runtime_ids::RuntimeCallInputs;
use super::statements::{
    compile_mir_statement, declare_task_runtime_refs, RuntimeRefs, StatementContext,
    TaskCodegenContext,
};
use super::values::*;
use crate::diagnostic::CodegenError;
use crate::mir::{MirFunction, MirStatement, MirTerminator};

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    /// Compile a CFG MIR function with MIR-owned expressions and control flow.
    pub(in crate::codegen) fn compile_mir_function(
        &mut self,
        function: &MirFunction,
        context: CompileContext<'_>,
    ) -> Result<(), CodegenError> {
        let CompileContext {
            types,
            func_ids,
            closure_call_ids,
            closure_drop_ids,
            callback_trampoline_ids,
            continuation_entry_ids,
            println_id,
            print_id,
            panic_id,
            continuation_resuspend_suspend_id,
            continuation_resuspend_suspend_payload_id,
            continuation_new_id,
            continuation_complete_function_pending_id,
            continuation_start_pending_timer_id,
            continuation_start_pending_provider_id,
            continuation_register_cleanup_id,
            continuation_register_cleanup_region_id,
            continuation_alloc_frame_id,
            continuation_alloc_result_id,
            continuation_alloc_suspend_result_id,
            continuation_release_suspend_result_id,
            continuation_clear_suspend_cleanups_id,
            continuation_alloc_spill_id,
            continuation_free_id,
            continuation_set_resume_entry_id,
            continuation_set_program_counter_id,
            continuation_set_root_group_id,
            continuation_root_group_id,
            continuation_resume_at_id,
            continuation_complete_id,
            continuation_complete_suspend_id,
            continuation_is_cancelled_id,
            continuation_frame_pointer_id,
            continuation_spill_pointer_id,
            continuation_result_pointer_id,
            continuation_suspend_result_pointer_id,
            allocate_id,
            closure_allocate_id,
            dup_id,
            drop_id,
            string_from_id,
            bytes_from_data_id,
            string_len_id,
            string_c_string_check_id,
            string_from_cstr_id,
            c_alloc_id,
            c_free_id,
            show_i64_id,
            debug_path_id,
            debug_path_status_id,
            debug_native_id_id,
            debug_string_id,
            debug_bytes_id,
            debug_duration_id,
            echo_id,
            show_u64_id,
            show_f64_id,
            show_bool_id,
            string_concat_id,
            string_eq_id,
            string_compare_id,
            string_starts_with_id,
            string_ends_with_id,
            string_contains_id,
            string_scalar_count_id,
            string_grapheme_count_id,
            string_is_ascii_id,
            string_trim_id,
            string_to_upper_id,
            string_to_lower_id,
            string_split_id,
            path_call_id,
            string_replace_id,
            string_get_byte_id,
            string_slice_id,
            class_drop_ids,
            continuation_cleanup_drop_ids,
            pointer_type,
            string_values,
            function_types,
            task_thunk_ids,
            handler_thunk_ids,
            task_result_drop_ids,
            task_failure_drop_ids,
            task_runtime_ids,
            function_id,
            is_main,
            machine_entry_literals,
        } = context;
        let function_type =
            function_types
                .get(&function_id)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: format!("function id {} has no signature", function_id.0),
                })?;
        let func_id = *func_ids
            .get(&function_id)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: format!(
                    "function id {} has no codegen implementation",
                    function_id.0
                ),
            })?;
        let uses_root_task_scope = function_uses_root_task_scope(function);
        // Resolve registry keys before borrowing FunctionBuilderContext.
        // AOT keys are preassigned; JIT keys are allocated process-wide.
        let continuation_entry_keys = function
            .continuations
            .iter()
            .map(|continuation| {
                (
                    continuation.id,
                    self.continuation_entry_key(function.id, continuation.id),
                )
            })
            .collect::<HashMap<_, _>>();
        let runtime_call_ids = self.declare_runtime_call_ids(RuntimeCallInputs {
            pointer_type,
            print_id,
            println_id,
            panic_id,
            allocate_id,
            closure_allocate_id,
            dup_id,
            drop_id,
            string_from_id,
            bytes_from_data_id,
            string_len_id,
            string_c_string_check_id,
            string_from_cstr_id,
            c_alloc_id,
            c_free_id,
            show_i64_id,
            debug_path_id,
            debug_path_status_id,
            debug_native_id_id,
            debug_string_id,
            debug_bytes_id,
            debug_duration_id,
            echo_id,
            show_u64_id,
            show_f64_id,
            show_bool_id,
            string_concat_id,
            string_eq_id,
            string_compare_id,
            string_starts_with_id,
            string_ends_with_id,
            string_contains_id,
            string_scalar_count_id,
            string_grapheme_count_id,
            string_is_ascii_id,
            string_trim_id,
            string_to_upper_id,
            string_to_lower_id,
            string_split_id,
            path_call_id,
            string_replace_id,
            string_get_byte_id,
            string_slice_id,
        })?;
        let mut context = self.module.make_context();
        context.func.signature = self.build_signature(
            function_type,
            is_main,
            types,
            pointer_type,
            self.module.isa().default_call_conv(),
        );
        let string_values = std::iter::from_fn(|| string_values.next())
            .map(|(value, length)| match value {
                crate::codegen::functions::context::StringValue::Pointer(pointer) => (
                    self.module
                        .materialize_string(&mut context.func, pointer, length),
                    length,
                ),
                value => (value, length),
            })
            .collect::<Vec<_>>();
        let mut string_values = string_values.into_iter();

        {
            let continuation_new_ref = self
                .module
                .declare_func_in_func(continuation_new_id, &mut context.func);
            let continuation_register_cleanup_ref = self
                .module
                .declare_func_in_func(continuation_register_cleanup_id, &mut context.func);
            let continuation_register_cleanup_region_ref = self
                .module
                .declare_func_in_func(continuation_register_cleanup_region_id, &mut context.func);
            let continuation_alloc_frame_ref = self
                .module
                .declare_func_in_func(continuation_alloc_frame_id, &mut context.func);
            let continuation_alloc_result_ref = self
                .module
                .declare_func_in_func(continuation_alloc_result_id, &mut context.func);
            let continuation_alloc_suspend_result_ref = self
                .module
                .declare_func_in_func(continuation_alloc_suspend_result_id, &mut context.func);
            let continuation_release_suspend_result_ref = self
                .module
                .declare_func_in_func(continuation_release_suspend_result_id, &mut context.func);
            let continuation_clear_suspend_cleanups_ref = self
                .module
                .declare_func_in_func(continuation_clear_suspend_cleanups_id, &mut context.func);
            let continuation_alloc_spill_ref = self
                .module
                .declare_func_in_func(continuation_alloc_spill_id, &mut context.func);
            let continuation_free_ref = self
                .module
                .declare_func_in_func(continuation_free_id, &mut context.func);
            let continuation_set_resume_entry_ref = self
                .module
                .declare_func_in_func(continuation_set_resume_entry_id, &mut context.func);
            let continuation_set_program_counter_ref = self
                .module
                .declare_func_in_func(continuation_set_program_counter_id, &mut context.func);
            let continuation_set_root_group_ref = self
                .module
                .declare_func_in_func(continuation_set_root_group_id, &mut context.func);
            let continuation_resume_at_ref = self
                .module
                .declare_func_in_func(continuation_resume_at_id, &mut context.func);
            let continuation_complete_ref = self
                .module
                .declare_func_in_func(continuation_complete_id, &mut context.func);
            let continuation_complete_function_pending_ref = self
                .module
                .declare_func_in_func(continuation_complete_function_pending_id, &mut context.func);
            let continuation_start_pending_timer_ref = self
                .module
                .declare_func_in_func(continuation_start_pending_timer_id, &mut context.func);
            let continuation_start_pending_provider_ref = self
                .module
                .declare_func_in_func(continuation_start_pending_provider_id, &mut context.func);
            let task_group_cancel_free_ref = self
                .module
                .declare_func_in_func(task_runtime_ids.group_cancel_free, &mut context.func);
            let continuation_complete_suspend_ref = self
                .module
                .declare_func_in_func(continuation_complete_suspend_id, &mut context.func);
            let continuation_suspend_result_pointer_ref = self
                .module
                .declare_func_in_func(continuation_suspend_result_pointer_id, &mut context.func);
            let runtime_call_refs = super::runtime_ids::declare_runtime_call_refs(
                &mut self.module,
                &mut context.func,
                runtime_call_ids,
            );
            let managed_drop_ref = runtime_call_refs.managed_drop;
            let main_call_conv = self.module.isa().default_call_conv();
            let main_result_id = self
                .module
                .declare_function(
                    joky_runtime_abi::symbols::MAIN_RESULT_SYMBOL,
                    Linkage::Import,
                    &Signature {
                        params: vec![
                            AbiParam::new(types::I32),
                            AbiParam::new(pointer_type),
                            AbiParam::new(pointer_type),
                        ],
                        returns: Vec::new(),
                        call_conv: main_call_conv,
                    },
                )
                .map_err(codegen_error)?;
            let main_result_ref = self
                .module
                .declare_func_in_func(main_result_id, &mut context.func);
            let allocate_ref = runtime_call_refs.allocate;
            let closure_allocate_ref = runtime_call_refs.closure_allocate;
            let class_drop_refs = class_drop_ids
                .iter()
                .map(|(id, function)| {
                    (
                        *id,
                        self.module
                            .declare_func_in_func(*function, &mut context.func),
                    )
                })
                .collect::<HashMap<_, _>>();
            let continuation_cleanup_drop_refs = continuation_cleanup_drop_ids
                .iter()
                .map(|(ty, drop)| {
                    (
                        *ty,
                        self.module.declare_func_in_func(*drop, &mut context.func),
                    )
                })
                .collect::<HashMap<_, _>>();
            let mut func_refs = HashMap::new();
            for (id, func_id) in func_ids {
                let reference = self
                    .module
                    .declare_func_in_func(*func_id, &mut context.func);
                func_refs.insert(*id, UserFunctionRef { reference });
            }
            let closure_call_refs = closure_call_ids
                .iter()
                .map(|(id, func)| {
                    (
                        *id,
                        self.module.declare_func_in_func(*func, &mut context.func),
                    )
                })
                .collect();
            let callback_trampoline_refs = callback_trampoline_ids
                .iter()
                .map(|(key, trampoline)| {
                    (
                        *key,
                        self.module
                            .declare_func_in_func(*trampoline, &mut context.func),
                    )
                })
                .collect();
            let closure_drop_refs = closure_drop_ids
                .iter()
                .map(|(id, func)| {
                    (
                        *id,
                        self.module.declare_func_in_func(*func, &mut context.func),
                    )
                })
                .collect();
            let task_thunk_refs = task_thunk_ids
                .iter()
                .map(|(id, thunk)| {
                    (
                        *id,
                        self.module.declare_func_in_func(*thunk, &mut context.func),
                    )
                })
                .collect();
            let handler_thunk_refs = handler_thunk_ids
                .iter()
                .map(|(id, thunk)| {
                    (
                        *id,
                        self.module.declare_func_in_func(*thunk, &mut context.func),
                    )
                })
                .collect();
            let task_result_drop_refs = task_result_drop_ids
                .iter()
                .map(|(id, drop)| {
                    (
                        *id,
                        self.module.declare_func_in_func(*drop, &mut context.func),
                    )
                })
                .collect();
            let task_failure_drop_refs = task_failure_drop_ids
                .iter()
                .map(|(operation, drop)| {
                    (
                        *operation,
                        self.module.declare_func_in_func(*drop, &mut context.func),
                    )
                })
                .collect();
            let task_runtime_refs =
                declare_task_runtime_refs(&mut self.module, &mut context.func, task_runtime_ids);

            let pending_refs =
                super::pending::declare_pending_refs(&mut self.module, &mut context.func)?;
            let map_key_refs = self.map_key_refs(&mut context.func);
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            builder.set_srcloc(crate::codegen::cranelift::source_location(
                function.source.body,
            ));
            let blocks = function
                .blocks
                .iter()
                .map(|_| builder.create_block())
                .collect::<Vec<_>>();
            let entry = blocks[function.entry.0];
            builder.append_block_params_for_function_params(entry);

            let mut phi_values = vec![HashMap::new(); function.blocks.len()];
            for (block_index, block) in function.blocks.iter().enumerate() {
                for statement in &block.statements {
                    let MirStatement::Phi { destination, .. } = statement else {
                        break;
                    };
                    let ty = function.value_types[destination.0];
                    let params = append_result_params(
                        &mut builder,
                        blocks[block_index],
                        ty,
                        pointer_type,
                        types,
                    );
                    let value = value_from_params(&params, ty, types).map_err(|error| {
                        CodegenError::RuntimeError {
                            message: error.to_string(),
                        }
                    })?;
                    phi_values[block_index].insert(*destination, value);
                }
            }

            builder.switch_to_block(entry);
            let continuation_allocation_refs =
                super::ordinary_continuations::ContinuationAllocationRefs {
                    new: continuation_new_ref,
                    set_resume_entry: continuation_set_resume_entry_ref,
                    alloc_frame: continuation_alloc_frame_ref,
                    alloc_result: continuation_alloc_result_ref,
                    alloc_spill: continuation_alloc_spill_ref,
                    register_cleanup: continuation_register_cleanup_ref,
                    register_cleanup_region: continuation_register_cleanup_region_ref,
                    group_cancel_free: task_group_cancel_free_ref,
                    managed_drop: managed_drop_ref,
                };
            let continuation_resources = initialize_continuations(
                &mut builder,
                function,
                &continuation_entry_keys,
                continuation_allocation_refs,
                &class_drop_refs,
                &continuation_cleanup_drop_refs,
                types,
                pointer_type,
                |metadata| !metadata.is_function_call(),
            )?;
            let mut call_handle_slots = HashMap::new();
            for metadata in function.continuations.iter().filter(|metadata| {
                metadata.callee.is_some()
                    || (metadata.callee.is_none() && metadata.operation.is_none())
            }) {
                let (slot, address) =
                    super::statements::create_task_slot(&mut builder, pointer_type, 1)?;
                let zero = builder.ins().iconst(pointer_type, 0);
                builder.ins().store(MemFlagsData::new(), zero, address, 0);
                call_handle_slots.insert(metadata.id, slot);
            }
            let continuation_handles = continuation_resources.handles;
            let continuation_spills = continuation_resources.spills;
            let continuation_frames = continuation_resources.frames;
            let continuation_results = continuation_resources.results;
            let mut base_environment = Environment::new();
            base_environment.function_refs = func_refs;
            base_environment.map_key_refs = map_key_refs;
            base_environment.closure_call_refs = closure_call_refs;
            base_environment.closure_drop_refs = closure_drop_refs;
            base_environment.callback_trampoline_refs = callback_trampoline_refs;
            base_environment.function_types = function_types.clone();
            base_environment.class_drop_refs = class_drop_refs;
            base_environment.managed_drop_ref = Some(managed_drop_ref);
            base_environment.allocate_ref = Some(allocate_ref);
            base_environment.closure_allocate_ref = Some(closure_allocate_ref);

            let block_params = builder.block_params(entry).to_vec();
            let mut parameter_offset = 0;
            if let Some(receiver_type) = function_type.receiver {
                let count = abi_types(receiver_type, pointer_type, types).len();
                let receiver = value_from_params(
                    &block_params[parameter_offset..parameter_offset + count],
                    receiver_type,
                    types,
                )
                .map_err(|error| CodegenError::RuntimeError {
                    message: error.to_string(),
                })?;
                if let Some(local) = function.receiver_local {
                    base_environment.bind_local(local, receiver);
                }
                parameter_offset += count;
            }
            for (parameter, ty) in function.parameters.iter().zip(&function_type.parameters) {
                let count = abi_types(*ty, pointer_type, types).len();
                let value = value_from_params(
                    &block_params[parameter_offset..parameter_offset + count],
                    *ty,
                    types,
                )
                .map_err(|error| CodegenError::RuntimeError {
                    message: error.to_string(),
                })?;
                base_environment.bind_local(parameter.local, value);
                parameter_offset += count;
            }

            let mut environments = vec![None; function.blocks.len()];
            environments[function.entry.0] = Some(base_environment.clone());
            let mut values = HashMap::new();
            let mut statement_context = StatementContext {
                function,
                types,
                pointer_type,
                string_values: &mut string_values,
                refs: RuntimeRefs {
                    calls: runtime_call_refs,
                    continuation_alloc_suspend_result: continuation_alloc_suspend_result_ref,
                    continuation_release_suspend_result: continuation_release_suspend_result_ref,
                    continuation_clear_suspend_cleanups: continuation_clear_suspend_cleanups_ref,
                    continuation_register_cleanup: continuation_register_cleanup_ref,
                    continuation_register_cleanup_region: continuation_register_cleanup_region_ref,
                    continuation_complete: continuation_complete_ref,
                    continuation_complete_function_pending:
                        continuation_complete_function_pending_ref,
                    continuation_start_pending_timer: continuation_start_pending_timer_ref,
                    continuation_start_pending_provider: continuation_start_pending_provider_ref,
                    continuation_set_program_counter: continuation_set_program_counter_ref,
                    continuation_resume_at: continuation_resume_at_ref,
                    continuation_complete_suspend: continuation_complete_suspend_ref,
                    continuation_suspend_result_pointer: continuation_suspend_result_pointer_ref,
                },
                tasks: TaskCodegenContext {
                    refs: task_runtime_refs,
                    thunks: task_thunk_refs,
                    handler_thunks: handler_thunk_refs,
                    result_drops: task_result_drop_refs,
                    failure_drops: task_failure_drop_refs,
                    function_types,
                    groups: HashMap::new(),
                    tasks: HashMap::new(),
                    handler_frames: Vec::new(),
                },
                continuation_handles: &continuation_handles,
                continuation_spills: &continuation_spills,
                continuation_frames: &continuation_frames,
                continuation_cleanup_drop_refs: &continuation_cleanup_drop_refs,
                suspend_pending: None,
                pending_abi: function_type.pending_abi,
                pending_refs,
                continuation_allocation_refs,
                continuation_entry_keys: &continuation_entry_keys,
                call_handle_slots: &call_handle_slots,
                pending_parent: if function_type.pending_abi && !is_main {
                    block_params.get(parameter_offset).copied()
                } else {
                    None
                },
            };
            // Compile blocks in reachable CFG order so a join block receives
            // the environment established by its predecessor. Numeric MIR
            // order is not necessarily topological (a handler join is often
            // allocated before the resume block that feeds it).
            let mut block_order = Vec::with_capacity(function.blocks.len());
            let mut pending_blocks = vec![function.entry.0];
            let mut seen_blocks = HashSet::new();
            while let Some(block_index) = pending_blocks.pop() {
                if !seen_blocks.insert(block_index) {
                    continue;
                }
                block_order.push(block_index);
                match function.blocks[block_index].terminator.as_ref() {
                    Some(MirTerminator::Goto { target, .. }) => pending_blocks.push(target.0),
                    Some(MirTerminator::Branch {
                        then_block,
                        else_block,
                        ..
                    }) => {
                        pending_blocks.push(else_block.0);
                        pending_blocks.push(then_block.0);
                    }
                    Some(MirTerminator::Return(_) | MirTerminator::Unreachable) | None => {}
                }
            }
            block_order.extend(
                (0..function.blocks.len()).filter(|block_index| !seen_blocks.contains(block_index)),
            );
            for block_index in block_order {
                let block = &function.blocks[block_index];
                let mut environment = environments[block_index]
                    .take()
                    .unwrap_or_else(|| base_environment.clone());
                if block.scoped {
                    environment.push_scope();
                }
                if block_index != function.entry.0 {
                    builder.switch_to_block(blocks[block_index]);
                }
                if block_index == function.entry.0 && uses_root_task_scope {
                    let inherited_region = builder.ins().iconst(pointer_type, 0);
                    let call = builder
                        .ins()
                        .call(statement_context.tasks.refs.group_new, &[inherited_region]);
                    let group = builder.inst_results(call)[0];
                    environment.bind_task_group(crate::mir::MirScopeId(0), group);
                    statement_context
                        .tasks
                        .groups
                        .insert(crate::mir::MirScopeId(0), group);
                    for handle in continuation_handles.values() {
                        builder
                            .ins()
                            .call(continuation_set_root_group_ref, &[*handle, group]);
                    }
                }
                values.extend(phi_values[block_index].clone());
                for statement in &block.statements {
                    builder.set_srcloc(crate::codegen::cranelift::source_location(
                        function.statement_span(statement),
                    ));
                    compile_mir_statement(
                        &mut builder,
                        &mut statement_context,
                        statement,
                        &mut values,
                        &mut environment,
                        &mut base_environment,
                    )?;
                }
                let terminator =
                    block
                        .terminator
                        .as_ref()
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR block has no terminator".to_owned(),
                        })?;
                if let Some(pending) = statement_context.suspend_pending.take() {
                    let MirTerminator::Goto { target, arguments } = terminator else {
                        return Err(CodegenError::RuntimeError {
                            message: "MIR Suspend must transfer to its resume block".to_owned(),
                        });
                    };
                    let arguments = arguments
                        .iter()
                        .map(|value| {
                            values
                                .get(value)
                                .cloned()
                                .ok_or_else(|| CodegenError::RuntimeError {
                                    message: "MIR jump value was not compiled".to_owned(),
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .flat_map(value_arguments)
                        .map(Into::into)
                        .collect::<Vec<_>>();
                    if environments[target.0].is_none() {
                        environments[target.0] = Some(environment.clone());
                    }
                    let pending_return = builder.create_block();
                    builder
                        .ins()
                        .brif(pending, pending_return, &[], blocks[target.0], &arguments);
                    builder.switch_to_block(pending_return);
                    let failed = builder.create_block();
                    let handoff = builder.create_block();
                    let terminal = builder.ins().icmp_imm_u(
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
                        pending,
                        1,
                    );
                    builder.ins().brif(terminal, failed, &[], handoff, &[]);
                    builder.switch_to_block(failed);
                    super::statements::free_native_reservations(&mut builder, &statement_context);
                    super::pending::return_status(&mut builder, pending);
                    builder.switch_to_block(handoff);
                    builder.seal_block(failed);
                    builder.seal_block(handoff);
                    // The resume entry owns only the active activation and
                    // allocates fresh state for later calls. Unused native
                    // reservations must not survive this native return.
                    let active =
                        block
                            .statements
                            .iter()
                            .rev()
                            .find_map(|statement| match statement {
                                MirStatement::Suspend { continuation, .. }
                                | MirStatement::TaskWait { continuation, .. }
                                | MirStatement::CownAcquire { continuation, .. } => {
                                    Some(*continuation)
                                }
                                _ => None,
                            });
                    for (id, handle) in &continuation_handles {
                        if Some(*id) != active {
                            builder.ins().call(continuation_free_ref, &[*handle]);
                        }
                    }
                    let pending_value = zero_compiled_value(
                        &mut builder,
                        function_type.return_type,
                        pointer_type,
                        types,
                    )
                    .map_err(|error| CodegenError::RuntimeError {
                        message: error.to_string(),
                    })?;
                    let mut pending_values = if is_main {
                        Vec::new()
                    } else {
                        value_arguments(pending_value)
                    };
                    if function_type.pending_abi {
                        pending_values.insert(0, pending);
                    }
                    builder.ins().return_(&pending_values);
                    builder.seal_block(pending_return);
                    if block.scoped {
                        environment.pop_scope();
                    }
                    continue;
                }
                match terminator {
                    MirTerminator::Goto { target, arguments } => {
                        let arguments = arguments
                            .iter()
                            .map(|value| {
                                values.get(value).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "MIR jump value was not compiled".to_owned(),
                                    }
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?
                            .into_iter()
                            .flat_map(value_arguments)
                            .map(Into::into)
                            .collect::<Vec<_>>();
                        if environments[target.0].is_none() {
                            environments[target.0] = Some(environment.clone());
                        }
                        builder.ins().jump(blocks[target.0], &arguments);
                    }
                    MirTerminator::Branch {
                        condition,
                        then_block,
                        else_block,
                    } => {
                        let condition = values.get(condition).cloned().ok_or_else(|| {
                            CodegenError::RuntimeError {
                                message: "MIR branch condition was not compiled".to_owned(),
                            }
                        })?;
                        let condition = expect_boolean(condition)?;
                        if environments[then_block.0].is_none() {
                            environments[then_block.0] = Some(environment.clone());
                        }
                        if environments[else_block.0].is_none() {
                            environments[else_block.0] = Some(environment.clone());
                        }
                        builder.ins().brif(
                            condition,
                            blocks[then_block.0],
                            &[],
                            blocks[else_block.0],
                            &[],
                        );
                    }
                    MirTerminator::Return(value) => {
                        if let Some(value_id) = value.filter(|_| !function_type.pending_abi) {
                            let result_pointer = function
                                .continuations
                                .iter()
                                .find(|continuation| {
                                    block_is_reachable(
                                        &function.blocks,
                                        continuation.resume_block,
                                        block.id,
                                    )
                                })
                                .and_then(|continuation| {
                                    continuation_results.get(&continuation.id)
                                });
                            if let Some(result_pointer) = result_pointer {
                                let value = values.get(&value_id).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "MIR continuation result value was not compiled"
                                            .to_owned(),
                                    }
                                })?;
                                let components = value_arguments(value);
                                let expected = abi_types(function.return_type, pointer_type, types);
                                if components.len() != expected.len() {
                                    return Err(CodegenError::RuntimeError {
                                        message: "MIR continuation result ABI shape mismatch"
                                            .to_owned(),
                                    });
                                }
                                let mut offset = 0;
                                for component in components {
                                    builder.ins().store(
                                        MemFlagsData::new(),
                                        component,
                                        *result_pointer,
                                        offset,
                                    );
                                    offset += 8;
                                }
                            }
                        }
                        for handle in continuation_handles.values() {
                            builder.ins().call(continuation_free_ref, &[*handle]);
                        }
                        let value = value
                            .map(|value| {
                                values.get(&value).cloned().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "MIR return value was not compiled".to_owned(),
                                    }
                                })
                            })
                            .transpose()?;
                        let return_value = value;
                        if is_main
                            && matches!(function_type.return_type, crate::sema::Type::Result(_))
                        {
                            emit_main_result(
                                &mut builder,
                                return_value.as_ref().ok_or_else(|| {
                                    CodegenError::RuntimeError {
                                        message: "main Result return value is missing".to_owned(),
                                    }
                                })?,
                                main_result_ref,
                            )?;
                        }
                        let mut return_values = if is_main {
                            Vec::new()
                        } else {
                            result_arguments(
                                return_value.unwrap_or(CompiledValue::Unit),
                                function_type.return_type,
                            )
                            .map_err(|error| {
                                CodegenError::RuntimeError {
                                    message: error.to_string(),
                                }
                            })?
                        };
                        if function_type.pending_abi {
                            let ready = builder.ins().iconst(cranelift_codegen::ir::types::I8, 0);
                            return_values.insert(0, ready);
                        }
                        builder.ins().return_(&return_values);
                    }
                    MirTerminator::Unreachable => {
                        builder
                            .ins()
                            .trap(cranelift_codegen::ir::TrapCode::unwrap_user(1));
                    }
                }
                if block.scoped {
                    environment.pop_scope();
                }
            }
            for block in blocks {
                builder.seal_block(block);
            }
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.enqueue_machine_function(func_id, context, function, None);
        for continuation in &function.continuations {
            let Some(entry_id) = continuation_entry_ids.get(&(function.id, continuation.id)) else {
                continue;
            };
            self.compile_continuation_machine_entry(
                function,
                continuation,
                *entry_id,
                func_ids,
                function_types,
                closure_call_ids,
                closure_drop_ids,
                callback_trampoline_ids,
                continuation_complete_id,
                continuation_is_cancelled_id,
                continuation_frame_pointer_id,
                continuation_spill_pointer_id,
                continuation_result_pointer_id,
                continuation_suspend_result_pointer_id,
                continuation_alloc_suspend_result_id,
                continuation_release_suspend_result_id,
                continuation_clear_suspend_cleanups_id,
                continuation_register_cleanup_id,
                continuation_register_cleanup_region_id,
                continuation_resuspend_suspend_id,
                continuation_resuspend_suspend_payload_id,
                continuation_set_resume_entry_id,
                continuation_set_program_counter_id,
                continuation_root_group_id,
                task_runtime_ids.group_new,
                task_runtime_ids.spawn_heap,
                task_runtime_ids.group_free,
                task_runtime_ids.failure_operation,
                task_runtime_ids.failure_claim,
                task_runtime_ids.failure_rethrow,
                task_runtime_ids.join,
                task_runtime_ids.claim_result,
                task_runtime_ids.cancel,
                task_runtime_ids.result_pointer,
                task_runtime_ids.race,
                task_runtime_ids.failure_payload,
                task_runtime_ids.abort,
                task_runtime_ids.abort_payload,
                task_failure_drop_ids,
                task_runtime_ids.handler_frame_begin_resumption_with_payload,
                task_runtime_ids.handler_frame_begin_resumption_with_payload_env,
                task_runtime_ids.handler_frame_dispatch_resumption,
                task_runtime_ids.handler_frame_resumption_payload_copy,
                task_runtime_ids.handler_frame_resumption_payload_consume,
                task_runtime_ids.handler_frame_free_resumption,
                handler_thunk_ids,
                task_runtime_ids,
                task_thunk_ids,
                task_result_drop_ids,
                &continuation_entry_keys,
                runtime_call_ids,
                class_drop_ids,
                continuation_cleanup_drop_ids,
                types,
                pointer_type,
                self.module.isa().default_call_conv(),
                machine_entry_literals,
            )?;
        }
        Ok(())
    }
}
