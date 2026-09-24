//! Conservative constant folding for scalar MIR expressions.

use crate::diagnostic::Diagnostic;
use crate::sema::Type;
use crate::syntax::BinaryOp;

use super::{MirPass, MirProgram};
use crate::mir::{MirConstant, MirStatement, NumericMethod};
use std::collections::HashMap;

/// Fold `Const` operands of a `Binary` statement without changing value IDs.
///
/// Runtime calls, strings and ownership-producing statements are deliberately
/// left untouched. Arithmetic that could trap or has ambiguous overflow
/// behavior is also skipped.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ConstantFolding;

impl MirPass for ConstantFolding {
    fn name(&self) -> &'static str {
        "constant-folding"
    }

    fn run(&mut self, program: &mut MirProgram) -> Result<usize, Diagnostic> {
        let mut changes = 0;
        for function in &mut program.functions {
            for block in &mut function.blocks {
                // A block-local environment is deliberately conservative: values
                // from another CFG branch are not assumed to dominate this block.
                let mut constants: HashMap<_, (MirConstant, Type)> = HashMap::new();
                for statement in &mut block.statements {
                    match statement {
                        MirStatement::Const { destination, value } => {
                            let ty = function.value_types[destination.0];
                            constants.insert(*destination, (value.clone(), ty));
                        }
                        MirStatement::Binary {
                            destination,
                            op,
                            left,
                            right,
                        } => {
                            let folded = constants.get(left).zip(constants.get(right)).and_then(
                                |((left, left_type), (right, right_type))| {
                                    if left_type != right_type {
                                        return None;
                                    }
                                    fold_binary(*op, left, right, *left_type)
                                },
                            );
                            if let Some(value) = folded {
                                let destination = *destination;
                                let ty = function.value_types[destination.0];
                                *statement = MirStatement::Const {
                                    destination,
                                    value: value.clone(),
                                };
                                constants.insert(destination, (value, ty));
                                changes += 1;
                            }
                        }
                        MirStatement::Numeric {
                            destination,
                            method,
                            arguments,
                        } => {
                            let folded = fold_numeric(
                                *method,
                                arguments,
                                &constants,
                                function.value_types[destination.0],
                            );
                            if let Some(value) = folded {
                                let destination = *destination;
                                let ty = function.value_types[destination.0];
                                *statement = MirStatement::Const {
                                    destination,
                                    value: value.clone(),
                                };
                                constants.insert(destination, (value, ty));
                                changes += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(changes)
    }
}

fn fold_binary(
    op: BinaryOp,
    left: &MirConstant,
    right: &MirConstant,
    ty: Type,
) -> Option<MirConstant> {
    match (left, right) {
        (MirConstant::Integer(left), MirConstant::Integer(right)) if ty.is_integer() => {
            fold_integer(op, *left, *right, ty)
        }
        (MirConstant::Float(left), MirConstant::Float(right)) if ty.is_float() => {
            fold_float(op, *left, *right, ty)
        }
        (MirConstant::Boolean(left), MirConstant::Boolean(right)) if ty == Type::Bool => match op {
            BinaryOp::Equal => Some(MirConstant::Boolean(left == right)),
            BinaryOp::NotEqual => Some(MirConstant::Boolean(left != right)),
            _ => None,
        },
        _ => None,
    }
}

fn fold_integer(op: BinaryOp, left: u64, right: u64, ty: Type) -> Option<MirConstant> {
    let result = match op {
        BinaryOp::Add => Some(MirConstant::Integer(normalize_int(
            left.wrapping_add(right),
            ty,
        ))),
        BinaryOp::Subtract => Some(MirConstant::Integer(normalize_int(
            left.wrapping_sub(right),
            ty,
        ))),
        BinaryOp::Multiply => Some(MirConstant::Integer(normalize_int(
            left.wrapping_mul(right),
            ty,
        ))),
        BinaryOp::Divide if right != 0 => {
            if ty.is_signed_integer() {
                let left = signed_int(left, ty);
                let right = signed_int(right, ty);
                left.checked_div(right)
                    .map(|value| MirConstant::Integer(normalize_int(value as u64, ty)))
            } else {
                Some(MirConstant::Integer(left / right))
            }
        }
        BinaryOp::Remainder if normalize_int(right, ty) != 0 => fold_remainder(left, right, ty),
        BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
            Some(MirConstant::Integer(fold_shift(op, left, right, ty)))
        }
        BinaryOp::BitAnd => Some(MirConstant::Integer(
            normalize_int(left, ty) & normalize_int(right, ty),
        )),
        BinaryOp::BitXor => Some(MirConstant::Integer(
            normalize_int(left, ty) ^ normalize_int(right, ty),
        )),
        BinaryOp::BitOr => Some(MirConstant::Integer(
            normalize_int(left, ty) | normalize_int(right, ty),
        )),
        BinaryOp::Equal => Some(MirConstant::Boolean(
            normalize_int(left, ty) == normalize_int(right, ty),
        )),
        BinaryOp::NotEqual => Some(MirConstant::Boolean(
            normalize_int(left, ty) != normalize_int(right, ty),
        )),
        BinaryOp::Less => Some(MirConstant::Boolean(
            compare_int(left, right, ty) == std::cmp::Ordering::Less,
        )),
        BinaryOp::LessEqual => Some(MirConstant::Boolean(
            compare_int(left, right, ty) != std::cmp::Ordering::Greater,
        )),
        BinaryOp::Greater => Some(MirConstant::Boolean(
            compare_int(left, right, ty) == std::cmp::Ordering::Greater,
        )),
        BinaryOp::GreaterEqual => Some(MirConstant::Boolean(
            compare_int(left, right, ty) != std::cmp::Ordering::Less,
        )),
        BinaryOp::Remainder | BinaryOp::Divide | BinaryOp::And | BinaryOp::Or => None,
    }?;
    Some(result)
}

fn fold_float(op: BinaryOp, left: f64, right: f64, ty: Type) -> Option<MirConstant> {
    let (left, right) = if ty == Type::F32 {
        (left as f32 as f64, right as f32 as f64)
    } else {
        (left, right)
    };
    match op {
        BinaryOp::Add => Some(MirConstant::Float(normalize_float(left + right, ty))),
        BinaryOp::Subtract => Some(MirConstant::Float(normalize_float(left - right, ty))),
        BinaryOp::Multiply => Some(MirConstant::Float(normalize_float(left * right, ty))),
        BinaryOp::Divide if right != 0.0 => {
            Some(MirConstant::Float(normalize_float(left / right, ty)))
        }
        BinaryOp::Remainder if right != 0.0 => {
            Some(MirConstant::Float(normalize_float(left % right, ty)))
        }
        BinaryOp::Equal => Some(MirConstant::Boolean(left == right)),
        BinaryOp::NotEqual => Some(MirConstant::Boolean(left != right)),
        BinaryOp::Less => Some(MirConstant::Boolean(left < right)),
        BinaryOp::LessEqual => Some(MirConstant::Boolean(left <= right)),
        BinaryOp::Greater => Some(MirConstant::Boolean(left > right)),
        BinaryOp::GreaterEqual => Some(MirConstant::Boolean(left >= right)),
        BinaryOp::Remainder
        | BinaryOp::Divide
        | BinaryOp::ShiftLeft
        | BinaryOp::ShiftRight
        | BinaryOp::BitAnd
        | BinaryOp::BitXor
        | BinaryOp::BitOr
        | BinaryOp::And
        | BinaryOp::Or => None,
    }
}

fn fold_numeric(
    method: NumericMethod,
    arguments: &[crate::mir::MirValueId],
    constants: &HashMap<crate::mir::MirValueId, (MirConstant, Type)>,
    result: Type,
) -> Option<MirConstant> {
    let operands = arguments
        .iter()
        .map(|argument| constants.get(argument).cloned())
        .collect::<Option<Vec<_>>>()?;
    match method {
        NumericMethod::Abs => {
            let (value, ty) = operands.first()?;
            if *ty != result {
                return None;
            }
            match value {
                MirConstant::Integer(bits) if ty.is_signed_integer() => signed_int(*bits, *ty)
                    .checked_abs()
                    .map(|value| MirConstant::Integer(normalize_int(value as u64, *ty))),
                MirConstant::Integer(bits) if ty.is_integer() => Some(MirConstant::Integer(*bits)),
                MirConstant::Float(value) if ty.is_float() => {
                    let value = normalize_float(*value, *ty);
                    Some(MirConstant::Float(if value.is_nan() {
                        value
                    } else {
                        value.abs()
                    }))
                }
                _ => None,
            }
        }
        NumericMethod::Min | NumericMethod::Max => {
            let (left, left_type) = operands.first()?;
            let (right, right_type) = operands.get(1)?;
            if *left_type != *right_type || *left_type != result {
                return None;
            }
            match (left, right) {
                (MirConstant::Integer(left), MirConstant::Integer(right))
                    if left_type.is_integer() =>
                {
                    let chosen = if method == NumericMethod::Max {
                        if left_type.is_signed_integer() {
                            if signed_int(*left, *left_type) > signed_int(*right, *left_type) {
                                *left
                            } else {
                                *right
                            }
                        } else if normalize_int(*left, *left_type)
                            > normalize_int(*right, *left_type)
                        {
                            *left
                        } else {
                            *right
                        }
                    } else if left_type.is_signed_integer() {
                        if signed_int(*left, *left_type) < signed_int(*right, *left_type) {
                            *left
                        } else {
                            *right
                        }
                    } else if normalize_int(*left, *left_type) < normalize_int(*right, *left_type) {
                        *left
                    } else {
                        *right
                    };
                    Some(MirConstant::Integer(chosen))
                }
                (MirConstant::Float(left), MirConstant::Float(right)) if left_type.is_float() => {
                    let left = normalize_float(*left, *left_type);
                    let right = normalize_float(*right, *left_type);
                    let value = if left.is_nan() {
                        left
                    } else if right.is_nan() {
                        right
                    } else if method == NumericMethod::Max {
                        left.max(right)
                    } else {
                        left.min(right)
                    };
                    Some(MirConstant::Float(value))
                }
                _ => None,
            }
        }
        NumericMethod::IntegerCast => {
            let (value, from) = operands.first()?;
            match value {
                MirConstant::Integer(bits) if from.is_integer() && result.is_integer() => Some(
                    MirConstant::Integer(convert_const_int(*bits, *from, result)),
                ),
                _ => None,
            }
        }
    }
}

fn convert_const_int(bits: u64, from: Type, to: Type) -> u64 {
    if integer_width(to) > integer_width(from) && from.is_signed_integer() {
        normalize_int(signed_int(bits, from) as u64, to)
    } else {
        normalize_int(bits, to)
    }
}

fn fold_remainder(left: u64, right: u64, ty: Type) -> Option<MirConstant> {
    if ty.is_signed_integer() {
        let left = signed_int(left, ty);
        let right = signed_int(right, ty);
        left.checked_rem(right)
            .map(|value| MirConstant::Integer(normalize_int(value as u64, ty)))
    } else {
        Some(MirConstant::Integer(
            normalize_int(left, ty) % normalize_int(right, ty),
        ))
    }
}

fn fold_shift(op: BinaryOp, left: u64, right: u64, ty: Type) -> u64 {
    let width = integer_width(ty);
    let amount = (normalize_int(right, ty) & u64::from(width - 1)) as u32;
    let shifted = match op {
        BinaryOp::ShiftLeft => normalize_int(left, ty) << amount,
        BinaryOp::ShiftRight if ty.is_signed_integer() => (signed_int(left, ty) >> amount) as u64,
        BinaryOp::ShiftRight => normalize_int(left, ty) >> amount,
        _ => left,
    };
    normalize_int(shifted, ty)
}

fn integer_width(ty: Type) -> u32 {
    match ty {
        Type::I8 | Type::U8 => 8,
        Type::I16 | Type::U16 => 16,
        Type::I32 | Type::U32 => 32,
        _ => 64,
    }
}

fn normalize_float(value: f64, ty: Type) -> f64 {
    if ty == Type::F32 {
        value as f32 as f64
    } else {
        value
    }
}

fn normalize_int(value: u64, ty: Type) -> u64 {
    let bits = match ty {
        Type::I8 | Type::U8 => 8,
        Type::I16 | Type::U16 => 16,
        Type::I32 | Type::U32 => 32,
        Type::I64 | Type::U64 => 64,
        _ => return value,
    };
    if bits == 64 {
        value
    } else {
        value & ((1u64 << bits) - 1)
    }
}

fn signed_int(value: u64, ty: Type) -> i128 {
    let value = normalize_int(value, ty) as i128;
    let bits = match ty {
        Type::I8 => 8,
        Type::I16 => 16,
        Type::I32 => 32,
        Type::I64 => 64,
        _ => 128,
    };
    if bits < 128 && value >= (1i128 << (bits - 1)) {
        value - (1i128 << bits)
    } else {
        value
    }
}

fn compare_int(left: u64, right: u64, ty: Type) -> std::cmp::Ordering {
    if ty.is_signed_integer() {
        signed_int(left, ty).cmp(&signed_int(right, ty))
    } else {
        normalize_int(left, ty).cmp(&normalize_int(right, ty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::CoreProgram;
    use crate::mir::passes::MirPassManager;
    use crate::{sema, syntax};

    fn lower(source: &str) -> MirProgram {
        let program = syntax::parse_program(source).expect("source should parse");
        let types = sema::check_program(&program).expect("source should type-check");
        let core = CoreProgram::lower(program, types).expect("HIR lowering should succeed");
        MirProgram::lower(&core).expect("MIR lowering should succeed")
    }

    #[test]
    fn folds_integer_addition() {
        let mut mir = lower("fn answer() -> Int32 { 2 + 3 } fn main() { let value = answer() }");
        let changes = ConstantFolding
            .run(&mut mir)
            .expect("folding should succeed");

        assert!(changes > 0);
        assert!(mir.functions()[0].blocks.iter().any(|block| {
            block.statements.iter().any(|statement| {
                matches!(
                    statement,
                    MirStatement::Const {
                        value: MirConstant::Integer(5),
                        ..
                    }
                )
            })
        }));
        assert!(!mir.functions()[0].blocks.iter().any(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::Binary { .. }))
        }));
        mir.verify().expect("folded MIR should remain valid");
    }

    #[test]
    fn folds_boolean_equality() {
        let mut mir =
            lower("fn answer() -> Bool { true == false } fn main() { let value = answer() }");
        assert_eq!(ConstantFolding.run(&mut mir).unwrap(), 1);
        assert!(mir.functions()[0].blocks.iter().any(|block| {
            block.statements.iter().any(|statement| {
                matches!(
                    statement,
                    MirStatement::Const {
                        value: MirConstant::Boolean(false),
                        ..
                    }
                )
            })
        }));
    }

    #[test]
    fn folds_f64_addition() {
        let mut mir =
            lower("fn answer() -> Float64 { 1.5 + 2.25 } fn main() { let value = answer() }");
        assert_eq!(ConstantFolding.run(&mut mir).unwrap(), 1);
        assert!(mir.functions()[0].blocks.iter().any(|block| {
            block.statements.iter().any(|statement| {
                matches!(
                    statement,
                    MirStatement::Const {
                        value: MirConstant::Float(value),
                        ..
                    } if (*value - 3.75).abs() < f64::EPSILON
                )
            })
        }));
    }

    #[test]
    fn default_pipeline_reports_constant_folding() {
        let mut mir = lower("fn answer() -> Int32 { 2 + 3 } fn main() { let value = answer() }");
        let report = MirPassManager::default_pipeline()
            .run(&mut mir)
            .expect("default pipeline should succeed");

        assert_eq!(report.passes.len(), 1);
        assert_eq!(report.passes[0].name, "constant-folding");
        assert!(report.passes[0].changes > 0);
    }

    #[test]
    fn saturating_casts_fold_before_truncating_or_reinterpreting_bits() {
        for (expression, target, expected) in [
            ("300 as| Int8", "Int8", 127),
            ("(0 - 200) as| Int8", "Int8", 128),
            ("(0 - 128) as| Int8", "Int8", 128),
            ("42 as| Int8", "Int8", 42),
            ("300 as| UInt8", "UInt8", 255),
            ("(0 - 1) as| UInt8", "UInt8", 0),
            ("255 as% Int8 as| UInt64", "UInt64", 0),
            ("((0 - 1) as% UInt64) as| Int64", "Int64", i64::MAX as u64),
        ] {
            let mut mir = lower(&format!(
                "fn answer() -> {target} {{ {expression} }} fn main() {{}}"
            ));
            MirPassManager::default_pipeline().run(&mut mir).unwrap();
            let function = mir.functions().iter().find(|f| f.name == "answer").unwrap();
            assert!(
                !function
                    .blocks
                    .iter()
                    .flat_map(|block| &block.statements)
                    .any(|statement| matches!(statement, MirStatement::Numeric { .. })),
                "{expression} should be completely folded"
            );
            let returned = function
                .blocks
                .iter()
                .find_map(|block| match block.terminator {
                    Some(crate::mir::MirTerminator::Return(Some(value))) => Some(value),
                    _ => None,
                })
                .unwrap();
            let actual = function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .find_map(|statement| match statement {
                    MirStatement::Const {
                        destination,
                        value: MirConstant::Integer(value),
                    } if *destination == returned => Some(*value),
                    _ => None,
                });
            assert_eq!(actual, Some(expected), "{expression}");
        }
    }
}
