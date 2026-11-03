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
        contract: Option<(&str, &FunctionInfo)>,
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
        let mut reserved_receiver = args.first().is_some_and(
            |arg| matches!(arg, Expr::Call { callee, .. } if callee == "$reserve_receiver"),
        );
        let mut reservation_ops = 0..0;
        for (index, (arg, ty)) in args.iter().zip(params).enumerate() {
            let temporary_start = self.local_scopes.last().unwrap().len();
            let automatic = !ty.contains_dyn()
                && contract.is_some_and(|(_, info)| info.can_borrow_argument(index))
                && ty.reference_type().is_some_and(|r| !r.mutable)
                && !self
                    .bound_hint(arg, vars, functions, types)
                    .is_some_and(|ty| ty.reference_type().is_some());
            let op_start = self.ops.len();
            let flow = if index == 0 && reserved_receiver {
                let Expr::Call { args, .. } = arg else {
                    unreachable!()
                };
                let flow =
                    self.expr_expected(&args[0], vars, functions, suspends, types, Some(ty))?;
                // Only member_call creates this marker, for a stable local path.
                // Preserve address evaluation while deferring exclusive access.
                for op in &mut self.ops[op_start..] {
                    match op {
                        Op::Reference {
                            dst,
                            src,
                            moving: false,
                        } => {
                            *op = Op::Reserve {
                                dst: dst.clone(),
                                src: src.clone(),
                            };
                        }
                        Op::Project { reservation, .. }
                        | Op::Buffer(BufferOp::Borrow { reservation, .. }) => *reservation = true,
                        _ => {}
                    }
                }
                reservation_ops = op_start..self.ops.len();
                flow
            } else if automatic {
                let (flow, borrowed) =
                    self.shared_argument(arg, ty, vars, functions, suspends, types)?;
                if borrowed {
                    let (name, _) = contract.expect("automatic borrow has a direct callee");
                    self.implicit_borrows
                        .push((name.to_owned(), index, arg.span()));
                }
                flow
            } else {
                self.expr_expected(arg, vars, functions, suspends, types, Some(ty))?
            };
            if reserved_receiver
                && index > 0
                && self.ops[op_start..].iter().any(|op| {
                    matches!(
                        op,
                        Op::Call { state: Some(_), .. }
                            | Op::Concurrent { .. }
                            | Op::TaskBranch { .. }
                            | Op::TaskJoin { .. }
                    )
                })
            {
                // Suspending arguments retain the original immediate-borrow
                // contract. Do not broaden reservations across Pending, or reject
                // existing calls whose arguments do not access the receiver.
                for op in &mut self.ops[reservation_ops.clone()] {
                    match op {
                        Op::Reserve { dst, src } => {
                            *op = Op::Reference {
                                dst: dst.clone(),
                                src: src.clone(),
                                moving: false,
                            };
                        }
                        Op::Project { reservation, .. }
                        | Op::Buffer(BufferOp::Borrow { reservation, .. }) => *reservation = false,
                        _ => {}
                    }
                }
                reserved_receiver = false;
            }
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
            let consuming = contract.is_some_and(|(_, info)| {
                matches!(info.compiler_intrinsic, Some(CompilerIntrinsic::MemDrop))
            });
            let value = if index == 0 && reserved_receiver {
                value
            } else {
                self.reference_value(value, consuming)?
            };
            if ty.is_place() || matches!(arg, Expr::AnonymousFunction(_)) {
                if let Some(scope) = self.argument_scopes.last_mut() {
                    scope.extend(
                        self.local_scopes.last().unwrap()[temporary_start..]
                            .iter()
                            .map(|slot| slot.index),
                    );
                }
            }
            extend_argument(&mut values, value);
        }
        if reserved_receiver {
            // Activation checks that no argument retains a conflicting loan.
            values[0] = self.reference_value(values[0].take(), false)?;
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
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        // Argument temporaries live through the complete call, including later
        // argument evaluation, and end before the caller's next expression.
        self.local_scopes.push(Vec::new());
        self.argument_scopes.push(Default::default());
        let flow = self.call_expr_inner(expr, vars, functions, suspends, types, expected)?;
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
    ) -> Result<(Flow, bool), Error> {
        let target = &expected.reference_type().unwrap().target;
        let place = expression_path(arg)
            .is_some_and(|path| vars.contains_key(path.split('.').next().unwrap()));
        if place && !target.is_copy_value() {
            return self
                .borrow_local(arg, false, vars, suspends, arg.span())
                .map(|flow| (flow, true));
        }
        let Flow::Value(value) =
            self.expr_expected(arg, vars, functions, suspends, types, Some(target))?
        else {
            return Ok((Flow::Diverged, false));
        };
        // A missing hint does not mean a value needs borrowing: expressions
        // such as (&*(alloc(...))) already evaluate to a reference.
        if value
            .as_ref()
            .is_some_and(|slot| slot.ty.reference_type().is_some())
        {
            return self
                .coerce_reference(Flow::Value(value), Some(expected))
                .map(|flow| (flow, false));
        }
        // Copy values snapshot at their argument position, as normal value
        // arguments do. Owning expression results move into this temporary.
        let name = format!("$call_argument_{}", self.slot_count);
        self.borrowed_names.require_place(name.clone());
        if let Some(owner) = value
            .as_ref()
            .filter(|slot| slot.ty.is_owner() && !slot.ty.is_place())
        {
            self.argument_scopes.last_mut().unwrap().insert(owner.index);
        }
        let start = self.local_scopes.last().unwrap().len();
        let mut binding = self.local_binding(
            &name,
            Binding {
                slot: value,
                mutable: false,
                place: None,
            },
        )?;
        let target = slot_type(&binding.slot);
        if binding.place.is_none() && target.is_copy_value() && target.contains_references() {
            // Shared-reference aggregates can be snapshotted for a noescape call,
            // without making explicit local or mutable borrows available.
            let dst = self.slot(Type::reference(target.clone(), true))?;
            self.emit(Op::Local {
                dst: dst.clone(),
                value: binding.slot.clone(),
                target: target.clone(),
            });
            self.local_scopes.last_mut().unwrap().push(dst.clone());
            binding.place = Some(dst);
        }
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
        .map(|flow| (flow, true))
    }

    fn call_expr_inner(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        if let Expr::Invoke { callee, args, span } = expr {
            return self.invoke_expr(callee, args, *span, vars, functions, suspends, types);
        }
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
            ) || self
                .type_hint(value, vars, functions, types)
                .is_some_and(|t| super::closures::callable_signature(&t).is_some())
                || (IntegerType::conversion(callee).is_none()
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
        let (name, info) = self.resolve_callee_expected(expr, vars, functions, types, expected)?;
        if let Some(intrinsic @ (CompilerIntrinsic::MemReplace | CompilerIntrinsic::MemSwap)) =
            &info.compiler_intrinsic
        {
            return self.mem_call(
                intrinsic,
                args,
                &info.signature.params,
                *span,
                vars,
                functions,
                suspends,
                types,
            );
        }
        self.check_drop_call(&name, *span)?;
        if info.extern_abi == crate::core::builtins::ExternAbi::C && !self.unsafe_context {
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
            Some((&name, &info)),
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
        if matches!(info.compiler_intrinsic, Some(CompilerIntrinsic::ArenaAlloc)) {
            let target = info.signature.params[1].clone();
            if target.contains_fixed_array()
                || !target.is_copy_storage_value()
                || target.reference_type().is_some()
            {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Arena.alloc requires Copy data, not a reference, String or opaque handle",
                ));
            }
            let dst = self.slot(info.signature.ret.clone())?;
            self.emit(Op::ArenaAlloc {
                dst: dst.clone(),
                arena: values[0].clone().unwrap(),
                value: values.get(1).and_then(Option::as_ref).map(|value| Slot {
                    index: value.index,
                    ty: target.clone(),
                }),
                target,
            });
            return Ok(Flow::Value(Some(dst)));
        }
        if let Some(flow) = self.source_domain_call(super::domains::DomainCall {
            info: &info,
            args,
            values: &values,
            span: *span,
            vars,
            functions,
            suspends,
            types,
        })? {
            return Ok(flow);
        }
        if matches!(info.compiler_intrinsic, Some(CompilerIntrinsic::Panic)) {
            self.emit(Op::Panic {
                message: values[0].clone().unwrap(),
            });
            return Ok(Flow::Diverged);
        }
        if matches!(info.compiler_intrinsic, Some(CompilerIntrinsic::Assert)) {
            self.emit(Op::Assert {
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
        if fixed_arrays::intrinsic(&info.compiler_intrinsic) {
            return self.fixed_array_call(&info, values);
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
        if let Some(dst) = result.as_ref().filter(|s| s.ty.is_place()) {
            self.emit(Op::Place(PlaceOp::Allocate(dst.clone())));
        }
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
    pub(super) fn concurrent_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let Expr::Concurrent {
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
        let empty_args = Vec::new();
        let mut planned = Vec::with_capacity(branches.len());
        let mut result_type = None;
        for (branch_index, source_branch) in branches.iter().enumerate() {
            let resolved = self.member_call(source_branch, vars, functions, types)?;
            let branch = resolved.as_ref().unwrap_or(source_branch);
            let local = if let Expr::Call {
                callee,
                type_args,
                span,
                ..
            } = branch
            {
                let target = self.local_callee(callee, vars, *span).filter(|value| {
                    self.type_hint(value, vars, functions, types)
                        .is_some_and(|ty| super::closures::callable_signature(&ty).is_some())
                });
                if target.is_some() && !type_args.is_empty() {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "Function values do not accept type arguments",
                    ));
                }
                target
            } else {
                None
            };
            let target = match branch {
                Expr::Invoke { callee, .. } => Some(callee.as_ref()),
                _ => local.as_ref(),
            };
            let mut captures = None;
            let (name, params, ret, callee, args, branch_span, indirect) = match branch {
                Expr::Block { span, .. } => {
                    let expected = if *kind == ConcurrentKind::Race {
                        expected.or(result_type.as_ref())
                    } else if let Some(Type::Tuple(tuple)) = expected {
                        tuple.elements.get(branch_index)
                    } else {
                        None
                    };
                    let (name, values, ret) =
                        self.nested_task(branch, *span, expected.cloned(), vars, functions, types)?;
                    captures = Some(values);
                    (name, Vec::new(), ret, "block", &empty_args, span, None)
                }
                Expr::Invoke { args, span, .. } | Expr::Call { args, span, .. }
                    if target.is_some() =>
                {
                    let flow =
                        self.callable_value(target.unwrap(), vars, functions, suspends, types)?;
                    let Flow::Value(value) = flow else {
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
                    let value = if value
                        .ty
                        .reference_type()
                        .is_some_and(|r| matches!(r.target, Type::Function(_)))
                    {
                        self.read_local(value)?.unwrap()
                    } else {
                        value
                    };
                    if !matches!(value.ty, Type::Function(_))
                        && super::closures::callable_suspends(&value.ty)
                    {
                        let Some(call) = self.prepare_callable(
                            value,
                            args,
                            *span,
                            vars,
                            functions,
                            suspends,
                            types,
                            branches.get(branch_index + 1),
                        )?
                        else {
                            return Ok(Flow::Diverged);
                        };
                        captures = Some(call.arguments);
                        (
                            call.name,
                            Vec::new(),
                            call.result,
                            "callable",
                            &empty_args,
                            span,
                            None,
                        )
                    } else {
                        let Type::Function(signature) = &value.ty else {
                            return Err(type_error(
                                span,
                                ErrorCode::InvalidCall,
                                "The callee is not a function value",
                            ));
                        };
                        if !signature.suspends {
                            return Err(type_error(
                                span,
                                ErrorCode::InvalidCall,
                                format!("{keyword} branches must be @suspends functions"),
                            ));
                        }
                        let name = self.indirect_contract(signature);
                        let params = signature.signature.params.clone();
                        let ret = signature.signature.ret.clone();
                        let target = self.snapshot(&Some(value))?.unwrap();
                        (
                            name,
                            params,
                            ret,
                            "function value",
                            args,
                            span,
                            Some(target),
                        )
                    }
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
                            format!("{keyword} branches must be @suspends functions"),
                        ));
                    }
                    let (name, info) = self.resolve_callee(branch, vars, functions, types)?;
                    if !info.suspends {
                        return Err(type_error(
                            branch_span,
                            ErrorCode::InvalidCall,
                            format!("{keyword} branch {callee} must be an @suspends function"),
                        ));
                    }
                    (
                        name,
                        info.signature.params,
                        info.signature.ret,
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
                        format!("{keyword} branches must be function calls or blocks"),
                    ))
                }
            };
            if ret.contains_references()
                || ret.contains_functions()
                || ret.contains_closures()
                || (ret.is_owner() && !ret.is_suspend_safe_owner())
            {
                return Err(type_error(
                    branch_span,
                    ErrorCode::InvalidCall,
                    format!("{keyword} results must not contain references or function values"),
                ));
            }
            if *kind == ConcurrentKind::Race {
                if let Some(expected) = &result_type {
                    if expected != &ret {
                        return Err(type_error(branch_span, ErrorCode::TypeMismatch,
                            format!("race branches must return the same type: expected {expected}, got {ret}")));
                    }
                }
                result_type = Some(ret.clone());
            }
            let values = if let Some(values) = captures {
                values
            } else {
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
                values
            };
            let done = self.slot(Type::Bool)?;
            let result = if *kind == ConcurrentKind::Join && ret != Type::Unit {
                Some(self.slot(ret)?)
            } else {
                None
            };
            let cleanup = result
                .as_ref()
                .filter(|s| s.ty.is_owner() && !s.ty.is_place())
                .map(|value| CleanupOp::Register {
                    value: value.clone(),
                    place: None,
                    hooks: vec![],
                    elements: vec![],
                });
            planned.push(ConcurrentBranch {
                result,
                cleanup,
                indirect,
                name,
                args: values,
                done,
            });
        }
        let state = self.state_count;
        self.state_count += 1;
        let result = match result_type {
            Some(ty) if ty != Type::Unit => Some(self.slot(ty)?),
            _ => None,
        };
        if let Some(value) = result
            .as_ref()
            .filter(|slot| slot.ty.is_owner() && !slot.ty.is_place())
        {
            for branch in &mut planned {
                branch.cleanup = Some(CleanupOp::Register {
                    value: value.clone(),
                    place: None,
                    hooks: vec![],
                    elements: vec![],
                });
            }
        }
        for destination in result
            .iter()
            .chain(planned.iter().filter_map(|b| b.result.as_ref()))
        {
            if destination.ty.is_place() {
                self.emit(Op::Place(PlaceOp::Allocate(destination.clone())));
            }
        }
        let values: Vec<_> = planned.iter().map(|b| b.result.clone()).collect();
        self.emit(Op::Concurrent {
            result: result.clone(),
            kind: *kind,
            failure: None,
            branches: planned,
            state,
        });
        self.emit(Op::ConcurrentFinish);
        if *kind == ConcurrentKind::Join && values.iter().any(Option::is_some) {
            self.tuple_values(values, types, *span)
        } else {
            Ok(Flow::Value(result))
        }
    }
}
