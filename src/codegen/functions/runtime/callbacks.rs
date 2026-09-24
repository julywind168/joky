use super::*;
use cranelift_codegen::ir::{MemFlagsData, StackSlotData, StackSlotKind};

pub(super) fn compile_callback_call(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    destination: &MirValueId,
    intrinsic: &RuntimeIntrinsic,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &Environment,
) -> Result<bool, CodegenError> {
    let pointer = context.pointer_type;
    let value = match intrinsic {
        RuntimeIntrinsic::CCallbackNew(Type::Function(id)) => {
            let CompiledValue::Function {
                code,
                environment: captured,
                ..
            } = values[&arguments[0].value]
            else {
                return Err(crate::codegen::helpers::codegen_error(
                    "callback closure was not compiled",
                ));
            };
            let fallback = values[&arguments[1].value].clone();
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let storage = builder.ins().stack_addr(pointer, slot, 0);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().store(MemFlagsData::new(), zero, storage, 0);
            for value in crate::codegen::abi::value_arguments(fallback) {
                builder.ins().store(MemFlagsData::new(), value, storage, 0);
            }
            let fallback = builder
                .ins()
                .load(types::I64, MemFlagsData::new(), storage, 0);
            let factory = environment.callback_trampoline_refs
                [&crate::codegen::environment::CallbackKey::Retained(*id)];
            let call = builder.ins().call(factory, &[code, captured, fallback]);
            CompiledValue::NativeHandle {
                pointer: builder.inst_results(call)[0],
                ty: Type::CCallback,
            }
        }
        RuntimeIntrinsic::CCallbackFunction
        | RuntimeIntrinsic::CCallbackContext
        | RuntimeIntrinsic::CCallbackClose
        | RuntimeIntrinsic::CCallbackFailed => {
            let CompiledValue::NativeHandle {
                pointer: handle, ..
            } = values[&arguments[0].value]
            else {
                return Err(crate::codegen::helpers::codegen_error(
                    "callback handle was not compiled",
                ));
            };
            if matches!(intrinsic, RuntimeIntrinsic::CCallbackClose) {
                builder.ins().call(context.refs.managed_drop, &[handle]);
                CompiledValue::Unit
            } else {
                let function = match intrinsic {
                    RuntimeIntrinsic::CCallbackFunction => context.refs.callback_function,
                    RuntimeIntrinsic::CCallbackContext => context.refs.callback_context,
                    _ => context.refs.callback_failed,
                };
                let call = builder.ins().call(function, &[handle]);
                let value = builder.inst_results(call)[0];
                if matches!(intrinsic, RuntimeIntrinsic::CCallbackFailed) {
                    CompiledValue::Boolean { value }
                } else {
                    let id = context
                        .types
                        .c_pointer_id(Type::Unit)
                        .expect("verified callback pointer type");
                    CompiledValue::NativeHandle {
                        pointer: value,
                        ty: if matches!(intrinsic, RuntimeIntrinsic::CCallbackFunction) {
                            Type::CPtr(id)
                        } else {
                            Type::CMutPtr(id)
                        },
                    }
                }
            }
        }
        _ => return Ok(false),
    };
    values.insert(*destination, value);
    Ok(true)
}
