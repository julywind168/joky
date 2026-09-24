//! Compiler-provided PartialOrd and Ord implementations.
use super::{CompiledValue, RuntimeCallContext};
use crate::diagnostic::CodegenError;
use cranelift_codegen::ir::{
    condcodes::{FloatCC, IntCC},
    types, InstBuilder,
};
use cranelift_frontend::FunctionBuilder;

pub(super) fn compile_compare(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    left: CompiledValue,
    right: CompiledValue,
    partial: bool,
) -> Result<CompiledValue, CodegenError> {
    let ordered = builder.ins().iconst(types::I8, 0);
    let (less, greater, unordered) = match (left, right) {
        (CompiledValue::Bytes { pointer: left }, CompiledValue::Bytes { pointer: right }) => {
            let call = builder
                .ins()
                .call(context.refs.bytes_compare, &[left, right]);
            let value = builder.inst_results(call)[0];
            builder.ins().call(context.refs.managed_drop, &[left]);
            builder.ins().call(context.refs.managed_drop, &[right]);
            (
                builder.ins().icmp_imm_s(IntCC::SignedLessThan, value, 0),
                builder.ins().icmp_imm_s(IntCC::SignedGreaterThan, value, 0),
                ordered,
            )
        }
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
            let call = builder.ins().call(
                context.refs.string_compare,
                &[left, left_len, right, right_len],
            );
            let value = builder.inst_results(call)[0];
            builder.ins().call(context.refs.managed_drop, &[left]);
            builder.ins().call(context.refs.managed_drop, &[right]);
            (
                builder.ins().icmp_imm_s(IntCC::SignedLessThan, value, 0),
                builder.ins().icmp_imm_s(IntCC::SignedGreaterThan, value, 0),
                ordered,
            )
        }
        (
            CompiledValue::Numeric { value: left, ty },
            CompiledValue::Numeric { value: right, .. },
        ) => {
            if ty.is_float() {
                (
                    builder.ins().fcmp(FloatCC::LessThan, left, right),
                    builder.ins().fcmp(FloatCC::GreaterThan, left, right),
                    builder.ins().fcmp(FloatCC::Unordered, left, right),
                )
            } else {
                let (lt, gt) = if ty.is_signed_integer() {
                    (IntCC::SignedLessThan, IntCC::SignedGreaterThan)
                } else {
                    (IntCC::UnsignedLessThan, IntCC::UnsignedGreaterThan)
                };
                (
                    builder.ins().icmp(lt, left, right),
                    builder.ins().icmp(gt, left, right),
                    ordered,
                )
            }
        }
        (CompiledValue::Boolean { value: left }, CompiledValue::Boolean { value: right })
        | (CompiledValue::Enum { tag: left, .. }, CompiledValue::Enum { tag: right, .. }) => (
            builder.ins().icmp(IntCC::UnsignedLessThan, left, right),
            builder.ins().icmp(IntCC::UnsignedGreaterThan, left, right),
            ordered,
        ),
        _ => {
            return Err(CodegenError::RuntimeError {
                message: "invalid builtin ordering operands".into(),
            })
        }
    };
    let zero = builder.ins().iconst(types::I32, 0);
    let one = builder.ins().iconst(types::I32, 1);
    let two = builder.ins().iconst(types::I32, 2);
    let tag = builder.ins().select(greater, two, one);
    let tag = builder.ins().select(less, zero, tag);
    let ordering = CompiledValue::Enum {
        tag,
        fields: vec![],
        ty: context.types.ordering_type(),
    };
    Ok(if partial {
        let tag = builder.ins().uextend(types::I32, unordered);
        CompiledValue::Enum {
            tag,
            fields: vec![ordering],
            ty: context.types.partial_ordering_type(),
        }
    } else {
        ordering
    })
}
