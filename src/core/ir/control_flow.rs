//! Shared control-flow typing and lowering. Abrupt exits have no normal value.
use super::*;

pub(super) fn is_true_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Literal {
            literal: Literal::Bool(true),
            ..
        }
    )
}

/// Hints conservatively count every break in this loop's lexical jump scope.
/// The checked walk records reachable breaks in `LoopTargets::has_break`.
pub(super) fn has_loop_break(body: &Expr) -> bool {
    let mut pending = vec![body];
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::Break { .. } => return true,
            Expr::While { .. } | Expr::AnonymousFunction(_) | Expr::Branch { .. } => {}
            Expr::Concurrent { branches, .. } => {
                pending.extend(branches.iter().filter(|b| !matches!(b, Expr::Block { .. })));
            }
            _ => pending.extend(closures::children(expr)),
        }
    }
    false
}

pub(super) fn unreachable_error(expr: &Expr) -> Error {
    type_error(
        &expr.span(),
        ErrorCode::UnreachableCode,
        "Unreachable expression after a control-flow exit",
    )
}

// Only diagnose calculations whose operands cannot have source-level effects.
// Fallible operations and postfix propagation can matter even without their value.
fn simple_calculation(expr: &Expr) -> bool {
    match expr {
        Expr::Ident { .. } | Expr::Literal { .. } => true,
        Expr::Field { base, .. } => simple_calculation(base),
        Expr::Unary { op, operand, .. } => {
            !matches!(op, UnaryOp::Unwrap | UnaryOp::Try) && simple_calculation(operand)
        }
        Expr::BinOp { lhs, op, rhs, .. } => {
            !op.can_fail() && simple_calculation(lhs) && simple_calculation(rhs)
        }
        Expr::Logical { lhs, rhs, .. } => simple_calculation(lhs) && simple_calculation(rhs),
        Expr::BinaryChain { first, rest, .. } => {
            simple_calculation(first)
                && rest
                    .iter()
                    .all(|(op, rhs)| !op.can_fail() && simple_calculation(rhs))
        }
        _ => false,
    }
}

impl FunctionPlan {
    pub(super) fn block_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        match expr {
            Expr::Graph { exprs, .. } => self.graph_expr(exprs, vars, functions, types),
            Expr::Unsafe { .. } | Expr::Block { .. } => {
                let previous = self.unsafe_context;
                self.unsafe_context |= matches!(expr, Expr::Unsafe { .. });
                self.local_scopes.push(Vec::new());
                let flow = self
                    .sequence(
                        expr,
                        expected,
                        &mut vars.clone(),
                        functions,
                        suspends,
                        types,
                    )
                    .and_then(|flow| match flow {
                        Flow::Value(value)
                            if slot_type(&value).is_owner()
                                || slot_type(&value).is_closure_value() =>
                        {
                            self.snapshot(&value).map(Flow::Value)
                        }
                        flow => Ok(flow),
                    });
                self.unsafe_context = previous;
                if let Ok(flow) = &flow {
                    self.escape_owner(flow);
                }
                self.end_local_scope();
                flow
            }
            Expr::Sequence { trailing, .. } => {
                let flow = self.sequence(
                    expr,
                    if *trailing { None } else { expected },
                    vars,
                    functions,
                    suspends,
                    types,
                )?;
                Ok(match flow {
                    Flow::Value(_) if *trailing => Flow::Value(None),
                    flow => flow,
                })
            }
            _ => unreachable!("only block forms use the structural planner"),
        }
    }

    pub(super) fn check_return(&mut self, value: &Option<Slot>, span: &Span) -> Result<(), Error> {
        let actual = slot_type(value);
        if self.infer_return {
            self.return_type = actual.clone();
            self.infer_return = false;
        }
        if actual != self.return_type {
            return Err(type_error(
                span,
                ErrorCode::TypeMismatch,
                format!(
                    "Return type mismatch: expected {}, got {actual}",
                    self.return_type,
                ),
            ));
        }
        Ok(())
    }

    fn return_exit(&mut self) -> Result<usize, Error> {
        if let Some(exit) = self.return_label {
            return Ok(exit);
        }
        let exit = self.label_count;
        self.label_count += 1;
        self.return_label = Some(exit);
        if self.return_type != Type::Unit && self.result.is_none() {
            self.result = Some(self.slot(self.return_type.clone())?);
        }
        Ok(exit)
    }

    pub(super) fn sequence(
        &mut self,
        container: &Expr,
        expected: Option<&Type>,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let (exprs, implicit) = match container {
            Expr::Block { exprs, .. } | Expr::Unsafe { exprs, .. } => (exprs, true),
            Expr::Sequence { exprs, .. } => (exprs, false),
            _ => unreachable!("expected a block or sequence"),
        };
        let mut flow = Flow::Value(None);
        for (index, expr) in exprs.iter().enumerate() {
            if matches!(flow, Flow::Diverged) {
                return Err(unreachable_error(expr));
            }
            let start = self.ops.len();
            flow = self.expr_expected(
                expr,
                vars,
                functions,
                suspends,
                types,
                if index + 1 == exprs.len() {
                    expected
                } else {
                    None
                },
            )?;
            if implicit
                && index + 1 < exprs.len()
                && matches!(&flow, Flow::Value(Some(slot)) if !slot.ty.is_owner())
            {
                self.check_implicit_discard(expr, start)?;
            }
        }
        Ok(flow)
    }

    fn check_implicit_discard(&self, mut tail: &Expr, start: usize) -> Result<(), Error> {
        while let Expr::Sequence {
            exprs,
            trailing: false,
            ..
        } = tail
        {
            let Some(last) = exprs.last() else {
                return Ok(());
            };
            tail = last;
        }
        if matches!(tail, Expr::Ident { .. }) || !simple_calculation(tail) {
            return Ok(());
        }
        // Operator syntax can dispatch to user-defined Add. Earlier calls in
        // the same semicolon sequence do not consume its final calculation.
        let span = tail.span();
        if self.ops[start..]
            .iter()
            .zip(&self.op_spans[start..])
            .any(|(op, site)| {
                matches!(op, Op::Call { .. })
                    && span.source_id() == site.source_id()
                    && span.contains(site.start())
            })
        {
            return Ok(());
        }
        let message = match tail {
            Expr::Unary { op: UnaryOp::Neg, .. }
            | Expr::Literal { literal: Literal::Integer { negative: true, .. }, .. } =>
                "Unused calculation after a newline. For subtraction, put '-' at the end of the preceding line. To discard the value, use 'let _ =' or ';'.",
            _ => "Unused calculation before a newline. Use 'let _ =' or ';' to discard the value explicitly.",
        };
        Err(type_error(&span, ErrorCode::UnusedValue, message))
    }

    /// Share the function exit and scope-join path with postfix propagation.
    pub(super) fn return_value(&mut self, value: Option<Slot>, span: Span) -> Result<Flow, Error> {
        self.check_return(&value, &span)?;
        let exit = self.return_exit()?;
        if let (Some(dst), Some(src)) = (&self.result, value) {
            self.emit(Op::Copy(dst.clone(), src));
        }
        if self.task_scopes.is_empty() {
            self.end_locals_from(0);
            self.emit(Op::Jump(exit));
        } else {
            // Return values are already saved; every crossed scope
            // joins and cleans up before the function exit runs.
            let target = self.route_task_scope_exit(exit, 0, 0);
            self.emit(Op::Jump(target));
        }
        Ok(Flow::Diverged)
    }

    pub(super) fn control_flow_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        match expr {
            Expr::Return { value, span } => {
                let value = if let Some(value) = value {
                    let expected = (!self.infer_return).then(|| self.return_type.clone());
                    let Flow::Value(value) = self.expr_expected(
                        value,
                        vars,
                        functions,
                        suspends,
                        types,
                        expected.as_ref(),
                    )?
                    else {
                        return Ok(Flow::Diverged);
                    };
                    value
                } else {
                    None
                };
                self.return_value(value, *span)
            }
            Expr::Break { span } | Expr::Continue { span } => {
                let keyword = if matches!(expr, Expr::Break { .. }) {
                    "break"
                } else {
                    "continue"
                };
                let (target, depth, loop_scope_depth) = {
                    let targets = self.loops.last_mut().ok_or_else(|| {
                        type_error(
                            span,
                            ErrorCode::InvalidControlFlow,
                            format!("{keyword} must be inside a while loop"),
                        )
                    })?;
                    if matches!(expr, Expr::Break { .. }) {
                        targets.has_break = true;
                    }
                    (
                        if matches!(expr, Expr::Break { .. }) {
                            targets.exit
                        } else {
                            targets.header
                        },
                        targets.local_depth,
                        targets.scope_depth,
                    )
                };
                if self.task_scopes.len() > loop_scope_depth {
                    // The jump leaves scopes opened inside the loop; each one
                    // joins and ends its locals before the jump completes.
                    let first = self.route_task_scope_exit(target, depth, loop_scope_depth);
                    self.emit(Op::Jump(first));
                } else {
                    self.end_locals_from(depth);
                    self.emit(Op::Jump(target));
                }
                Ok(Flow::Diverged)
            }
            Expr::While {
                condition, body, ..
            } => {
                let header = self.label_count;
                let body_label = header + 1;
                let exit = header + 2;
                self.label_count += 3;
                self.loops.push(LoopTargets {
                    header,
                    exit,
                    has_break: false,
                    local_depth: self.local_scopes.len(),
                    scope_depth: self.task_scopes.len(),
                });
                self.emit(Op::Label(header));
                self.local_scopes.push(Vec::new());
                let test = self.expr(condition, &mut vars.clone(), functions, suspends, types)?;
                self.end_local_scope();
                let Flow::Value(test) = test else {
                    if let Expr::Block { exprs, .. } = body.as_ref() {
                        if let Some(first) = exprs.first() {
                            return Err(unreachable_error(first));
                        }
                    } else {
                        return Err(unreachable_error(body));
                    }
                    let targets = self.loops.pop().unwrap();
                    return if targets.has_break {
                        self.emit(Op::Label(exit));
                        Ok(Flow::Value(None))
                    } else {
                        Ok(Flow::Diverged)
                    };
                };
                if slot_type(&test) != Type::Bool {
                    return Err(type_error(
                        &condition.span(),
                        ErrorCode::TypeMismatch,
                        "while condition must be Bool",
                    ));
                }
                let unconditional = is_true_literal(condition);
                if unconditional {
                    self.emit(Op::Jump(body_label));
                } else {
                    self.emit(Op::Branch {
                        condition: test.unwrap(),
                        then_label: body_label,
                        else_label: exit,
                    });
                }
                self.emit(Op::Label(body_label));
                if matches!(
                    self.expr(body, &mut vars.clone(), functions, suspends, types)?,
                    Flow::Value(_)
                ) {
                    self.emit(Op::Jump(header));
                }
                let targets = self.loops.pop().unwrap();
                if unconditional && !targets.has_break {
                    Ok(Flow::Diverged)
                } else {
                    self.emit(Op::Label(exit));
                    Ok(Flow::Value(None))
                }
            }
            Expr::If { .. } | Expr::IfChain { .. } | Expr::Logical { .. } => {
                self.conditional_expr(expr, vars, functions, suspends, types, expected)
            }
            _ => unreachable!("expected a control-flow expression"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn if_branches<'a>(
        &mut self,
        arms: impl Iterator<Item = (&'a Expr, &'a Expr)> + Clone,
        fallback: Option<&'a Expr>,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let values = arms.clone().map(|(_, body)| body).chain(fallback);
        let hint = fallback.and_then(|_| {
            values
                .clone()
                .find_map(|body| self.type_hint(body, vars, functions, types))
        });
        let aggregate = if hint.is_none() && expected.is_none() {
            values
                .clone()
                .find_map(|body| self.aggregate_default_hint(body, vars, functions, types))
        } else {
            None
        };
        let expected = if expected
            .is_some_and(|ty| ty.dyn_interface().is_some() || arrays::element_type(ty).is_some())
        {
            expected
        } else {
            hint.as_ref().or(expected).or(aggregate.as_ref())
        };
        let exit = self.label_count;
        self.label_count += 1;
        let mut result = Flow::Diverged;
        for (condition, body) in arms {
            self.local_scopes.push(Vec::new());
            let test = self.expr(condition, &mut vars.clone(), functions, suspends, types)?;
            self.end_local_scope();
            let Flow::Value(test) = test else {
                return Err(unreachable_error(body));
            };
            if slot_type(&test) != Type::Bool {
                return Err(type_error(
                    &condition.span(),
                    ErrorCode::TypeMismatch,
                    "if condition must be Bool",
                ));
            }
            let then_label = self.label_count;
            let next = then_label + 1;
            self.label_count += 2;
            self.emit(Op::Branch {
                condition: test.unwrap(),
                then_label,
                else_label: next,
            });
            self.emit(Op::Label(then_label));
            if let Flow::Value(value) = self.expr_expected(
                body,
                &mut vars.clone(),
                functions,
                suspends,
                types,
                expected,
            )? {
                if fallback.is_none() && value.is_some() {
                    return Err(type_error(
                        &body.span(),
                        ErrorCode::TypeMismatch,
                        "if without else must have a Unit then branch",
                    ));
                }
                self.merge_if_value(&mut result, &value)?;
                self.emit(Op::Jump(exit));
            }
            self.emit(Op::Label(next));
        }
        if let Some(other) = fallback {
            if let Flow::Value(value) = self.expr_expected(
                other,
                &mut vars.clone(),
                functions,
                suspends,
                types,
                expected,
            )? {
                self.merge_if_value(&mut result, &value)?;
            }
        } else {
            result = Flow::Value(None);
        }
        let Flow::Value(mut result) = result else {
            return Ok(Flow::Diverged);
        };
        self.emit(Op::Label(exit));
        if result
            .as_ref()
            .is_some_and(|s| s.ty.reference_type().is_some() && s.ty.dyn_interface().is_some())
        {
            result = self.reference_value(result, false)?;
        }
        Ok(Flow::Value(result))
    }

    fn merge_if_value(&mut self, result: &mut Flow, value: &Option<Slot>) -> Result<(), Error> {
        if let Flow::Value(existing) = result {
            if slot_type(existing) != slot_type(value) {
                return Err(type_error(
                    &self.current_span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "if branches must have the same type, got {} and {}",
                        slot_type(existing),
                        slot_type(value)
                    ),
                ));
            }
            if let (Some(dst), Some(src)) = (existing, value) {
                self.emit(Op::Copy(dst.clone(), src.clone()));
            }
        } else {
            *result = Flow::Value(self.snapshot(value)?);
        }
        Ok(())
    }

    fn conditional_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        match expr {
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => self.if_branches(
                std::iter::once((condition.as_ref(), then_branch.as_ref())),
                else_branch.as_deref(),
                vars,
                functions,
                suspends,
                types,
                expected,
            ),
            Expr::IfChain { arms, fallback, .. } => self.if_branches(
                arms.iter().map(|(test, body)| (test, body)),
                fallback.as_deref(),
                vars,
                functions,
                suspends,
                types,
                expected,
            ),
            Expr::Logical { op, lhs, rhs, .. } => {
                let Flow::Value(left) = self.expr(lhs, vars, functions, suspends, types)? else {
                    return Err(unreachable_error(rhs));
                };
                if slot_type(&left) != Type::Bool {
                    return Err(type_error(
                        &lhs.span(),
                        ErrorCode::TypeMismatch,
                        "Logical operand must be Bool",
                    ));
                }
                let result = self.snapshot(&left)?;
                let right_label = self.label_count;
                let exit = right_label + 1;
                self.label_count += 2;
                let (then_label, else_label) = match op {
                    LogicalOp::And => (right_label, exit),
                    LogicalOp::Or => (exit, right_label),
                };
                self.emit(Op::Branch {
                    condition: left.unwrap(),
                    then_label,
                    else_label,
                });
                self.emit(Op::Label(right_label));
                self.local_scopes.push(Vec::new());
                let right = self.expr(rhs, &mut vars.clone(), functions, suspends, types)?;
                self.end_local_scope();
                if let Flow::Value(right) = right {
                    if slot_type(&right) != Type::Bool {
                        return Err(type_error(
                            &rhs.span(),
                            ErrorCode::TypeMismatch,
                            "Logical operand must be Bool",
                        ));
                    }
                    self.emit(Op::Copy(result.as_ref().unwrap().clone(), right.unwrap()));
                }
                // The short-circuit path always supplies a normal Bool value.
                self.emit(Op::Label(exit));
                Ok(Flow::Value(result))
            }
            _ => unreachable!("expected a conditional expression"),
        }
    }
}
