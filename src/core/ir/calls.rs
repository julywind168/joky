//! Function calls and integer conversion intrinsics, checked before backend lowering.
use super::*;

impl FunctionPlan {
    /// Shared call-argument pipeline: arity check, per-argument expected-type
    /// checking, mismatch rejection, and argument extension. `later(index)`
    /// supplies the next expression that would become unreachable when
    /// argument `index` diverges; `Ok(None)` means the divergence is terminal
    /// (no later expression exists) and the caller returns `Flow::Diverged`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn checked_arguments<'a>(
        &mut self,
        callee: &str,
        args: &'a [Expr],
        params: &[Type],
        contract: Option<&FunctionInfo>,
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
            let automatic = contract.is_some_and(|info| {
                !info.suspends && (info.noescape || info.noescape_params.contains(&index))
            }) && ty.reference_type().is_some_and(|r| !r.mutable)
                && !self
                    .type_hint(arg, vars, functions, types)
                    .is_some_and(|ty| ty.reference_type().is_some());
            let flow = if automatic {
                self.shared_argument(arg, ty, vars, functions, suspends, types)?
            } else {
                self.expr_expected(arg, vars, functions, suspends, types, Some(ty))?
            };
            let Flow::Value(value) = flow else {
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
        // Argument temporaries live through the complete call, including later
        // argument evaluation, and end before the caller's next expression.
        self.local_scopes.push(Vec::new());
        self.argument_scopes.push(Default::default());
        let flow = self.call_expr_inner(expr, vars, functions, suspends, types)?;
        let temporary = self.argument_scopes.pop().unwrap();
        let locals = self.local_scopes.pop().unwrap();
        // Ordinary bindings introduced by argument sequences belong to the
        // caller's lexical scope. Only compiler-authored temporaries end here.
        for local in locals
            .iter()
            .filter(|local| !temporary.contains(&local.index))
        {
            self.local_scopes.last_mut().unwrap().push(local.clone());
        }
        for local in locals
            .into_iter()
            .rev()
            .filter(|local| temporary.contains(&local.index))
        {
            self.emit(Op::EndLocal(local));
        }
        Ok(flow)
    }

    #[allow(clippy::too_many_arguments)]
    fn shared_argument(
        &mut self,
        arg: &Expr,
        expected: &Type,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let target = &expected.reference_type().unwrap().target;
        let place = expression_path(arg)
            .is_some_and(|path| vars.contains_key(path.split('.').next().unwrap()));
        if place && !target.is_copy_value() {
            return self.borrow_local(arg, false, vars, suspends, arg.span());
        }
        let Flow::Value(value) =
            self.expr_expected(arg, vars, functions, suspends, types, Some(target))?
        else {
            return Ok(Flow::Diverged);
        };
        // Copy values snapshot at their argument position, as normal value
        // arguments do. Owning expression results move into this temporary.
        let name = format!("$call_argument_{}", self.slot_count);
        self.borrowed_names.require_place(name.clone());
        if let Some(owner) = value.as_ref().filter(|slot| slot.ty.is_owner()) {
            self.argument_scopes.last_mut().unwrap().insert(owner.index);
        }
        let start = self.local_scopes.last().unwrap().len();
        let binding = self.local_binding(
            &name,
            Binding {
                slot: value,
                mutable: false,
                place: None,
            },
        )?;
        self.argument_scopes.last_mut().unwrap().extend(
            self.local_scopes.last().unwrap()[start..]
                .iter()
                .map(|slot| slot.index),
        );
        let locals = HashMap::from([(name.clone(), binding)]);
        self.borrow_local(
            &Expr::Ident {
                name,
                span: arg.span(),
            },
            false,
            &locals,
            suspends,
            arg.span(),
        )
    }

    fn call_expr_inner(
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
        if let Some(value) = self.local_callee(callee, vars, *span).filter(|value| {
            matches!(
                self.type_hint(value, vars, functions, types),
                Some(Type::Function(_))
            ) || (IntegerType::conversion(callee).is_none()
                && (callee.contains('.')
                    || self
                        .function_name(callee)
                        .is_none_or(|name| !functions.contains_key(&name))))
        }) {
            if !type_args.is_empty() {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    "Function values do not accept type arguments",
                ));
            }
            return self.invoke_expr(&value, args, *span, vars, functions, suspends, types);
        }
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
                None,
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
        if matches!(callee.as_str(), "float32" | "float64") {
            if !type_args.is_empty() || args.len() != 1 {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    "Float conversion expects one argument and no type arguments",
                ));
            }
            let Flow::Value(value) = self.expr(&args[0], vars, functions, suspends, types)? else {
                return Ok(Flow::Diverged);
            };
            if !slot_type(&value).is_numeric() {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Float conversion requires an integer or float",
                ));
            }
            let dst = self.slot(if callee == "float32" {
                Type::Float32
            } else {
                Type::Float64
            })?;
            self.emit(Op::Convert(dst.clone(), value.unwrap()));
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
            if source.integer().is_none()
                && !(source == Type::Rune && target == IntegerType::UInt32)
            {
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
        self.check_drop_call(&name, *span)?;
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
            Some(&info),
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
        if matches!(info.compiler_intrinsic, Some(CompilerIntrinsic::Require)) {
            self.emit(Op::Require {
                condition: values[0].clone().unwrap(),
                message: values[1].clone().unwrap(),
            });
            return Ok(Flow::Value(None));
        }
        if matches!(
            info.compiler_intrinsic,
            Some(CompilerIntrinsic::PlainCopy(_) | CompilerIntrinsic::ReferenceFree(_))
        ) {
            return Ok(Flow::Value(None));
        }
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
            indirect: None,
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
            let (name, params, callee, args, branch_span, indirect) = match branch {
                Expr::Invoke { callee, args, span } => {
                    let Flow::Value(value) = self.expr(callee, vars, functions, suspends, types)?
                    else {
                        if let Some(next) = args.first().or_else(|| branches.get(branch_index + 1))
                        {
                            return Err(unreachable_error(next));
                        }
                        return Ok(Flow::Diverged);
                    };
                    let Some(value) = value else {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidCall,
                            "Unit is not callable",
                        ));
                    };
                    let Type::Function(signature) = &value.ty else {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidCall,
                            "The callee is not a function value",
                        ));
                    };
                    if !signature.suspends || signature.signature.ret != Type::Unit {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidCall,
                            format!(
                                "{keyword} branches must be @suspends functions returning Unit"
                            ),
                        ));
                    }
                    let name = self.indirect_contract(signature);
                    let params = signature.signature.params.clone();
                    let target = self.snapshot(&Some(value))?.unwrap();
                    (name, params, "function value", args, span, Some(target))
                }
                Expr::Call {
                    callee,
                    args,
                    span: branch_span,
                    ..
                } => {
                    if !functions.get(callee).is_some_and(|f| f.internal) {
                        self.check_call_root(callee, vars, branch_span)?;
                    }
                    if IntegerType::conversion(callee).is_some() {
                        return Err(type_error(
                            branch_span,
                            ErrorCode::InvalidCall,
                            format!(
                                "{keyword} branches must be @suspends functions returning Unit"
                            ),
                        ));
                    }
                    let (name, info) = self.resolve_callee(branch, vars, functions, types)?;
                    if !info.suspends || info.signature.ret != Type::Unit {
                        return Err(type_error(branch_span, ErrorCode::InvalidCall,
                            format!("{keyword} branch {callee} must be an @suspends function returning Unit")));
                    }
                    (
                        name,
                        info.signature.params,
                        callee.as_str(),
                        args,
                        branch_span,
                        None,
                    )
                }
                _ => {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        format!("{keyword} branches must be function calls"),
                    ))
                }
            };
            let Some(values) = self.checked_arguments(
                callee,
                args,
                &params,
                None,
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
                indirect,
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
