use super::runtime_ids::{
    declare_basic_runtime_ids, declare_continuation_runtime_ids, declare_task_runtime_ids,
    BasicRuntimeIds, ContinuationRuntimeIds,
};
use super::*;
use crate::codegen::functions::StringValue;
use crate::mir::{MirFunctionId, MirValueId};
use cranelift_module::FuncId;

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(crate) fn compile_and_run_program(
        &mut self,
        mir: &MirProgram,
        runtime_scope: &joky_runtime::host::RuntimeScope,
    ) -> Result<(), CodegenError> {
        self.compile_program_inner(mir, Some(runtime_scope))
    }

    /// Lower a complete MIR program into the selected Cranelift module.
    /// Passing no runtime scope performs compilation only, which is used by
    /// the object backend before it serializes the module.
    pub(crate) fn compile_program(&mut self, mir: &MirProgram) -> Result<(), CodegenError> {
        self.compile_program_inner(mir, None)
    }

    fn compile_program_inner(
        &mut self,
        mir: &MirProgram,
        runtime_scope: Option<&joky_runtime::host::RuntimeScope>,
    ) -> Result<(), CodegenError> {
        let jit_compile_guard = if M::IS_AOT {
            None
        } else {
            Some(
                JIT_COMPILE_LOCK
                    .get_or_init(|| Mutex::new(()))
                    .lock()
                    .expect("JIT compile mutex"),
            )
        };
        let types = mir.types();
        let functions = mir.functions();
        if M::IS_AOT {
            // Keys belong to this linked program, never to compiler-process
            // history. Reserve all continuation IDs, including non-machine ones.
            let mut continuations = functions
                .iter()
                .flat_map(|function| {
                    function
                        .continuations
                        .iter()
                        .map(move |continuation| (function.id, continuation.id))
                })
                .collect::<Vec<_>>();
            continuations
                .sort_unstable_by_key(|(function, continuation)| (function.0, continuation.0));
            for (index, identity) in continuations.into_iter().enumerate() {
                if self
                    .continuation_entry_keys
                    .insert(identity, index + 1)
                    .is_some()
                {
                    return Err(CodegenError::RuntimeError {
                        message: "duplicate AOT continuation identity".into(),
                    });
                }
            }
        }
        self.string_literals.extend(
            collect_program_strings(functions)
                .into_iter()
                .map(|value| value.to_vec().into_boxed_slice()),
        );
        let pointer_type = self.module.target_config().pointer_type();
        let call_conv = self.module.isa().default_call_conv();

        let task_runtime_ids = declare_task_runtime_ids(self, pointer_type, call_conv)?;

        let ContinuationRuntimeIds {
            println_id,
            print_id,
            panic_id,
            continuation_resuspend_suspend_id,
            continuation_resuspend_suspend_payload_id,
            continuation_new_id,
            continuation_register_cleanup_id,
            continuation_register_cleanup_region_id,
            continuation_alloc_frame_id,
            continuation_alloc_result_id,
            continuation_alloc_suspend_result_id,
            continuation_release_suspend_result_id,
            continuation_clear_suspend_cleanups_id,
            continuation_frame_pointer_id,
            continuation_spill_pointer_id,
            continuation_result_pointer_id,
            continuation_suspend_result_pointer_id,
            continuation_alloc_spill_id,
            continuation_free_id,
            continuation_set_resume_entry_id,
            continuation_set_root_group_id,
            continuation_root_group_id,
            continuation_set_program_counter_id,
            continuation_resume_at_id,
            continuation_complete_id,
            continuation_complete_suspend_id,
            continuation_is_cancelled_id,
            continuation_begin_function_pending_id: _,
            continuation_complete_function_pending_id,
            continuation_start_pending_timer_id,
            continuation_start_pending_provider_id,
        } = declare_continuation_runtime_ids(self, pointer_type, call_conv)?;

        let BasicRuntimeIds {
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
        } = declare_basic_runtime_ids(self, pointer_type, call_conv)?;
        let drop_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };

        // Declare every class drop function before defining any of them so
        // mutually-referencing class layouts can call each other's glue.
        let mut class_drop_ids = HashMap::new();
        for class_id in 0..types.class_count() {
            let symbol = format!("joky_drop_class_{class_id}_{}", self.next_function);
            self.next_function += 1;
            let function = self
                .module
                .declare_function(&symbol, Linkage::Local, &drop_signature)
                .map_err(codegen_error)?;
            class_drop_ids.insert(class_id, function);
        }

        let abort_operations = functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.statements)
            .filter_map(|statement| match statement {
                MirStatement::TaskAbort { operation, .. } => Some(*operation),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut task_failure_drop_ids = HashMap::new();
        for operation in abort_operations {
            let info = types
                .effects()
                .operation_info(operation)
                .expect("verified task abort operation");
            if !info.parameters.iter().any(|ty| types.needs_drop(*ty)) {
                continue;
            }
            let symbol = format!(
                "joky_task_failure_drop_{}_{}_{}",
                operation.effect.0, operation.operation, self.next_function
            );
            self.next_function += 1;
            let drop = self
                .module
                .declare_function(&symbol, Linkage::Local, &drop_signature)
                .map_err(codegen_error)?;
            task_failure_drop_ids.insert(operation, drop);
        }
        for (operation, drop) in &task_failure_drop_ids {
            let parameter_types = &types
                .effects()
                .operation_info(*operation)
                .expect("verified task abort operation")
                .parameters;
            self.compile_task_failure_drop(
                *drop,
                parameter_types,
                &class_drop_ids,
                drop_id,
                pointer_type,
                call_conv,
                types,
            )?;
        }

        // First pass: build all function signatures from MIR
        let mut function_types = HashMap::new();
        for function in functions {
            function_types.insert(function.id, mir_function_type(function));
        }
        // Every suspending function uses the Pending call ABI: its suspend
        // points hand activations to the scheduler instead of blocking a
        // native frame. There is no synchronous fallback anymore.
        for function_type in function_types.values_mut() {
            function_type.pending_abi = function_type.is_suspending;
        }
        let closure_captures = closure_capture_types(functions)?;

        let mut func_ids = HashMap::new();
        for function in functions {
            let symbol = mir_function_symbol(function);
            let signature = self.build_signature(
                &function_types[&function.id],
                is_main_mir(function),
                types,
                pointer_type,
                call_conv,
            );
            let is_aot_main = runtime_scope.is_none() && is_main_mir(function);
            let linkage = if is_aot_main {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let func_name = if is_aot_main {
                "joky_main".to_owned()
            } else {
                format!("joky_{}_{}", symbol, self.next_function)
            };
            self.next_function += 1;

            let func_id = self
                .module
                .declare_function(&func_name, linkage, &signature)
                .map_err(codegen_error)?;

            func_ids.insert(function.id, func_id);
        }
        for class_id in 0..types.class_count() {
            let user_drop_id = if types.class_drop_effects(class_id).is_some() {
                Some(
                    *func_ids
                        .get(
                            &functions
                                .iter()
                                .find(|f| {
                                    f.receiver == Some(Type::Class(class_id))
                                        && f.name == crate::sema::DROP_METHOD
                                })
                                .ok_or_else(|| CodegenError::RuntimeError {
                                    message: format!("class {class_id} has no Drop implementation"),
                                })?
                                .id,
                        )
                        .expect("declared Drop"),
                )
            } else {
                None
            };
            super::super::classes::define_drop_glue(
                &mut self.module,
                class_id,
                class_drop_ids[&class_id],
                user_drop_id,
                &class_drop_ids,
                drop_id,
                pointer_type,
                call_conv,
                types,
            )?;
        }

        let task_functions = functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.statements)
            .filter_map(|statement| match statement {
                MirStatement::TaskCreate { function, .. } => Some(*function),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        let task_thunk_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        let mut task_thunk_ids = HashMap::new();
        for task in task_functions {
            let symbol = format!("joky_task_thunk_{}_{}", task.0, self.next_function);
            self.next_function += 1;
            let thunk = self
                .module
                .declare_function(&symbol, Linkage::Local, &task_thunk_signature)
                .map_err(codegen_error)?;
            task_thunk_ids.insert(task, thunk);
        }
        let mut handler_functions = std::collections::HashSet::new();
        let mut handler_function_arities = HashMap::new();
        for function in functions {
            for block in &function.blocks {
                for statement in &block.statements {
                    let MirStatement::HandlerEnter { handlers } = statement else {
                        continue;
                    };
                    for handler in handlers {
                        let Some(handler_function) = handler.resumable_function else {
                            continue;
                        };
                        let Some(function_type) = function_types.get(&handler_function) else {
                            continue;
                        };
                        let Some(operation_info) =
                            types.effects().operation_info(handler.operation)
                        else {
                            continue;
                        };
                        let operation_arity = operation_info.parameters.len();
                        if function_type.receiver.is_none()
                        && function_type.parameters.len() >= operation_arity
                        // Unit handlers have no result words, but still
                        // need a canonical thunk so the runtime can invoke
                        // them through the same frame ABI.
                        && (matches!(function_type.return_type, Type::Unit)
                            || !abi_types(function_type.return_type, pointer_type, types)
                                .is_empty())
                        {
                            handler_functions.insert(handler_function);
                            handler_function_arities
                                .entry(handler_function)
                                .or_insert(operation_arity);
                        }
                    }
                }
            }
        }
        let handler_thunk_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let mut handler_thunk_ids = HashMap::new();
        for handler_function in handler_functions {
            let symbol = format!(
                "joky_handler_thunk_{}_{}",
                handler_function.0, self.next_function
            );
            self.next_function += 1;
            let thunk = self
                .module
                .declare_function(&symbol, Linkage::Local, &handler_thunk_signature)
                .map_err(codegen_error)?;
            handler_thunk_ids.insert(handler_function, thunk);
        }
        let task_result_drop_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        let mut task_result_drop_ids = HashMap::new();
        for task in task_thunk_ids.keys() {
            let result = function_types[task].return_type;
            if !types.needs_drop(result) {
                continue;
            }
            let symbol = format!("joky_task_result_drop_{}_{}", task.0, self.next_function);
            self.next_function += 1;
            let drop = self
                .module
                .declare_function(&symbol, Linkage::Local, &task_result_drop_signature)
                .map_err(codegen_error)?;
            task_result_drop_ids.insert(*task, drop);
        }
        let mut continuation_cleanup_types = std::collections::HashSet::new();
        for function in functions {
            // Both resumable continuations and Pending-capable ABIs own a
            // result buffer, including implementations that always return Ready.
            if function.is_suspending || !function.continuations.is_empty() {
                collect_tagged_continuation_cleanup_types(
                    function.return_type,
                    types,
                    &mut continuation_cleanup_types,
                );
            }
            for continuation in &function.continuations {
                if let Some(operation_id) = continuation.operation {
                    if let Some(operation) = types.effects().operation_info(operation_id) {
                        collect_tagged_continuation_cleanup_types(
                            operation.return_type,
                            types,
                            &mut continuation_cleanup_types,
                        );
                    }
                }
                for slot in &continuation.frame_slots {
                    collect_tagged_continuation_cleanup_types(
                        slot.ty,
                        types,
                        &mut continuation_cleanup_types,
                    );
                }
                for spill in &continuation.spill_slots {
                    collect_tagged_continuation_cleanup_types(
                        function.value_types[spill.value.0],
                        types,
                        &mut continuation_cleanup_types,
                    );
                }
            }
        }
        let mut continuation_cleanup_drop_ids = HashMap::new();
        for ty in continuation_cleanup_types {
            let symbol = format!("joky_continuation_drop_{}", self.next_function);
            self.next_function += 1;
            let drop = self
                .module
                .declare_function(&symbol, Linkage::Local, &task_result_drop_signature)
                .map_err(codegen_error)?;
            continuation_cleanup_drop_ids.insert(ty, drop);
        }
        let mut closure_call_ids = HashMap::new();
        let mut closure_drop_ids = HashMap::new();
        for closure in closure_captures.keys() {
            let call_symbol = format!("joky_closure_call_{}_{}", closure.0, self.next_function);
            self.next_function += 1;
            let target_type = &function_types[closure];
            let capture_count = closure_captures[closure].len();
            let mut params = std::iter::once(AbiParam::new(pointer_type))
                .chain(
                    target_type.parameters[capture_count..]
                        .iter()
                        .flat_map(|ty| abi_types(*ty, pointer_type, types))
                        .map(AbiParam::new),
                )
                .collect::<Vec<_>>();
            if target_type.pending_abi {
                params.push(AbiParam::new(pointer_type));
            }
            let mut returns = abi_types(target_type.return_type, pointer_type, types)
                .into_iter()
                .map(AbiParam::new)
                .collect::<Vec<_>>();
            if target_type.pending_abi {
                returns.insert(0, AbiParam::new(cranelift_codegen::ir::types::I8));
            }
            let signature = Signature {
                params,
                returns,
                call_conv,
            };
            let call_id = self
                .module
                .declare_function(&call_symbol, Linkage::Local, &signature)
                .map_err(codegen_error)?;
            closure_call_ids.insert(*closure, call_id);
            let drop_symbol = format!("joky_closure_drop_{}_{}", closure.0, self.next_function);
            self.next_function += 1;
            let drop_id = self
                .module
                .declare_function(&drop_symbol, Linkage::Local, &drop_signature)
                .map_err(codegen_error)?;
            closure_drop_ids.insert(*closure, drop_id);
        }
        for (closure, captures) in &closure_captures {
            let dynamic = functions
                .iter()
                .flat_map(|f| {
                    f.blocks
                        .iter()
                        .flat_map(move |b| b.statements.iter().map(move |s| (f, s)))
                })
                .find_map(|(f, statement)| {
                    let MirStatement::DynamicValue {
                        destination,
                        methods,
                        ..
                    } = statement
                    else {
                        return None;
                    };
                    let slot = methods.iter().position(|method| method == closure)?;
                    let Type::Dyn(id) = f.value_types[destination.0] else {
                        return None;
                    };
                    Some((
                        methods.len(),
                        types.dynamic_types[id].methods[slot].receiver
                            == crate::syntax::ReceiverMode::Owned,
                    ))
                });
            super::super::functions::values::define_closure_call_glue(
                &mut self.module,
                closure_call_ids[closure],
                func_ids[closure],
                captures,
                &function_types[closure].parameters,
                function_types[closure].return_type,
                function_types[closure].pending_abi,
                pointer_type,
                call_conv,
                types,
                dynamic,
            )?;
            super::super::functions::values::define_closure_drop_glue(
                &mut self.module,
                closure_drop_ids[closure],
                captures,
                &class_drop_ids,
                drop_id,
                pointer_type,
                call_conv,
                types,
                dynamic.map(|(slots, _)| slots),
            )?;
        }

        // Native callbacks: scan calls to extern functions for closure-literal
        // arguments and compile one C trampoline per (closure, signature).
        // The sema contract guarantees the closure literal feeds the call in
        // the same block, so a linear walk pairs values with definitions.
        self.define_map_key_adapters(functions, types, &func_ids)?;
        let mut callback_trampoline_ids: HashMap<crate::codegen::environment::CallbackKey, FuncId> =
            HashMap::new();
        {
            let mut trampoline_defs: Vec<(MirFunctionId, usize)> = Vec::new();
            let mut trampoline_slot: HashMap<(MirFunctionId, usize), FuncId> = HashMap::new();
            for function in functions {
                if function.foreign.is_some() {
                    continue;
                }
                for block in &function.blocks {
                    let mut function_values: HashMap<MirValueId, MirFunctionId> = HashMap::new();
                    for statement in &block.statements {
                        match statement {
                            MirStatement::FunctionValue {
                                destination,
                                function,
                                ..
                            } => {
                                function_values.insert(*destination, *function);
                            }
                            MirStatement::Call {
                                function: callee,
                                arguments,
                                ..
                            } => {
                                let callee_type = &function_types[callee];
                                if !callee_type.foreign {
                                    continue;
                                }
                                for (index, ty) in callee_type.parameters.iter().enumerate() {
                                    let Type::Function(fn_type_id) = ty else {
                                        continue;
                                    };
                                    let Some(arg) = arguments.iter().find(|a| a.parameter == index)
                                    else {
                                        continue;
                                    };
                                    let Some(closure) = function_values.get(&arg.value) else {
                                        return Err(codegen_error(
                                            "native callback argument must be a closure literal",
                                        ));
                                    };
                                    let key = (*closure, *fn_type_id);
                                    let trampoline = match trampoline_slot.get(&key) {
                                        Some(id) => *id,
                                        None => {
                                            let callback = types.function_type(*fn_type_id);
                                            let signature = Signature {
                                                params: callback
                                                    .parameters
                                                    .iter()
                                                    .map(|ty| {
                                                        super::foreign::c_parameter(
                                                            *ty,
                                                            pointer_type,
                                                        )
                                                    })
                                                    .collect::<Result<_, _>>()?,
                                                returns: if callback.return_type == Type::Unit {
                                                    Vec::new()
                                                } else {
                                                    vec![super::foreign::c_parameter(
                                                        callback.return_type,
                                                        pointer_type,
                                                    )?]
                                                },
                                                call_conv,
                                            };
                                            let symbol = format!(
                                                "joky_callback_trampoline_{}_{}",
                                                closure.0, self.next_function
                                            );
                                            self.next_function += 1;
                                            let id = self
                                                .module
                                                .declare_function(
                                                    &symbol,
                                                    Linkage::Local,
                                                    &signature,
                                                )
                                                .map_err(codegen_error)?;
                                            trampoline_slot.insert(key, id);
                                            trampoline_defs.push(key);
                                            id
                                        }
                                    };
                                    callback_trampoline_ids.insert(
                                        crate::codegen::environment::CallbackKey::Literal(
                                            *callee, arg.value,
                                        ),
                                        trampoline,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            for (closure, fn_type_id) in &trampoline_defs {
                self.define_callback_trampoline(
                    trampoline_slot[&(*closure, *fn_type_id)],
                    closure_call_ids[closure],
                    *fn_type_id,
                    types,
                )?;
            }
        }

        let callback_types = functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.statements)
            .filter_map(|statement| match statement {
                MirStatement::RuntimeCall {
                    intrinsic: crate::mir::RuntimeIntrinsic::CCallbackNew(Type::Function(id)),
                    ..
                } => Some(*id),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        for id in callback_types {
            let factory = self.define_retained_callback(id, types)?;
            callback_trampoline_ids.insert(
                crate::codegen::environment::CallbackKey::Retained(id),
                factory,
            );
        }

        // Declare independently callable continuation entries. Resume tails
        // may contain loops and direct calls to known-synchronous functions.
        let continuation_entry_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        let mut continuation_entry_ids = HashMap::new();
        for function in functions {
            for continuation in &function.continuations {
                if !continuation.kind.is_suspending() {
                    continue;
                }
                if machine_entry_blocker(function, continuation, types).is_some()
                    && !(continuation.callee.is_none() && continuation.operation.is_none())
                {
                    continue;
                }
                let symbol = if M::IS_AOT {
                    format!("joky_resume_entry_{}_{}", function.id.0, continuation.id.0)
                } else {
                    let symbol = format!(
                        "joky_resume_entry_{}_{}_{}",
                        function.id.0, continuation.id.0, self.next_function
                    );
                    self.next_function += 1;
                    symbol
                };
                let entry = self
                    .module
                    .declare_function(
                        &symbol,
                        if runtime_scope.is_none() {
                            Linkage::Export
                        } else {
                            Linkage::Local
                        },
                        &continuation_entry_signature,
                    )
                    .map_err(codegen_error)?;
                if runtime_scope.is_none() {
                    let key = self.continuation_entry_key(function.id, continuation.id);
                    self.aot_machine_entries.push((key, symbol));
                }
                continuation_entry_ids.insert((function.id, continuation.id), entry);
            }
        }
        // Second pass: compile all functions
        let machine_entry_literals = self.string_literals.clone();
        for function in functions {
            if function.foreign.is_some() {
                self.compile_foreign_function(function, func_ids[&function.id], types)?;
                continue;
            }
            let literals = collect_function_strings(function);
            let string_values: Vec<_> = literals
                .iter()
                .map(|literal| {
                    self.string_literals
                        .iter()
                        .find(|stored| stored.as_ref() == literal.as_slice())
                        .map(|stored| (StringValue::Pointer(stored.as_ptr()), stored.len()))
                        .expect("collected string literal was retained")
                })
                .collect();
            let mut string_values = string_values.iter().copied();
            self.compile_mir_function(
                function,
                CompileContext {
                    types,
                    func_ids: &func_ids,
                    closure_call_ids: &closure_call_ids,
                    callback_trampoline_ids: &callback_trampoline_ids,
                    closure_drop_ids: &closure_drop_ids,
                    continuation_entry_ids: &continuation_entry_ids,
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
                    class_drop_ids: &class_drop_ids,
                    continuation_cleanup_drop_ids: &continuation_cleanup_drop_ids,
                    pointer_type,
                    string_values: &mut string_values,
                    function_types: &function_types,
                    task_thunk_ids: &task_thunk_ids,
                    handler_thunk_ids: &handler_thunk_ids,
                    task_result_drop_ids: &task_result_drop_ids,
                    task_failure_drop_ids: &task_failure_drop_ids,
                    task_runtime_ids,
                    function_id: function.id,
                    is_main: is_main_mir(function),
                    machine_entry_literals: &machine_entry_literals,
                },
            )?;
        }

        for (handler_function, thunk) in &handler_thunk_ids {
            self.compile_resumable_handler_thunk(
                *thunk,
                func_ids[handler_function],
                &function_types[handler_function],
                pointer_type,
                call_conv,
                types,
                *handler_function_arities
                    .get(handler_function)
                    .expect("handler thunk arity was collected"),
            )?;
        }

        for (task, thunk) in &task_thunk_ids {
            self.compile_task_thunk(
                *thunk,
                func_ids[task],
                &function_types[task],
                pointer_type,
                call_conv,
                types,
            )?;
        }
        for (task, drop) in &task_result_drop_ids {
            self.compile_task_result_drop(
                *drop,
                function_types[task].return_type,
                &class_drop_ids,
                drop_id,
                pointer_type,
                call_conv,
                types,
            )?;
        }
        for (ty, drop) in &continuation_cleanup_drop_ids {
            self.compile_task_result_drop(
                *drop,
                *ty,
                &class_drop_ids,
                drop_id,
                pointer_type,
                call_conv,
                types,
            )?;
        }
        self.flush_pending_machine_functions()?;
        self.module.finalize_module().map_err(codegen_error)?;

        // Object compilation has no runtime scope and must stop after all
        // definitions have been emitted. JIT callers continue through the
        // address publication and entry-point execution below.
        let Some(runtime_scope) = runtime_scope else {
            self.aot_machine_entries
                .sort_unstable_by_key(|(key, _)| *key);
            drop(jit_compile_guard);
            return Ok(());
        };
        // JIT addresses are only stable after finalization. Replace this
        // backend's entries before publishing the finalized address set.
        self.registered_machine_scopes.push(runtime_scope.clone());
        for function in functions {
            for continuation in &function.continuations {
                let key = self.continuation_entry_key(function.id, continuation.id);
                runtime_scope.unregister_machine_entry(key);
                if let Some(entry) = continuation_entry_ids.get(&(function.id, continuation.id)) {
                    let code = self.module.function_address(*entry);
                    let registered = unsafe {
                        runtime_scope.register_machine_entry(key, code.cast::<c_void>().cast_mut())
                    };
                    if !registered {
                        return Err(CodegenError::RuntimeError {
                            message: format!(
                                "failed to register continuation machine entry key {key}"
                            ),
                        });
                    }
                }
            }
        }

        drop(jit_compile_guard);

        // Run the main function
        let main_function_id = functions
            .iter()
            .find(|function| is_main_mir(function))
            .map(|function| function.id)
            .expect("semantic analysis requires a main function");
        let main_id = func_ids
            .get(&main_function_id)
            .expect("main function must be declared");
        let code = self.module.function_address(*main_id);
        // Object backends have no in-process entry address. They stop after
        // definitions are emitted; the caller consumes the finished object.
        if code.is_null() {
            return Ok(());
        }
        runtime_scope.reset_root_failure();
        if function_types[&main_function_id].pending_abi {
            use joky_runtime::host::FunctionCallStatus;
            let status = unsafe { runtime_scope.invoke_pending_entry(code) };
            if matches!(
                status,
                FunctionCallStatus::Failed | FunctionCallStatus::Cancelled
            ) {
                return Err(CodegenError::RuntimeError {
                    message: format!("Pending root returned {status:?}"),
                });
            }
        } else {
            let function: extern "C" fn() = unsafe { std::mem::transmute(code) };
            function();
        }
        // Provider and timer completions resume through the worker pool. Drain
        // callbacks queued by this synchronous entry point before inspecting
        // failures or returning ownership to the caller.
        runtime_scope.wait_for_idle();
        if let Some(message) = runtime_scope.take_main_error() {
            return Err(CodegenError::RuntimeError { message });
        }
        match runtime_scope.function_failure() {
            0 => {}
            status => {
                return Err(CodegenError::RuntimeError {
                    message: format!("Pending continuation chain terminated with status {status}"),
                })
            }
        }
        if let Some(operation) = runtime_scope.take_root_failure_operation() {
            let effect = crate::sema::EffectOperationId {
                effect: crate::sema::EffectId((operation >> 32) as usize),
                operation: operation as u32 as usize,
            };
            let operation_name = types
                .effects()
                .operation_info(effect)
                .map(|operation| operation.name.as_str())
                .unwrap_or("<unknown>");
            let effect_name = types
                .effects()
                .effect(effect.effect)
                .map(|effect| effect.name.as_str())
                .unwrap_or("<unknown>");
            let prefix = match types.effects().operation_mode(effect) {
                Some(crate::sema::EffectMode::Aborts) => "unhandled aborting effect operation",
                _ => "unhandled effect operation",
            };
            return Err(CodegenError::RuntimeError {
                message: format!("{prefix} '{effect_name}.{operation_name}'"),
            });
        }

        Ok(())
    }
}
