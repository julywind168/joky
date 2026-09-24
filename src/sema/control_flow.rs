use crate::diagnostic::SemanticError;
use crate::syntax::Expr;

use super::checker::Checker;
use super::expectation::TypeExpectation;
use super::types::Type;
use super::validation::type_mismatch;

impl Checker {
    pub(super) fn check_loop(
        &mut self,
        body: &Expr,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        self.loop_break_types.push(expectation.ty());
        self.check_expression(body, TypeExpectation::none())?;
        let result = self
            .loop_break_types
            .pop()
            .expect("loop context was pushed above")
            .unwrap_or(Type::Unit);
        if expectation.is_required() {
            if let Some(expected) = expectation.ty() {
                if expected != result {
                    return Err(type_mismatch(expected, result, body.span));
                }
            }
        }
        Ok(result)
    }

    pub(super) fn check_if(
        &mut self,
        condition: &Expr,
        then_branch: &Expr,
        else_branch: &Expr,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        use crate::syntax::ExprKind;
        // Synthesized implicit else: empty block whose span equals the then branch
        let implicit_else = matches!(&else_branch.kind, ExprKind::Block(stmts) if stmts.is_empty())
            && else_branch.span == then_branch.span;

        self.check_expression(condition, TypeExpectation::require(Type::Bool))?;
        let then_type = self.check_expression(then_branch, expectation)?;

        // With an implicit else the then branch must be Unit or diverge,
        // otherwise report a dedicated error
        if implicit_else && !does_not_complete(then_branch) && then_type != Type::Unit {
            return Err(SemanticError::IfMissingElse {
                then_type: super::types::type_name(then_type).to_owned(),
                span: then_branch.span,
            });
        }

        let else_expectation = if does_not_complete(then_branch) {
            expectation
        } else {
            TypeExpectation::require(then_type)
        };
        let else_type = self.check_expression(else_branch, else_expectation)?;
        Ok(if does_not_complete(then_branch) {
            else_type
        } else {
            then_type
        })
    }
}

impl Checker {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_for(
        &mut self,
        index: Option<&str>,
        item: &str,
        iterable: &Expr,
        body: &Expr,
        limit: Option<&Expr>,
        expectation: TypeExpectation,
        span: crate::Span,
    ) -> Result<Type, SemanticError> {
        if limit.is_some() {
            self.check_mutable_captures(
                body,
                std::iter::once(item.to_owned()).chain(index.map(str::to_owned)),
                "parallel for",
            )?;
        }
        let input = self.check_expression(iterable, TypeExpectation::none())?;
        let input = if self.implements_trait(input, "Cursor") {
            input
        } else if let Some(cursor) = self.adapt_into_cursor(input) {
            self.for_into_cursor.insert(iterable.id, cursor);
            cursor
        } else {
            return Err(SemanticError::FunctionNotSupported {
                name: "for requires a List, a Cursor implementation, or an IntoCursor container"
                    .to_owned(),
                span: iterable.span,
            });
        };
        if limit.is_some() && !matches!(input, Type::List(_)) {
            return Err(SemanticError::FunctionNotSupported {
                name: "parallel for requires a List input".into(),
                span: iterable.span,
            });
        }
        let result = if matches!(
            input,
            Type::Param(_)
                | Type::List(_)
                | Type::BytesCursor
                | Type::MapCursor(_)
                | Type::MapKeyCursor(_)
                | Type::MapValueCursor(_)
                | Type::MutListCursor(_)
                | Type::MutMapCursor(_)
                | Type::MutSetCursor(_)
        ) {
            self.check_bound_method(input, "Cursor", "advance", &[], iterable.span)?
        } else {
            let method = self
                .nominal_methods(input)
                .and_then(|methods| methods.get(super::CURSOR_METHOD))
                .expect("checked Cursor implementation")
                .clone();
            self.current_effects.extend(&method.used_effects);
            method.return_type
        };
        let Type::Option(option) = result else {
            unreachable!("Cursor return type")
        };
        let Type::Tuple(pair) = self.option_types[option] else {
            unreachable!("Cursor pair")
        };
        let element = self.tuple_types[pair][0];
        if index == Some(item) {
            return Err(SemanticError::FunctionNotSupported {
                name: "index and item bindings must be distinct".to_owned(),
                span,
            });
        }
        if let Some(limit) = limit {
            self.check_expression(limit, TypeExpectation::require(Type::U64))?;
            if matches!(limit.kind, crate::syntax::ExprKind::Integer(0)) {
                return Err(SemanticError::FunctionNotSupported {
                    name: "parallel limit must be greater than zero".to_owned(),
                    span: limit.span,
                });
            }
        }
        reject_for_control(body, limit.is_some())?;
        super::checker::intern_option(&mut self.option_types, element);
        super::checker::intern_tuple(&mut self.tuple_types, vec![Type::U64, input]);
        let expected = expectation.ty().and_then(|ty| match ty {
            Type::List(id) => Some(self.list_types[id]),
            _ => None,
        });
        self.scopes.push(std::collections::HashMap::new());
        self.bind(item.to_owned(), element);
        if let Some(index) = index {
            self.bind(index.to_owned(), Type::U64);
        }
        self.loop_break_types.push(Some(Type::Unit));
        if limit.is_some() {
            self.return_types.push(Type::Unit);
        }
        let result = self.check_expression(body, TypeExpectation::from_option(expected));
        if limit.is_some() {
            self.return_types.pop();
        }
        self.loop_break_types.pop();
        self.scopes.pop();
        let result = result?;
        if limit.is_some() {
            for (name, capture_span) in super::free_names::free_names(
                body,
                std::iter::once(item.to_owned()).chain(index.map(str::to_owned)),
            ) {
                if self.lookup(&name).is_some_and(|binding| {
                    binding.type_value.is_none() && self.is_owned_capture_type(binding.value_type)
                }) {
                    return Err(SemanticError::FunctionNotSupported { name: format!("parallel for cannot capture owned or borrowed local '{name}'; create it inside the iteration or use a Cown"), span: capture_span });
                }
            }
        }
        // A Unit body is a statement loop unless the surrounding context
        // asked for a List, in which case `()` / `continue` still collect.
        if expected.is_none() && result == Type::Unit {
            if limit.is_some() {
                super::checker::intern_list(&mut self.list_types, Type::Unit);
            }
            return Ok(Type::Unit);
        }
        self.check_list_element(result, body.span)?;
        Ok(Type::List(super::checker::intern_list(
            &mut self.list_types,
            result,
        )))
    }

    fn adapt_into_cursor(&mut self, input: Type) -> Option<Type> {
        let intern_pair =
            |tuples: &mut Vec<Vec<Type>>, options: &mut Vec<Type>, item: Type, cursor: Type| {
                super::checker::intern_option(
                    options,
                    Type::Tuple(super::checker::intern_tuple(tuples, vec![item, cursor])),
                );
                cursor
            };
        match input {
            Type::Map(id) => {
                let info = self.maps[id];
                let item = Type::Tuple(super::checker::intern_tuple(
                    &mut self.tuple_types,
                    vec![info.key, info.value],
                ));
                Some(intern_pair(
                    &mut self.tuple_types,
                    &mut self.option_types,
                    item,
                    Type::MapCursor(id),
                ))
            }
            Type::MutMap(id) => {
                let info = self.maps[id];
                let item = Type::Tuple(super::checker::intern_tuple(
                    &mut self.tuple_types,
                    vec![info.key, info.value],
                ));
                Some(intern_pair(
                    &mut self.tuple_types,
                    &mut self.option_types,
                    item,
                    Type::MutMapCursor(id),
                ))
            }
            Type::MutSet(id) => Some(intern_pair(
                &mut self.tuple_types,
                &mut self.option_types,
                self.maps[id].key,
                Type::MutSetCursor(id),
            )),
            Type::MutList(id) => Some(intern_pair(
                &mut self.tuple_types,
                &mut self.option_types,
                self.list_types[id],
                Type::MutListCursor(id),
            )),
            _ => None,
        }
    }
}

fn reject_for_control(expression: &Expr, parallel: bool) -> Result<(), SemanticError> {
    use crate::syntax::{ExprKind, ExprVisitor};
    struct Check {
        depth: usize,
        parallel: bool,
        error: Option<SemanticError>,
    }
    impl ExprVisitor for Check {
        type Output = ();
        fn visit_expr(&mut self, expr: &Expr) {
            if self.error.is_some() {
                return;
            }
            match &expr.kind {
                ExprKind::Closure { .. } => {}
                ExprKind::Break { value }
                    if self.depth == 0 && (self.parallel || value.is_some()) =>
                {
                    self.error = Some(SemanticError::FunctionNotSupported {
                        name: if self.parallel {
                            "break cannot exit a parallel for; use a sequential for"
                        } else {
                            "for only supports bare break, without a value"
                        }
                        .to_owned(),
                        span: expr.span,
                    });
                }
                ExprKind::While { condition, body } => {
                    self.visit_expr(condition);
                    self.depth += 1;
                    self.visit_expr(body);
                    self.depth -= 1;
                }
                ExprKind::Loop { body } => {
                    self.depth += 1;
                    self.visit_expr(body);
                    self.depth -= 1;
                }
                ExprKind::For {
                    iterable,
                    body,
                    limit,
                    ..
                } => {
                    self.visit_expr(iterable);
                    if let Some(limit) = limit {
                        self.visit_expr(limit);
                    }
                    self.depth += 1;
                    self.visit_expr(body);
                    self.depth -= 1;
                }
                _ => crate::syntax::walk_expr(self, expr),
            }
        }
        fn default_output(&self) {}
    }
    let mut check = Check {
        depth: 0,
        parallel,
        error: None,
    };
    check.visit_expr(expression);
    match check.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Branches that transfer loop control contribute no value to their parent.
/// This is deliberately structural; no new public Never type is introduced.
pub(super) fn does_not_complete(expression: &Expr) -> bool {
    use crate::syntax::ExprKind;
    match &expression.kind {
        ExprKind::Break { .. } | ExprKind::Continue => true,
        ExprKind::Block(expressions) => expressions.iter().any(does_not_complete),
        ExprKind::If {
            then_branch,
            else_branch,
            ..
        } => does_not_complete(then_branch) && does_not_complete(else_branch),
        ExprKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|arm| does_not_complete(&arm.value))
        }
        _ => false,
    }
}
