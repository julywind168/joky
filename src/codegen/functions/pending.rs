//! Shared function-call Pending ABI emission for native and resume entries.

use cranelift_codegen::ir::{types, AbiParam, FuncRef, Function, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};

use crate::codegen::abi::{abi_types, value_from_params};
use crate::codegen::environment::CompiledValue;
use crate::codegen::helpers::codegen_error;
use crate::diagnostic::CodegenError;
use crate::sema::{Type, TypeTable};
use joky_runtime_abi::symbols::*;

pub(super) fn cown_handle_array(
    builder: &mut FunctionBuilder<'_>,
    pointer: cranelift_codegen::ir::Type,
    arguments: &[crate::mir::MirCallArgument],
    values: &std::collections::HashMap<crate::mir::MirValueId, CompiledValue>,
) -> Result<(Value, Value), CodegenError> {
    let (slot, array) = super::statements::create_task_slot(builder, pointer, arguments.len())?;
    for (index, argument) in arguments.iter().enumerate() {
        let Some(CompiledValue::Cown {
            pointer: handle, ..
        }) = values.get(&argument.value)
        else {
            return Err(CodegenError::RuntimeError {
                message: "Cown acquisition handle is missing".into(),
            });
        };
        builder
            .ins()
            .stack_store(pointer, *handle, slot, (index * 8) as i32);
    }
    let count = builder.ins().iconst(pointer, arguments.len() as i64);
    Ok((array, count))
}

#[derive(Clone, Copy)]
pub(super) struct PendingRefs {
    pub(super) begin: FuncRef,
    pub(super) poll: FuncRef,
    pub(super) poll_resume: FuncRef,
    pub(super) take: FuncRef,
    pub(super) cancel: FuncRef,
    pub(super) free: FuncRef,
    pub(super) parent: FuncRef,
    pub(super) own_handler: FuncRef,
    pub(super) exit_handler: FuncRef,
    pub(super) task_wait: FuncRef,
    pub(super) cown_try: FuncRef,
    pub(super) cown_acquire: FuncRef,
    pub(super) cown_wait: FuncRef,
    pub(super) timer: FuncRef,
    pub(super) provider: FuncRef,
    pub(super) fail_chain: FuncRef,
    pub(super) frame: FuncRef,
    pub(super) spill: FuncRef,
    pub(super) retire: FuncRef,
    pub(super) discard_ready: FuncRef,
    pub(super) complete: FuncRef,
}

pub(super) fn declare_pending_refs(
    module: &mut impl Module,
    function: &mut Function,
) -> Result<PendingRefs, CodegenError> {
    let pointer = module.target_config().pointer_type();
    let mut import = |name, parameters| {
        let mut signature = module.make_signature();
        signature.params = vec![AbiParam::new(pointer); parameters];
        signature.returns.push(AbiParam::new(types::I8));
        let id = module
            .declare_function(name, Linkage::Import, &signature)
            .map_err(codegen_error)?;
        Ok::<_, CodegenError>(module.declare_func_in_func(id, function))
    };
    let own_handler = import(CONTINUATION_OWN_HANDLER_SYMBOL, 2)?;
    let exit_handler = import(CONTINUATION_EXIT_HANDLER_SYMBOL, 1)?;
    let cown_try = import(COWN_TRY_ACQUIRE_MANY_SYMBOL, 2)?;
    let cown_wait = import(CONTINUATION_START_COWN_WAIT_SYMBOL, 4)?;
    let cown_acquire = import(CONTINUATION_START_COWN_ACQUIRE_SYMBOL, 4)?;
    let task_wait = import(CONTINUATION_START_TASK_WAIT_SYMBOL, 4)?;
    let begin = import(CONTINUATION_BEGIN_FUNCTION_PENDING_WITH_PARENT_SYMBOL, 2)?;
    let poll = import(
        joky_runtime_abi::symbols::CONTINUATION_POLL_FUNCTION_PENDING_SYMBOL,
        1,
    )?;
    let poll_resume = import(CONTINUATION_POLL_FUNCTION_PENDING_RESUME_SYMBOL, 1)?;
    let take = import(
        joky_runtime_abi::symbols::CONTINUATION_TAKE_FUNCTION_PENDING_RESULT_SYMBOL,
        3,
    )?;
    let cancel = import(joky_runtime_abi::symbols::CONTINUATION_CANCEL_SYMBOL, 1)?;
    let complete = import(
        joky_runtime_abi::symbols::CONTINUATION_COMPLETE_FUNCTION_PENDING_SYMBOL,
        3,
    )?;
    let mut import = |name, parameters: Vec<cranelift_codegen::ir::Type>, result| {
        let mut signature = module.make_signature();
        signature.params = parameters.into_iter().map(AbiParam::new).collect();
        signature.returns.push(AbiParam::new(result));
        let id = module
            .declare_function(name, Linkage::Import, &signature)
            .map_err(codegen_error)?;
        Ok::<_, CodegenError>(module.declare_func_in_func(id, function))
    };
    let parent = import(CONTINUATION_FUNCTION_PARENT_SYMBOL, vec![pointer], pointer)?;
    let frame = import(
        joky_runtime_abi::symbols::CONTINUATION_FRAME_POINTER_SYMBOL,
        vec![pointer],
        pointer,
    )?;
    let spill = import(
        joky_runtime_abi::symbols::CONTINUATION_SPILL_POINTER_SYMBOL,
        vec![pointer],
        pointer,
    )?;
    let timer = import(
        joky_runtime_abi::symbols::CONTINUATION_START_PENDING_TIMER_SYMBOL,
        vec![pointer, types::I64, types::I64, pointer],
        types::I8,
    )?;
    let provider = import(
        joky_runtime_abi::symbols::CONTINUATION_START_PENDING_PROVIDER_SYMBOL,
        vec![pointer, types::I64, pointer, pointer, pointer],
        types::I8,
    )?;
    let mut signature = module.make_signature();
    signature.params = vec![AbiParam::new(pointer), AbiParam::new(types::I8)];
    let id = module
        .declare_function(
            CONTINUATION_FAIL_FUNCTION_CHAIN_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(codegen_error)?;
    let fail_chain = module.declare_func_in_func(id, function);
    signature.params = vec![AbiParam::new(pointer)];
    let id = module
        .declare_function(
            CONTINUATION_DISCARD_READY_CALL_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(codegen_error)?;
    let discard_ready = module.declare_func_in_func(id, function);
    let id = module
        .declare_function(CONTINUATION_FREE_SYMBOL, Linkage::Import, &signature)
        .map_err(codegen_error)?;
    let free = module.declare_func_in_func(id, function);
    signature.params = vec![AbiParam::new(pointer); 2];
    let id = module
        .declare_function(
            CONTINUATION_RETIRE_FUNCTION_ACTIVATION_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(codegen_error)?;
    let retire = module.declare_func_in_func(id, function);
    Ok(PendingRefs {
        own_handler,
        exit_handler,
        task_wait,
        cown_try,
        cown_acquire,
        cown_wait,
        begin,
        poll,
        poll_resume,
        take,
        cancel,
        free,
        parent,
        timer,
        provider,
        fail_chain,
        frame,
        spill,
        retire,
        discard_ready,
        complete,
    })
}

/// Propagate a terminal status without interpreting placeholder return words.
pub(super) fn return_status_if_error(
    builder: &mut FunctionBuilder<'_>,
    status: Value,
    cleanup: impl FnOnce(&mut FunctionBuilder<'_>),
) {
    let failed = builder.create_block();
    let ready = builder.create_block();
    builder.ins().brif(status, failed, &[], ready, &[]);
    builder.switch_to_block(failed);
    cleanup(builder);
    return_status(builder, status);
    builder.switch_to_block(ready);
    builder.seal_block(failed);
    builder.seal_block(ready);
}

pub(super) fn return_status(builder: &mut FunctionBuilder<'_>, status: Value) {
    let returns = builder
        .func
        .signature
        .returns
        .iter()
        .skip(1)
        .map(|param| param.value_type)
        .collect::<Vec<_>>();
    let mut values = vec![status];
    for ty in returns {
        values.push(if ty == types::F32 {
            builder.ins().f32const(0.0)
        } else if ty == types::F64 {
            builder.ins().f64const(0.0)
        } else {
            builder.ins().iconst(ty, 0)
        });
    }
    builder.ins().return_(&values);
}

/// The result is copied under the runtime cancellation lock, including Unit.
/// The caller must branch on status before using the returned value.
pub(super) fn take_result(
    builder: &mut FunctionBuilder<'_>,
    take: FuncRef,
    handle: Value,
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<(Value, CompiledValue), CodegenError> {
    let components = abi_types(ty, pointer_type, types);
    let (_, output) = super::statements::create_task_slot(builder, pointer_type, components.len())?;
    // Failed/cancelled transfers leave output untouched. Initialize each word
    // so even the unused SSA placeholders on those paths are defined.
    let zero = builder.ins().iconst(pointer_type, 0);
    for index in 0..components.len() {
        builder
            .ins()
            .store(MemFlagsData::new(), zero, output, (index * 8) as i32);
    }
    let size = builder
        .ins()
        .iconst(pointer_type, (components.len() * 8) as i64);
    let call = builder.ins().call(take, &[handle, output, size]);
    let status = builder.inst_results(call)[0];
    let values = components
        .into_iter()
        .enumerate()
        .map(|(index, ty)| {
            builder
                .ins()
                .load(ty, MemFlagsData::new(), output, (index * 8) as i32)
        })
        .collect::<Vec<_>>();
    let result =
        value_from_params(&values, ty, types).map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?;
    Ok((status, result))
}
