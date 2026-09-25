//! Equality for compiler-provided PartialEq implementations.
use super::{CompiledValue, RuntimeCallContext};
use crate::{diagnostic::CodegenError, sema::Type, syntax::BinaryOp};
use cranelift_codegen::ir::{condcodes::IntCC, types, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;

/// Consumes the retained operand copies, including shared structural fields.
pub(super) fn compile_equal(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    left: CompiledValue,
    right: CompiledValue,
) -> Result<Value, CodegenError> {
    let value = match (left, right) {
        (CompiledValue::Unit, CompiledValue::Unit) => builder.ins().iconst(types::I8, 1),
        (
            CompiledValue::String {
                pointer: left,
                length: left_len,
            },
            CompiledValue::String {
                pointer: right,
                length: right_len,
            },
        ) => {
            let call = builder
                .ins()
                .call(context.refs.string_eq, &[left, left_len, right, right_len]);
            let value = builder.inst_results(call)[0];
            builder.ins().call(context.refs.managed_drop, &[left]);
            builder.ins().call(context.refs.managed_drop, &[right]);
            value
        }
        (CompiledValue::Bytes { pointer: left }, CompiledValue::Bytes { pointer: right }) => {
            let call = builder.ins().call(context.refs.bytes_eq, &[left, right]);
            let value = builder.inst_results(call)[0];
            builder.ins().call(context.refs.managed_drop, &[left]);
            builder.ins().call(context.refs.managed_drop, &[right]);
            value
        }
        (CompiledValue::Enum { tag: left, .. }, CompiledValue::Enum { tag: right, .. }) => {
            builder.ins().icmp(IntCC::Equal, left, right)
        }
        (
            CompiledValue::Struct { fields: left, .. },
            CompiledValue::Struct { fields: right, .. },
        )
        | (
            CompiledValue::Tuple { elements: left, .. },
            CompiledValue::Tuple {
                elements: right, ..
            },
        ) => {
            let mut equal = builder.ins().iconst(types::I8, 1);
            for (left, right) in left.into_iter().zip(right) {
                let field = compile_equal(builder, context, left, right)?;
                equal = builder.ins().band(equal, field);
            }
            equal
        }
        (left, right) => {
            let result = super::super::super::operators::compile_binary_value(
                builder,
                BinaryOp::Equal,
                left,
                right,
                Type::Bool,
                context.pointer_type,
                context.refs.panic,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            let CompiledValue::Boolean { value } = result else {
                unreachable!("comparison returns Bool")
            };
            value
        }
    };
    Ok(value)
}
