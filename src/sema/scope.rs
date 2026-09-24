//! Lexical scope management during semantic analysis

use std::collections::HashMap;

use crate::diagnostic::SemanticError;
use crate::syntax::{Expr, ExprKind};

use super::checker::Checker;
use super::expectation::TypeExpectation;
use super::types::{type_name, Type};

#[derive(Clone, Copy)]
pub(super) struct Binding {
    pub(super) value_type: Type,
    pub(super) type_value: Option<Type>,
    pub(super) mutable: bool,
    pub(super) declaration: Option<crate::syntax::NodeId>,
}

impl Checker {
    pub(super) fn check_block(
        &mut self,
        expressions: &[Expr],
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        self.scopes.push(HashMap::new());
        let result = (|| {
            if let Some((last, preceding)) = expressions.split_last() {
                for expression in preceding {
                    // Non-last expressions are statements: they must be Unit,
                    // except `echo`, which observes a value without consuming it.
                    let ty = self.check_expression(expression, TypeExpectation::none())?;
                    if ty != Type::Unit && !is_echo(expression) {
                        return Err(SemanticError::UnusedValue {
                            actual: type_name(ty),
                            span: expression.span,
                        });
                    }
                }
                self.check_expression(last, expectation)
            } else {
                Ok(Type::Unit)
            }
        })();
        self.scopes.pop();
        result
    }

    pub(super) fn bind(&mut self, name: String, value_type: Type) {
        self.scopes
            .last_mut()
            .expect("a checker always has a scope")
            .insert(
                name,
                Binding {
                    value_type,
                    type_value: None,
                    mutable: false,
                    declaration: None,
                },
            );
    }

    pub(super) fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    pub(super) fn bind_local_value(
        &mut self,
        name: String,
        value_type: Type,
        declaration: crate::syntax::NodeId,
        mutable: bool,
    ) {
        self.scopes.last_mut().expect("local scope").insert(
            name,
            Binding {
                value_type,
                type_value: None,
                mutable,
                declaration: Some(declaration),
            },
        );
    }

    pub(super) fn check_mutable_captures(
        &self,
        expression: &Expr,
        bound: impl IntoIterator<Item = String>,
        boundary: &str,
    ) -> Result<(), SemanticError> {
        for (name, span) in super::free_names::free_names(expression, bound) {
            if self.lookup(&name).is_some_and(|binding| binding.mutable) {
                return Err(SemanticError::MutableCapture {
                    name,
                    boundary: boundary.to_owned(),
                    span,
                });
            }
        }
        Ok(())
    }

    pub(super) fn bind_type(&mut self, name: String, value: Type) {
        self.scopes
            .last_mut()
            .expect("a checker always has a type scope")
            .insert(
                name,
                Binding {
                    value_type: Type::Unit,
                    type_value: Some(value),
                    mutable: false,
                    declaration: None,
                },
            );
    }
}

fn is_echo(expression: &Expr) -> bool {
    matches!(
        &expression.kind,
        ExprKind::Call { callee, .. }
            if matches!(&callee.kind, ExprKind::Name(name) if name == "echo")
    )
}
