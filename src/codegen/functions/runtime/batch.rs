use super::*;
use crate::codegen::abi::value_arguments;

pub(super) fn compile_batch_call(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    destination: &MirValueId,
    intrinsic: &RuntimeIntrinsic,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
) -> Result<bool, CodegenError> {
    if !matches!(
        intrinsic,
        RuntimeIntrinsic::BatchNew(_)
            | RuntimeIntrinsic::BatchWorker
            | RuntimeIntrinsic::BatchNext(_)
            | RuntimeIntrinsic::BatchPush(_)
            | RuntimeIntrinsic::BatchFinish(_)
            | RuntimeIntrinsic::SeqNew
            | RuntimeIntrinsic::SeqPush(_)
            | RuntimeIntrinsic::SeqFinish(_)
    ) {
        return Ok(false);
    }
    let params = arguments
        .iter()
        .map(|arg| {
            values
                .get(&arg.value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "missing batch argument".to_owned(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut words: Vec<_> = params.into_iter().flat_map(value_arguments).collect();
    let pointer = context.pointer_type;
    let result = match intrinsic {
        RuntimeIntrinsic::BatchNew(_) => {
            let call = builder.ins().call(context.refs.batch_new, &words);
            CompiledValue::NativeHandle {
                pointer: builder.inst_results(call)[0],
                ty: Type::Batch,
            }
        }
        RuntimeIntrinsic::BatchWorker => {
            let call = builder.ins().call(context.refs.batch_worker, &words);
            let value = builder.inst_results(call)[0];
            builder.ins().call(context.refs.managed_drop, &[words[0]]);
            CompiledValue::Boolean { value }
        }
        RuntimeIntrinsic::BatchNext(element) => {
            let (slot, address) = create_list_word_slot(builder, pointer, 1)?;
            words.push(address);
            let call = builder.ins().call(context.refs.batch_next, &words);
            let node = builder.inst_results(call)[0];
            let index = builder.ins().stack_load(pointer, types::I64, slot, 0);
            let list = Type::List(context.types.list_id(*element).expect("checked input List"));
            let ty = Type::Tuple(
                context
                    .types
                    .tuple_id(&[Type::U64, list])
                    .expect("checked item tuple"),
            );
            builder.ins().call(context.refs.managed_drop, &[words[0]]);
            CompiledValue::Tuple {
                elements: vec![
                    CompiledValue::Numeric {
                        value: index,
                        ty: Type::U64,
                    },
                    CompiledValue::List {
                        pointer: node,
                        ty: list,
                    },
                ],
                ty,
            }
        }
        RuntimeIntrinsic::BatchPush(_) => {
            builder.ins().call(context.refs.batch_push, &words);
            builder.ins().call(context.refs.managed_drop, &[words[0]]);
            CompiledValue::Unit
        }
        RuntimeIntrinsic::BatchFinish(element) => {
            let call = builder.ins().call(context.refs.batch_finish, &words);
            let result = builder.inst_results(call)[0];
            builder.ins().call(context.refs.managed_drop, &[words[0]]);
            CompiledValue::List {
                pointer: result,
                ty: Type::List(
                    context
                        .types
                        .list_id(*element)
                        .expect("checked output List"),
                ),
            }
        }
        RuntimeIntrinsic::SeqNew => {
            let call = builder.ins().call(context.refs.seq_new, &[]);
            CompiledValue::NativeHandle {
                pointer: builder.inst_results(call)[0],
                ty: Type::SeqBuilder,
            }
        }
        RuntimeIntrinsic::SeqPush(_) => {
            // The runtime adopts the singleton node; the borrowed builder
            // reference is released afterwards.
            builder.ins().call(context.refs.seq_push, &words);
            builder.ins().call(context.refs.managed_drop, &[words[0]]);
            CompiledValue::Unit
        }
        RuntimeIntrinsic::SeqFinish(element) => {
            // Detaches the list; the borrowed builder reference is released
            // afterwards, matching BatchFinish.
            let call = builder.ins().call(context.refs.seq_finish, &words);
            let result = builder.inst_results(call)[0];
            builder.ins().call(context.refs.managed_drop, &[words[0]]);
            CompiledValue::List {
                pointer: result,
                ty: Type::List(
                    context
                        .types
                        .list_id(*element)
                        .expect("checked output List"),
                ),
            }
        }
        _ => unreachable!(),
    };
    values.insert(*destination, result);
    Ok(true)
}
