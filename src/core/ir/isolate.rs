//! A single local child provides an owning, drain-before-recovery failure boundary.
use super::*;

fn exits(expr: &Expr, loops: usize) -> Result<(), Error> {
    match expr {
        Expr::AnonymousFunction(_) | Expr::Branch { .. } => return Ok(()),
        Expr::Concurrent { branches, .. } => {
            for branch in branches.iter().filter(|b| !matches!(b, Expr::Block { .. })) {
                exits(branch, loops)?;
            }
            return Ok(());
        }
        Expr::Return { span, .. } | Expr::Break { span } | Expr::Continue { span }
            if matches!(expr, Expr::Return { .. }) || loops == 0 =>
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidControlFlow,
                "Control flow cannot leave isolate; use its final expression",
            ));
        }
        Expr::While {
            condition, body, ..
        } => {
            exits(condition, loops)?;
            return exits(body, loops + 1);
        }
        Expr::IndexedLoop(loop_) => return exits(&loop_.body, loops + 1),
        _ => {}
    }
    for child in closures::children(expr) {
        exits(child, loops)?;
    }
    Ok(())
}

impl FunctionPlan {
    pub(super) fn isolate_result(&self, value: &Type, types: &TypeRegistry) -> Result<Type, Error> {
        let context = self.generics.borrow();
        let helper = context
            .templates
            .get("joky/task.__isolate_success")
            .ok_or_else(|| {
                type_error(
                    &self.current_span,
                    ErrorCode::InvalidCall,
                    "isolate requires import joky/task",
                )
            })?;
        crate::core::types::substitute(
            &helper.ret_ty,
            &[(helper.type_params[0].id, value.clone())]
                .into_iter()
                .collect(),
            types,
        )
    }

    pub(super) fn isolate_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let Expr::Isolate { body, span } = expr else {
            unreachable!()
        };
        if !suspends {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "isolate must be inside an @suspends function",
            ));
        }
        exits(body, 0)?;
        let ret = expected
            .and_then(|ty| match ty {
                Type::Enum(e) if e.name == "joky/result.Result" => {
                    e.arguments.first()?.as_type().cloned()
                }
                _ => None,
            })
            .filter(|ty| self.isolate_result(ty, types).ok().as_ref() == expected)
            .or_else(|| self.bound_hint(body, vars, functions, types))
            .ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Cannot infer isolate result type",
                )
            })?;
        if ret.contains_references()
            || ret.contains_functions()
            || ret.contains_closures()
            || (ret.is_owner() && !ret.is_suspend_safe_owner())
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "isolate results must be reference-free values",
            ));
        }
        let result_ty = self.isolate_result(&ret, types)?;
        let (name, args, _) = self.nested_task(
            body,
            ("isolate branch", *span),
            Some(ret.clone()),
            vars,
            functions,
            types,
        )?;
        let result = (ret != Type::Unit)
            .then(|| self.slot(ret.clone()))
            .transpose()?;
        if let Some(value) = &result {
            if value.ty.is_place() {
                self.emit(Op::Place(PlaceOp::Allocate(value.clone())));
            }
        }
        let failure = self.slot(Type::UInt64)?;
        let done = self.slot(Type::Bool)?;
        let cleanup = result
            .as_ref()
            .filter(|s| s.ty.is_owner() && !s.ty.is_place())
            .map(|value| CleanupOp::Register {
                value: value.clone(),
                place: None,
                elements: vec![],
            });
        let state = self.state_count;
        self.state_count += 1;
        self.emit(Op::Concurrent {
            result: result.clone(),
            kind: ConcurrentKind::Isolate,
            failure: Some(failure.clone()),
            branches: vec![ConcurrentBranch {
                name,
                args,
                result: None,
                cleanup,
                indirect: None,
                done,
            }],
            state,
        });
        self.emit(Op::ConcurrentFinish);
        let mut local = vars.clone();
        local.insert(
            "$isolate.value".into(),
            Binding {
                slot: result,
                mutable: false,
                place: None,
            },
        );
        local.insert(
            "$isolate.failure".into(),
            Binding {
                slot: Some(failure),
                mutable: false,
                place: None,
            },
        );
        let ident = |name: &str| Expr::Ident {
            name: name.into(),
            span: *span,
        };
        let call = |name: &str, arg| Expr::Call {
            callee: format!("joky/task.__isolate_{name}"),
            type_args: vec![crate::core::types::GenericArgument::Type(ret.clone())],
            args: vec![arg],
            span: *span,
        };
        let scope = self.scope.clone();
        for name in ["joky/task.__isolate_success", "joky/task.__isolate_failure"] {
            std::sync::Arc::make_mut(&mut self.scope)
                .functions
                .insert(name.into(), name.into());
        }
        let flow = self.expr_expected(
            &Expr::If {
                condition: Box::new(Expr::BinOp {
                    op: BinOp::Eq,
                    lhs: Box::new(ident("$isolate.failure")),
                    rhs: Box::new(Expr::Literal {
                        literal: Literal::UInt64(0),
                        span: *span,
                    }),
                    span: *span,
                }),
                then_branch: Box::new(call("success", ident("$isolate.value"))),
                else_branch: Some(Box::new(call("failure", ident("$isolate.failure")))),
                span: *span,
            },
            &mut local,
            functions,
            suspends,
            types,
            Some(&result_ty),
        );
        self.scope = scope;
        flow
    }
}
