//! Shared control-flow typing and lowering. Abrupt exits have no normal value.
use super::*;

pub(super) fn unreachable_error(expr: &Expr) -> Error {
    type_error(
        &expr.span(),
        ErrorCode::UnreachableCode,
        "Unreachable expression after a control-flow exit",
    )
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
            Expr::Unsafe { exprs, .. } | Expr::Block { exprs, .. } => {
                let previous = self.unsafe_context;
                self.unsafe_context |= matches!(expr, Expr::Unsafe { .. });
                self.local_scopes.push(Vec::new());
                let flow = self
                    .sequence(
                        exprs,
                        expected,
                        &mut vars.clone(),
                        functions,
                        suspends,
                        types,
                    )
                    .and_then(|flow| match flow {
                        Flow::Value(value) if slot_type(&value).is_owner() => {
                            self.snapshot(&value).map(Flow::Value)
                        }
                        flow => Ok(flow),
                    });
                self.unsafe_context = previous;
                self.end_local_scope();
                flow
            }
            Expr::Sequence {
                exprs, trailing, ..
            } => {
                let flow = self.sequence(
                    exprs,
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

    fn check_return(&self, value: &Option<Slot>, span: &Span) -> Result<(), Error> {
        let actual = slot_type(value);
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
        exprs: &[Expr],
        expected: Option<&Type>,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let mut flow = Flow::Value(None);
        for (index, expr) in exprs.iter().enumerate() {
            if matches!(flow, Flow::Diverged) {
                return Err(unreachable_error(expr));
            }
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
        }
        Ok(flow)
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
                    let Flow::Value(value) = self.expr_expected(
                        value,
                        vars,
                        functions,
                        suspends,
                        types,
                        Some(&self.return_type.clone()),
                    )?
                    else {
                        return Ok(Flow::Diverged);
                    };
                    value
                } else {
                    None
                };
                self.check_return(&value, span)?;
                let exit = self.return_exit()?;
                if let (Some(dst), Some(src)) = (&self.result, value) {
                    self.emit(Op::Copy(dst.clone(), src));
                }
                self.emit(Op::Jump(exit));
                Ok(Flow::Diverged)
            }
            Expr::Break { span } | Expr::Continue { span } => {
                let keyword = if matches!(expr, Expr::Break { .. }) {
                    "break"
                } else {
                    "continue"
                };
                let targets = self.loops.last_mut().ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::InvalidControlFlow,
                        format!("{keyword} must be inside a while loop"),
                    )
                })?;
                let target = if matches!(expr, Expr::Break { .. }) {
                    targets.has_break = true;
                    targets.exit
                } else {
                    targets.header
                };
                let depth = targets.local_depth;
                self.end_locals_from(depth);
                self.emit(Op::Jump(target));
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
                self.emit(Op::Branch {
                    condition: test.unwrap(),
                    then_label: body_label,
                    else_label: exit,
                });
                // A normally completing condition may be false. Do not infer
                // divergence from constants or speculate about loop execution.
                self.emit(Op::Label(body_label));
                if matches!(
                    self.expr(body, &mut vars.clone(), functions, suspends, types)?,
                    Flow::Value(_)
                ) {
                    self.emit(Op::Jump(header));
                }
                self.emit(Op::Label(exit));
                self.loops.pop();
                Ok(Flow::Value(None))
            }
            Expr::If { .. } | Expr::Logical { .. } => {
                self.conditional_expr(expr, vars, functions, suspends, types, expected)
            }
            _ => unreachable!("expected a control-flow expression"),
        }
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
                span,
            } => {
                self.local_scopes.push(Vec::new());
                let test = self.expr(condition, &mut vars.clone(), functions, suspends, types)?;
                self.end_local_scope();
                let Flow::Value(test) = test else {
                    return Err(unreachable_error(then_branch));
                };
                if slot_type(&test) != Type::Bool {
                    return Err(type_error(
                        &condition.span(),
                        ErrorCode::TypeMismatch,
                        "if condition must be Bool",
                    ));
                }
                let then_label = self.label_count;
                let else_label = then_label + 1;
                let exit = then_label + 2;
                self.label_count += 3;
                self.emit(Op::Branch {
                    condition: test.unwrap(),
                    then_label,
                    else_label,
                });
                self.emit(Op::Label(then_label));
                let hint = else_branch
                    .as_ref()
                    .and_then(|other| self.binary_hint(then_branch, other, vars, functions, types));
                let aggregate_hint = if hint.is_none() && expected.is_none() {
                    self.aggregate_default_hint(then_branch, vars, functions, types)
                        .or_else(|| {
                            else_branch.as_ref().and_then(|other| {
                                self.aggregate_default_hint(other, vars, functions, types)
                            })
                        })
                } else {
                    None
                };
                let expected = hint.as_ref().or(expected).or(aggregate_hint.as_ref());
                let then_flow = self.expr_expected(
                    then_branch,
                    &mut vars.clone(),
                    functions,
                    suspends,
                    types,
                    expected,
                )?;
                let mut result = None;
                if let Flow::Value(value) = &then_flow {
                    if else_branch.is_none() && value.is_some() {
                        return Err(type_error(
                            &then_branch.span(),
                            ErrorCode::TypeMismatch,
                            "if without else must have a Unit then branch",
                        ));
                    }
                    result = self.snapshot(value)?;
                    self.emit(Op::Jump(exit));
                }
                self.emit(Op::Label(else_label));
                let else_flow = if let Some(branch) = else_branch {
                    self.expr_expected(
                        branch,
                        &mut vars.clone(),
                        functions,
                        suspends,
                        types,
                        expected,
                    )?
                } else {
                    Flow::Value(None)
                };
                if let Flow::Value(value) = &else_flow {
                    if let Flow::Value(then_value) = &then_flow {
                        if slot_type(then_value) != slot_type(value) {
                            return Err(type_error(
                                span,
                                ErrorCode::TypeMismatch,
                                format!(
                                    "if branches must have the same type, got {} and {}",
                                    slot_type(then_value),
                                    slot_type(value),
                                ),
                            ));
                        }
                        if let (Some(dst), Some(src)) = (&result, value) {
                            self.emit(Op::Copy(dst.clone(), src.clone()));
                        }
                    } else {
                        result = self.snapshot(value)?;
                    }
                }
                if matches!((&then_flow, &else_flow), (Flow::Diverged, Flow::Diverged)) {
                    Ok(Flow::Diverged)
                } else {
                    self.emit(Op::Label(exit));
                    Ok(Flow::Value(result))
                }
            }
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
