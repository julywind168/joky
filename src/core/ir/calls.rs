//! Function calls and integer conversion intrinsics, checked before backend lowering.
use super::*;

impl FunctionPlan {
    /// Shared call-argument pipeline: arity check, per-argument expected-type
    /// checking, mismatch rejection, and argument extension. `later(index)`
    /// supplies the next expression that would become unreachable when
    /// argument `index` diverges; `Ok(None)` means the divergence is terminal
    /// (no later expression exists) and the caller returns `Flow::Diverged`.
    #[allow(clippy::too_many_arguments)]
    fn checked_arguments<'a>(
        &mut self,
        callee: &str,
        args: &'a [Expr],
        params: &[Type],
        span: Span,
        later: impl Fn(usize) -> Option<&'a Expr>,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Option<Vec<Option<Slot>>>, Error> {
        if params.len() != args.len() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                format!("Argument count mismatch for `{callee}`"),
            ));
        }
        let mut values = Vec::with_capacity(args.len());
        for (index, (arg, ty)) in args.iter().zip(params).enumerate() {
            let Flow::Value(value) =
                self.expr_expected(arg, vars, functions, suspends, types, Some(ty))?
            else {
                if let Some(next) = later(index) {
                    return Err(unreachable_error(next));
                }
                return Ok(None);
            };
            if &slot_type(&value) != ty {
                return Err(type_error(
                    &arg.span(),
                    ErrorCode::TypeMismatch,
                    format!("Argument type mismatch for `{callee}`"),
                ));
            }
            let value = self.reference_value(value, false)?;
            extend_argument(&mut values, value);
        }
        Ok(Some(values))
    }

    pub(super) fn call_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Expr::Call {
            callee,
            type_args,
            args,
            span,
        } = expr
        else {
            unreachable!()
        };
        if !functions.get(callee).is_some_and(|f| f.internal) {
            self.check_call_root(callee, vars, span)?;
        }
        if matches!(callee.as_str(), "Ptr.null" | "Ptr.is_null") {
            if !type_args.is_empty() {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    "Ptr intrinsics do not accept type arguments",
                ));
            }
            let params = if callee == "Ptr.null" {
                vec![]
            } else {
                vec![Type::Ptr]
            };
            let Some(values) = self.checked_arguments(
                callee,
                args,
                &params,
                *span,
                |i| args.get(i + 1),
                vars,
                functions,
                suspends,
                types,
            )?
            else {
                return Ok(Flow::Diverged);
            };
            let null = self.slot(Type::Ptr)?;
            self.emit(Op::Literal(null.clone(), Constant::NullPtr));
            if callee == "Ptr.null" {
                return Ok(Flow::Value(Some(null)));
            }
            let dst = self.slot(Type::Bool)?;
            self.emit(Op::Binary(
                dst.clone(),
                BinOp::Eq,
                values[0].clone().unwrap(),
                null,
            ));
            return Ok(Flow::Value(Some(dst)));
        }
        if let Some(target) = IntegerType::conversion(callee) {
            if !type_args.is_empty() {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    "Integer conversions do not accept explicit type arguments",
                ));
            }
            let [arg] = args.as_slice() else {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    format!("Integer conversion `{callee}` expects exactly one argument"),
                ));
            };
            // The target is not an expected source type. Unconstrained
            // integer literals still default to Int32.
            let Flow::Value(value) = self.expr(arg, vars, functions, suspends, types)? else {
                return Ok(Flow::Diverged);
            };
            let source = slot_type(&value);
            if source.integer().is_none() {
                return Err(type_error(
                    &arg.span(),
                    ErrorCode::TypeMismatch,
                    format!("Integer conversion `{callee}` requires an integer, got {source}"),
                ));
            }
            let dst = self.slot(target.ty())?;
            self.emit(Op::Convert(dst.clone(), value.unwrap()));
            return Ok(Flow::Value(Some(dst)));
        }
        let (name, info) = self.resolve_callee(expr, vars, functions, types)?;
        if info.c_abi && !self.unsafe_context {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("Call to C import {callee} requires an unsafe block"),
            ));
        }
        if info.suspends && !suspends {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("Caller of `{callee}` must be marked @suspends"),
            ));
        }
        let Some(values) = self.checked_arguments(
            callee,
            args,
            &info.signature.params,
            *span,
            |index| args.get(index + 1),
            vars,
            functions,
            suspends,
            types,
        )?
        else {
            return Ok(Flow::Diverged);
        };
        let result = if info.signature.ret == Type::Unit {
            None
        } else {
            Some(self.slot(info.signature.ret.clone())?)
        };
        let state = info.suspends.then(|| {
            let state = self.state_count;
            self.state_count += 1;
            state
        });
        self.emit(Op::Call {
            name,
            args: values,
            result: result.clone(),
            state,
        });
        Ok(Flow::Value(result))
    }
}

impl FunctionPlan {
    pub(super) fn scope_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Expr::Scope {
            kind,
            branches,
            span,
        } = expr
        else {
            unreachable!()
        };
        let keyword = kind.keyword();
        if !suspends {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("{keyword} must be inside an @suspends function"),
            ));
        }
        if branches.is_empty() {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("{keyword} requires at least one branch"),
            ));
        }
        let mut planned = Vec::with_capacity(branches.len());
        for (branch_index, source_branch) in branches.iter().enumerate() {
            let resolved = self.member_call(source_branch, vars, functions, types)?;
            let branch = resolved.as_ref().unwrap_or(source_branch);
            let Expr::Call {
                callee,
                args,
                span: branch_span,
                ..
            } = branch
            else {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    format!("{keyword} branches must be function calls"),
                ));
            };
            if !functions.get(callee).is_some_and(|f| f.internal) {
                self.check_call_root(callee, vars, branch_span)?;
            }
            if IntegerType::conversion(callee).is_some() {
                return Err(type_error(
                    branch_span,
                    ErrorCode::InvalidCall,
                    format!("{keyword} branches must be @suspends functions returning Unit"),
                ));
            }
            let (name, info) = self.resolve_callee(branch, vars, functions, types)?;
            if !info.suspends || info.signature.ret != Type::Unit {
                return Err(type_error(
                    branch_span,
                    ErrorCode::InvalidCall,
                    format!(
                        "{keyword} branch `{callee}` must be an @suspends function returning Unit"
                    ),
                ));
            }
            let Some(values) = self.checked_arguments(
                callee,
                args,
                &info.signature.params,
                *branch_span,
                |index| {
                    args.get(index + 1)
                        .or_else(|| branches.get(branch_index + 1))
                },
                vars,
                functions,
                suspends,
                types,
            )?
            else {
                return Ok(Flow::Diverged);
            };
            let done = self.slot(Type::Bool)?;
            planned.push(ScopeBranch {
                name,
                args: values,
                done,
            });
        }
        let state = self.state_count;
        self.state_count += 1;
        self.emit(Op::Scope {
            kind: *kind,
            branches: planned,
            state,
        });
        Ok(Flow::Value(None))
    }
}
