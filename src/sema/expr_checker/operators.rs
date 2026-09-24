use super::*;
use crate::sema::expectation::TypeExpectation;
use crate::sema::types::{integer_shape, Type};
use crate::sema::validation::{
    check_negative_integer, constant_integer, is_numeric_literal, type_mismatch,
};
use crate::syntax::{BinaryOp, CastMode, Expr, ExprKind};

impl Checker {
    /// Checks a cast expression against the static losslessness matrix. The
    /// plain `as` form is reserved for conversions whose target value range
    /// contains the source range; lossy conversions require `as?` (checked),
    /// `as%` (wrapping), or `as|` (saturating). Floats are rejected until
    /// float cast semantics are designed.
    pub(super) fn check_cast_modes(
        &mut self,
        mode: CastMode,
        source: Type,
        target: Type,
        span: crate::Span,
    ) -> Result<Type, SemanticError> {
        let Some(source_shape) = integer_shape(source) else {
            return Err(SemanticError::IntegerCastRequiresInteger { span });
        };
        let Some(target_shape) = integer_shape(target) else {
            return Err(SemanticError::IntegerCastRequiresInteger { span });
        };
        let lossless = match (source_shape.1, target_shape.1) {
            (false, false) => source_shape.0 <= target_shape.0,
            (true, true) => source_shape.0 <= target_shape.0,
            // Unsigned sources only promote into signed targets one width
            // wider or more; everything else loses values.
            (false, true) => source_shape.0 < target_shape.0,
            (true, false) => false,
        };
        match mode {
            CastMode::Lossless if lossless => Ok(target),
            CastMode::Checked | CastMode::Wrapping | CastMode::Saturating if lossless => {
                Err(SemanticError::LosslessCastRequiresPlainAs { span })
            }
            CastMode::Lossless => Err(SemanticError::LossyCastRequiresExplicitMode { span }),
            CastMode::Checked => {
                let id = intern_result(&mut self.result_types, target, Type::String);
                Ok(Type::Result(id))
            }
            CastMode::Wrapping | CastMode::Saturating => Ok(target),
        }
    }

    pub(super) fn check_negation(
        &mut self,
        expression: &Expr,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        let value_type = expectation.ty().filter(|ty| ty.is_numeric()).unwrap_or({
            if matches!(expression.kind, ExprKind::Float(_)) {
                Type::F64
            } else {
                Type::I32
            }
        });
        if value_type.is_integer() && !value_type.is_signed_integer() {
            return Err(SemanticError::CannotNegateUnsigned {
                type_name: type_name(value_type).to_owned(),
                span: expression.span,
            });
        }

        if let ExprKind::Integer(value) = expression.kind {
            check_negative_integer(value, value_type, expression.span)?;
            self.types.insert(expression.id, value_type);
        } else {
            self.check_expression(expression, TypeExpectation::require(value_type))?;
        }
        Ok(value_type)
    }

    pub(super) fn check_binary_tree(
        &mut self,
        expression: &Expr,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        let mut stack = Vec::new();
        let mut current = expression;
        let mut child_expectation = expectation;
        while let ExprKind::Binary { op, left, right } = &current.kind {
            stack.push((
                current,
                *op,
                left.as_ref(),
                right.as_ref(),
                child_expectation,
            ));
            child_expectation = binary_left_expectation(*op, child_expectation);
            current = left.as_ref();
        }

        let last = stack.len().saturating_sub(1);
        let mut left_type = None;
        for (index, (node, operator, left, right, child_expectation)) in
            stack.into_iter().rev().enumerate()
        {
            let ty = self.check_binary(operator, left, right, left_type, child_expectation)?;
            if index != last {
                if child_expectation.is_required() {
                    if let Some(expected) = child_expectation.ty() {
                        if ty != expected {
                            return Err(type_mismatch(expected, ty, node.span));
                        }
                    }
                }
                self.record_drop_effects(ty, node.span)?;
                self.types.insert(node.id, ty);
            }
            left_type = Some(ty);
        }
        Ok(left_type.expect("binary tree has at least one operator"))
    }

    pub(super) fn check_binary(
        &mut self,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        left_type: Option<Type>,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        if matches!(operator, BinaryOp::And | BinaryOp::Or) {
            self.check_binary_left(left, left_type, TypeExpectation::require(Type::Bool))?;
            self.check_expression(right, TypeExpectation::require(Type::Bool))?;
            return Ok(Type::Bool);
        }
        if operator == BinaryOp::Add {
            if let Some(expected) = expectation.ty().filter(|ty| ty.is_numeric()) {
                self.check_binary_left(left, left_type, TypeExpectation::require(expected))?;
                self.check_expression(right, TypeExpectation::require(expected))?;
                return Ok(expected);
            }
            // String concatenation needs the left operand's type, but a bare
            // numeric literal must stay untyped so `42 + x` can take `x`'s type.
            if left_type.is_none() && is_numeric_literal(left) {
                return self.finish_numeric_binary(operator, left, right, None, expectation);
            }
            let add_left = self.check_binary_left(left, left_type, TypeExpectation::none())?;
            if add_left == Type::String {
                self.check_expression(right, TypeExpectation::require(Type::String))?;
                return Ok(Type::String);
            }
            return self.finish_numeric_binary(operator, left, right, Some(add_left), expectation);
        }
        if operator.is_comparison() {
            return self.check_comparison(operator, left, right, left_type);
        }
        self.finish_numeric_binary(operator, left, right, left_type, expectation)
    }

    fn finish_numeric_binary(
        &mut self,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        left_type: Option<Type>,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        let value_type = if let Some(expected) = expectation.ty().filter(|ty| ty.is_numeric()) {
            self.check_binary_left(left, left_type, TypeExpectation::require(expected))?;
            self.check_expression(right, TypeExpectation::require(expected))?;
            expected
        } else if left_type.is_none() && is_numeric_literal(left) && !is_numeric_literal(right) {
            let right_type = self.check_expression(right, TypeExpectation::none())?;
            if !right_type.is_numeric() {
                return Err(SemanticError::NumericOperatorRequiresNumeric { span: right.span });
            }
            self.check_expression(left, TypeExpectation::hint(right_type))?;
            right_type
        } else {
            let left_type = self.check_binary_left(left, left_type, TypeExpectation::none())?;
            if !left_type.is_numeric() {
                return Err(SemanticError::NumericOperatorRequiresNumeric { span: left.span });
            }
            self.check_expression(right, TypeExpectation::require(left_type))?;
            left_type
        };

        if matches!(operator, BinaryOp::Divide | BinaryOp::Remainder)
            && value_type.is_integer()
            && constant_integer(right) == Some(0)
        {
            return Err(SemanticError::DivisionByZero { span: right.span });
        }
        if operator.is_bitwise() && !value_type.is_integer() {
            return Err(SemanticError::BitwiseOperatorRequiresInteger {
                span: left.span.merge(right.span),
            });
        }
        Ok(value_type)
    }

    pub(super) fn check_bit_not(
        &mut self,
        expression: &Expr,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        if matches!(expression.kind, ExprKind::Float(_))
            || expectation.ty().is_some_and(|ty| ty.is_float())
        {
            return Err(SemanticError::BitwiseOperatorRequiresInteger {
                span: expression.span,
            });
        }
        let value_type = if let Some(expected) = expectation.ty().filter(|ty| ty.is_integer()) {
            expected
        } else if is_numeric_literal(expression) {
            Type::I32
        } else {
            let value_type = self.check_expression(expression, TypeExpectation::none())?;
            if !value_type.is_integer() {
                return Err(SemanticError::BitwiseOperatorRequiresInteger {
                    span: expression.span,
                });
            }
            return Ok(value_type);
        };
        if let ExprKind::Integer(value) = expression.kind {
            check_positive_integer(value, value_type, expression.span)?;
            self.types.insert(expression.id, value_type);
        } else {
            self.check_expression(expression, TypeExpectation::require(value_type))?;
        }
        Ok(value_type)
    }

    fn check_binary_left(
        &mut self,
        left: &Expr,
        left_type: Option<Type>,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        let Some(left_type) = left_type else {
            return self.check_expression(left, expectation);
        };
        if expectation.is_required() {
            if let Some(expected) = expectation.ty() {
                if left_type != expected {
                    return Err(type_mismatch(expected, left_type, left.span));
                }
            }
        }
        Ok(left_type)
    }

    pub(super) fn check_comparison(
        &mut self,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        left_type: Option<Type>,
    ) -> Result<Type, SemanticError> {
        let ordered = matches!(
            operator,
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual
        );
        // A typed peer supplies missing Option/Result/List payload types.
        // This affects inference only; HIR still evaluates left before right.
        let (left_type, right_type) = if let Some(left_type) = left_type {
            let expectation = if !ordered
                && matches!(
                    left_type,
                    Type::Option(_) | Type::Result(_) | Type::Tuple(_) | Type::List(_)
                ) {
                TypeExpectation::hint(left_type)
            } else {
                TypeExpectation::none()
            };
            let right_type = self.check_expression(right, expectation)?;
            (left_type, right_type)
        } else if !ordered && needs_comparison_context(left) && !needs_comparison_context(right) {
            let right_type = self.check_expression(right, TypeExpectation::none())?;
            let left_type = self.check_expression(left, TypeExpectation::hint(right_type))?;
            (left_type, right_type)
        } else {
            let left_type = self.check_expression(left, TypeExpectation::none())?;
            let expectation = if !ordered
                && matches!(
                    left_type,
                    Type::Option(_) | Type::Result(_) | Type::Tuple(_) | Type::List(_)
                ) {
                TypeExpectation::hint(left_type)
            } else {
                TypeExpectation::none()
            };
            let right_type = self.check_expression(right, expectation)?;
            (left_type, right_type)
        };
        // `option == None` only reads the tag, so the payload needs no PartialEq.
        let compares_with_none = !ordered
            && matches!(left_type, Type::Option(_))
            && (is_none_constructor(left) || is_none_constructor(right));
        let comparable = if compares_with_none {
            true
        } else if ordered {
            self.implements_trait(left_type, "PartialOrd")
        } else {
            self.implements_trait(left_type, "PartialEq")
        };
        if left_type != right_type || !comparable {
            return Err(if ordered {
                SemanticError::OrderedComparisonRequiresPartialOrd {
                    span: left.span.merge(right.span),
                }
            } else {
                SemanticError::ComparisonTypeMismatch {
                    span: left.span.merge(right.span),
                }
            });
        }
        Ok(Type::Bool)
    }
}

fn binary_left_expectation(operator: BinaryOp, expectation: TypeExpectation) -> TypeExpectation {
    if matches!(operator, BinaryOp::And | BinaryOp::Or) {
        TypeExpectation::require(Type::Bool)
    } else if operator.is_comparison() {
        TypeExpectation::none()
    } else if expectation.ty().is_some_and(|ty| ty.is_numeric()) {
        expectation
    } else {
        TypeExpectation::none()
    }
}

fn is_none_constructor(expression: &Expr) -> bool {
    matches!(&expression.kind, ExprKind::Name(name) if name == "None")
}

fn needs_comparison_context(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Name(name) => name == "None",
        ExprKind::Call {
            callee, arguments, ..
        } => match &callee.kind {
            ExprKind::Name(name) if name == "Some" => arguments
                .iter()
                .any(|argument| needs_comparison_context(&argument.value)),
            ExprKind::Name(name) => matches!(name.as_str(), "Ok" | "Err"),
            _ => false,
        },
        ExprKind::Tuple(elements) => elements.iter().any(needs_comparison_context),
        ExprKind::CollectionLiteral(crate::syntax::CollectionLiteral::List(elements)) => {
            elements.is_empty() || elements.iter().any(needs_comparison_context)
        }
        _ => false,
    }
}
