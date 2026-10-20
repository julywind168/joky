//! Call-site captures contain checked entry identities and bounded value inputs.
use super::*;
pub(super) struct DomainCall<'a> {
    pub info: &'a FunctionInfo,
    pub args: &'a [Expr],
    pub values: &'a [Option<Slot>],
    pub span: Span,
    pub vars: &'a mut HashMap<String, Binding>,
    pub functions: &'a HashMap<String, FunctionInfo>,
    pub suspends: bool,
    pub types: &'a TypeRegistry,
}
impl FunctionPlan {
    /// Emit a module-local metadata key, never a callable user projection.
    pub(super) fn directory_type(&mut self, ty: &Type, span: Span) -> Result<Slot, Error> {
        let operation = if self.generics.borrow().checking {
            String::new()
        } else {
            let root = self.pointee(ty, &self.type_registry)?;
            if !matches!(&root, Type::Record(record) if record.template.is_none()
                && record.parameters.is_empty() && record.arguments.is_empty())
                || !self.domain_storage(&root)
            {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Directory lookup requires a supported non-generic state record",
                ));
            }
            let mut context = self.generics.borrow_mut();
            let witness = context
                .templates
                .iter()
                .find(|(_, f)| {
                    matches!(
                        f.compiler_intrinsic,
                        Some(CompilerIntrinsic::DirectoryWitness)
                    )
                })
                .map(|(name, _)| name.clone())
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::InvalidCall,
                        "Missing directory type witness",
                    )
                })?;
            let name = context.instantiate(
                &witness,
                vec![ty.clone().into()],
                crate::FunctionSignature::new(vec![Type::reference(ty.clone(), false)], Type::Unit),
                self.instantiation_depth + 1,
                span,
            )?;
            self.domain_calls.push((name.clone(), span));
            name
        };
        let result = self.slot(Type::String)?;
        self.emit(Op::Text {
            dst: result.clone(),
            source: TextSource::Literal(operation),
        });
        self.register_owner(result.clone(), None);
        Ok(result)
    }

    pub(super) fn source_domain_call(
        &mut self,
        call: DomainCall<'_>,
    ) -> Result<Option<Flow>, Error> {
        let DomainCall {
            info,
            args,
            values,
            span,
            vars,
            functions,
            suspends,
            types,
        } = call;
        use crate::parser::ast::DomainAccessKind;
        use CompilerIntrinsic::*;
        // Rigid template checks defer this capability; concrete instances must
        // satisfy it before any bits operation reaches a backend.
        let checking = self.generics.borrow().checking;
        let scalar = |ty: &Type| {
            crate::core::instance::scalar(ty) || (checking && matches!(ty, Type::Parameter(_)))
        };
        let Some(kind) = &info.compiler_intrinsic else {
            return Ok(None);
        };
        if let TimerStart(periodic) = kind {
            let (operation, target) = self
                .function_value_info(&args[3], vars, functions, types, true)?
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::InvalidCall,
                        "Timer requires a named domain operation",
                    )
                })?;
            if target.suspends
                || target.signature.params.len() != 2
                || !matches!(&target.signature.params[0], Type::Reference(r) if r.mutable)
                || !(scalar(&target.signature.params[1])
                    || self.domain_value(&target.signature.params[1]))
                || target.signature.ret != Type::Unit
            {
                return Err(type_error(&span, ErrorCode::InvalidCall,
                    "Timer requires a synchronous mutable domain operation with bounded value input (64 contract members, 64 lanes, 8 aggregate levels) and Unit result"));
            }
            if !checking {
                self.domain_calls.push((operation.clone(), args[3].span()));
            }
            let module = self.slot(crate::runtime::domain::module_type())?;
            self.emit(Op::DomainModule(module.clone()));
            let Flow::Value(text) = self.expr(
                &Expr::Literal {
                    literal: Literal::String(operation),
                    span,
                },
                vars,
                functions,
                suspends,
                types,
            )?
            else {
                unreachable!()
            };
            let value_input = checking || !scalar(&target.signature.params[1]);
            let flag = self.slot(Type::Bool)?;
            self.emit(Op::Literal(flag.clone(), Constant::Bool(*periodic)));
            let name = if value_input {
                let mut context = self.generics.borrow_mut();
                let name = context
                    .templates
                    .iter()
                    .find(|(_, f)| matches!(f.compiler_intrinsic, Some(TimerValueFinish)))
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        type_error(&span, ErrorCode::InvalidCall, "Missing timer value adapter")
                    })?;
                context.instantiate(
                    &name,
                    vec![target.signature.params[1].clone().into()],
                    crate::FunctionSignature::new(
                        vec![
                            crate::runtime::domain::module_type(),
                            Type::UInt64,
                            crate::runtime::domain::handle_type(),
                            Type::Int32,
                            Type::String,
                            target.signature.params[1].clone(),
                            Type::Bool,
                        ],
                        info.signature.ret.clone(),
                    ),
                    self.instantiation_depth + 1,
                    span,
                )?
            } else {
                functions
                    .iter()
                    .find(|(_, f)| matches!(f.compiler_intrinsic, Some(TimerFinish)))
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        type_error(&span, ErrorCode::InvalidCall, "Missing timer adapter")
                    })?
            };
            let mut captures = vec![
                Some(module),
                Some(Slot {
                    index: values[0].as_ref().unwrap().index,
                    ty: Type::UInt64,
                }),
                Some(Slot {
                    index: values[1].as_ref().unwrap().index,
                    ty: crate::runtime::domain::handle_type(),
                }),
                values[2].clone(),
                text,
            ];
            if value_input {
                captures.extend_from_slice(&values[4..]);
            } else {
                captures.push(Some(self.pack_domain_scalar(values[4].clone())?));
            }
            captures.push(Some(flag));
            let result = self.slot(info.signature.ret.clone())?;
            let state = self.state_count;
            self.state_count += 1;
            self.emit(Op::Call {
                indirect: None,
                name,
                args: captures,
                result: Some(result.clone()),
                state: Some(state),
            });
            return Ok(Some(Flow::Value(Some(result))));
        }
        if matches!(kind, DomainReadOnly) {
            let bits = self.pack_domain_scalar(values[0].clone())?;
            let mask = self.slot(Type::UInt64)?;
            self.emit(Op::Literal(mask.clone(), Constant::UInt64(!1)));
            let result = self.slot(Type::UInt64)?;
            self.emit(Op::Binary(
                result.clone(),
                crate::parser::ast::BinOp::BitAnd,
                bits,
                mask,
            ));
            return Ok(Some(Flow::Value(
                self.unpack_domain_scalar(result, &info.signature.ret)?,
            )));
        }
        if matches!(kind, DomainModule) {
            let slot = self.slot(crate::runtime::domain::module_type())?;
            self.emit(Op::DomainModule(slot.clone()));
            return Ok(Some(Flow::Value(Some(slot))));
        }
        if matches!(kind, DomainSnapshotValue) && !checking && scalar(&info.signature.ret) {
            let result = self.slot(Type::UInt64)?;
            let state = self.state_count;
            self.state_count += 1;
            self.emit(Op::Call {
                indirect: None,
                name: "__domain_snapshot_value".into(),
                args: values.to_vec(),
                result: Some(result.clone()),
                state: Some(state),
            });
            return Ok(Some(Flow::Value(
                self.unpack_domain_scalar(result, &info.signature.ret)?,
            )));
        }
        if matches!(kind, DomainPack | DomainUnpack) {
            let ty = if matches!(kind, DomainPack) {
                &info.signature.params[0]
            } else {
                &info.signature.ret
            };
            if !scalar(ty) {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Domain inputs and results must be plain scalars",
                ));
            }
            let value = if matches!(kind, DomainPack) {
                Some(self.pack_domain_scalar(values[0].clone())?)
            } else {
                self.unpack_domain_scalar(values[0].clone().unwrap(), ty)?
            };
            return Ok(Some(Flow::Value(value)));
        }
        if matches!(
            kind,
            DomainSetRead | DomainSetCandidate(_) | DomainSetEdit(_)
        ) {
            let (operation, target) = self
                .function_value_info(&args[2], vars, functions, types, true)?
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::InvalidCall,
                        "Domain set requires a named language operation",
                    )
                })?;
            let editing = matches!(kind, DomainSetEdit(_));
            let candidate = matches!(kind, DomainSetCandidate(_) | DomainSetEdit(_));
            if editing
                && (target.signature.params.len() != 2
                    || target.signature.ret != Type::Unit
                    || target.signature.params[0]
                        .reference_type()
                        .is_none_or(|r| !r.mutable))
            {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Candidate editor requires a mutable state reference and Unit result",
                ));
            }
            let inputs = &target.signature.params[usize::from(!candidate || editing)..];
            if target.suspends
                || inputs.len() != usize::from(candidate)
                || !inputs
                    .iter()
                    .all(|ty| scalar(ty) || (candidate && self.domain_value(ty)))
                || (!candidate
                    && !scalar(&target.signature.ret)
                    && !self.domain_value(&target.signature.ret))
            {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Domain operation requires a synchronous bounded value signature (64 contract members, 64 lanes, 8 aggregate levels)",
                ));
            }
            self.domain_calls.push((operation.clone(), args[2].span()));
            let module = self.slot(crate::runtime::domain::module_type())?;
            self.emit(Op::DomainModule(module.clone()));
            let handle = values[1].as_ref().unwrap();
            let mut captures = vec![
                Some(module),
                values[0].clone(),
                Some(Slot {
                    index: handle.index,
                    ty: crate::runtime::domain::handle_type(),
                }),
            ];
            let Flow::Value(text) = self.expr(
                &Expr::Literal {
                    literal: Literal::String(operation),
                    span,
                },
                vars,
                functions,
                suspends,
                types,
            )?
            else {
                unreachable!()
            };
            captures.push(text);
            let value_candidate = candidate && (editing || checking || !inputs.iter().all(scalar));
            if value_candidate {
                captures.extend_from_slice(&values[3..]);
            } else if candidate {
                captures.push(Some(self.pack_domain_scalar(values[3].clone())?));
            }
            let value_read = !candidate && (checking || !scalar(&target.signature.ret));
            let name = if value_read {
                let mut context = self.generics.borrow_mut();
                let name = context
                    .templates
                    .iter()
                    .find(|(_, f)| matches!(f.compiler_intrinsic, Some(DomainSetReadFinish)))
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        type_error(
                            &span,
                            ErrorCode::InvalidCall,
                            "Missing snapshot projection adapter",
                        )
                    })?;
                context.instantiate(
                    &name,
                    vec![target.signature.ret.clone().into()],
                    crate::FunctionSignature::new(
                        vec![
                            crate::runtime::domain::module_type(),
                            Type::reference(crate::runtime::domain::snapshot_set_type(), false),
                            crate::runtime::domain::handle_type(),
                            Type::String,
                        ],
                        target.signature.ret.clone(),
                    ),
                    self.instantiation_depth + 1,
                    span,
                )?
            } else if value_candidate {
                let adding = matches!(kind, DomainSetCandidate(true) | DomainSetEdit(true));
                let mut context = self.generics.borrow_mut();
                let name = context.templates.iter()
                    .find(|(_, f)| if editing { matches!(f.compiler_intrinsic, Some(DomainSetEditFinish(flag)) if flag == adding) } else { matches!(f.compiler_intrinsic, Some(DomainSetValueFinish(flag)) if flag == adding) })
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| type_error(&span, ErrorCode::InvalidCall, "Missing domain set value adapter"))?;
                context.instantiate(
                    &name,
                    vec![inputs[0].clone().into()],
                    crate::FunctionSignature::new(
                        vec![
                            crate::runtime::domain::module_type(),
                            info.signature.params[0].clone(),
                            crate::runtime::domain::handle_type(),
                            Type::String,
                            inputs[0].clone(),
                        ],
                        info.signature.ret.clone(),
                    ),
                    self.instantiation_depth + 1,
                    span,
                )?
            } else if candidate {
                let adding = matches!(kind, DomainSetCandidate(true));
                functions.iter().find(|(_, f)| matches!(f.compiler_intrinsic, Some(DomainSetFinish(flag)) if flag == adding))
                    .map(|(name,_)| name.clone()).ok_or_else(|| type_error(&span,ErrorCode::InvalidCall,"Missing domain set adapter"))?
            } else {
                "__domain_set_read".into()
            };
            let result = self.slot(if candidate || value_read {
                info.signature.ret.clone()
            } else {
                Type::UInt64
            })?;
            if !candidate {
                captures[1].as_mut().unwrap().ty =
                    Type::reference(crate::runtime::domain::snapshot_set_type(), false);
            }
            let output = result.clone();
            let state = self.state_count;
            self.state_count += 1;
            self.emit(Op::Call {
                indirect: None,
                name,
                args: captures,
                result: Some(output),
                state: Some(state),
            });
            if result.ty.is_owner() {
                self.register_owner(result.clone(), None);
            }
            if candidate {
                return Ok(Some(Flow::Value(Some(result))));
            }
            return Ok(Some(Flow::Value(if value_read {
                Some(result)
            } else {
                self.unpack_domain_scalar(result, &info.signature.ret)?
            })));
        }
        if !matches!(
            kind,
            DomainCreate
                | DomainSnapshot
                | DomainSnapshotPair
                | DomainCandidate
                | DomainCandidatePair
                | DomainAt(_)
                | DomainPair(_)
                | DomainRequest
                | DomainAccess(_)
        ) {
            return Ok(None);
        }
        let ambient = matches!(kind, DomainAccess(_));
        let pair = matches!(kind, DomainPair(_) | DomainSnapshotPair);
        let index = if pair || matches!(kind, DomainCandidate | DomainCandidatePair) {
            2
        } else {
            usize::from(!matches!(kind, DomainCreate | DomainAccess(_)))
        };
        let (operation, target) = self
            .function_value_info(&args[index], vars, functions, types, true)?
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Domain access requires a named language operation",
                )
            })?;
        let request = matches!(kind, DomainRequest);
        let candidate = matches!(kind, DomainCandidate | DomainCandidatePair);
        let create = matches!(kind, DomainCreate) || candidate;
        let snapshot = matches!(kind, DomainSnapshot | DomainSnapshotPair);
        let access = match kind {
            DomainAt(access) | DomainAccess(access) | DomainPair(access) => Some(*access),
            _ => None,
        };
        let write = access == Some(DomainAccessKind::Write);
        let query = access == Some(DomainAccessKind::Query);
        let inputs = &target.signature.params[if pair {
            2
        } else {
            usize::from(!request && !create)
        }..];
        let value_call = matches!(kind, DomainAt(_) | DomainPair(_) | DomainAccess(_));
        let value_input = value_call || create || snapshot || request;
        let value_type = |ty: &Type| scalar(ty) || (value_input && self.domain_value(ty));
        if inputs.len() != usize::from(request || create || write || query)
            || !inputs.iter().all(value_type)
            || (!create && !value_type(&target.signature.ret))
            || target.suspends != request
        {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                if request {
                    "Domain request requires a suspending bounded value signature (64 contract members, 64 lanes, 8 aggregate levels)"
                } else if value_input {
                    "Domain operation requires a synchronous bounded value signature (64 contract members, 64 lanes, 8 aggregate levels)"
                } else {
                    "Domain operation requires a checked scalar signature"
                },
            ));
        }
        if !request {
            self.domain_calls
                .push((operation.clone(), args[index].span()));
        }
        let mut captures = Vec::new();
        if !ambient {
            let module = self.slot(crate::runtime::domain::module_type())?;
            self.emit(Op::DomainModule(module.clone()));
            captures.push(Some(module));
        }
        if pair {
            for value in &values[..2] {
                let handle = value.as_ref().unwrap();
                captures.push(Some(Slot {
                    index: handle.index,
                    ty: crate::runtime::domain::handle_type(),
                }));
            }
        }
        if candidate {
            captures.push(values[0].clone());
            captures.push(Some(Slot {
                index: values[1].as_ref().unwrap().index,
                ty: crate::runtime::domain::handle_type(),
            }));
        }
        if index == 1 {
            let handle = values[0].as_ref().unwrap();
            captures.push(Some(Slot {
                index: handle.index,
                ty: crate::runtime::domain::handle_type(),
            }));
        }
        let Flow::Value(text) = self.expr(
            &Expr::Literal {
                literal: Literal::String(operation),
                span,
            },
            vars,
            functions,
            suspends,
            types,
        )?
        else {
            unreachable!()
        };
        captures.push(text);
        if matches!(kind, DomainCreate) && (checking || !inputs.iter().all(scalar)) {
            let input_type = inputs[0].clone();
            let name = {
                let mut context = self.generics.borrow_mut();
                let name = context
                    .templates
                    .iter()
                    .find(|(_, f)| matches!(f.compiler_intrinsic, Some(DomainCreateFinish)))
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        type_error(
                            &span,
                            ErrorCode::InvalidCall,
                            "Missing domain constructor adapter",
                        )
                    })?;
                context.instantiate(
                    &name,
                    vec![
                        target.signature.ret.clone().into(),
                        input_type.clone().into(),
                    ],
                    crate::FunctionSignature::new(
                        vec![
                            crate::runtime::domain::module_type(),
                            Type::String,
                            input_type,
                        ],
                        info.signature.ret.clone(),
                    ),
                    self.instantiation_depth + 1,
                    span,
                )?
            };
            captures.extend_from_slice(&values[index + 1..]);
            let result = self.slot(info.signature.ret.clone())?;
            let state = self.state_count;
            self.state_count += 1;
            self.emit(Op::Call {
                indirect: None,
                name,
                args: captures,
                result: Some(result.clone()),
                state: Some(state),
            });
            self.register_owner(result.clone(), None);
            return Ok(Some(Flow::Value(Some(result))));
        }
        if value_call {
            let scalar_call =
                !checking && inputs.iter().all(scalar) && scalar(&target.signature.ret);
            let input_type = inputs.first().cloned().unwrap_or(Type::Unit);
            let result_type = target.signature.ret.clone();
            let adapter_input = if scalar_call {
                Type::UInt64
            } else {
                input_type.clone()
            };
            let name = {
                let mut context = self.generics.borrow_mut();
                let name = context.templates.iter()
                    .find(|(_, f)| {
                        if scalar_call {
                            matches!(f.compiler_intrinsic, Some(DomainScalarFinish(flag)) if flag == ambient)
                        } else {
                            matches!(f.compiler_intrinsic, Some(DomainValueFinish(flag)) if flag == ambient)
                        }
                    })
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| type_error(&span, ErrorCode::InvalidCall, "Missing domain access adapter"))?;
                let mut params = if ambient {
                    vec![Type::String, adapter_input]
                } else {
                    vec![
                        crate::runtime::domain::module_type(),
                        crate::runtime::domain::handle_type(),
                        crate::runtime::domain::handle_type(),
                        Type::UInt64,
                        Type::String,
                        adapter_input,
                    ]
                };
                if scalar_call {
                    params.extend([Type::Bool, Type::Bool]);
                }
                context.instantiate(
                    &name,
                    if scalar_call {
                        vec![result_type.into()]
                    } else {
                        vec![input_type.into(), result_type.into()]
                    },
                    crate::FunctionSignature::new(params, info.signature.ret.clone()),
                    self.instantiation_depth + 1,
                    span,
                )?
            };
            if !pair && !ambient {
                captures.insert(2, captures[1].clone());
            }
            if !ambient {
                let count = self.slot(Type::UInt64)?;
                self.emit(Op::Literal(
                    count.clone(),
                    Constant::UInt64(if pair { 2 } else { 1 }),
                ));
                captures.insert(3, Some(count));
            }
            // Checked arguments flatten records, including zero-lane empty records.
            if scalar_call {
                let input = if inputs.is_empty() {
                    None
                } else {
                    values[index + 1].clone()
                };
                captures.push(Some(self.pack_domain_scalar(input)?));
                let flag = self.slot(Type::Bool)?;
                self.emit(Op::Literal(flag.clone(), Constant::Bool(write)));
                captures.push(Some(flag));
                let flag = self.slot(Type::Bool)?;
                self.emit(Op::Literal(flag.clone(), Constant::Bool(query)));
                captures.push(Some(flag));
            } else if inputs.is_empty() {
                captures.push(None);
            } else {
                captures.extend_from_slice(&values[index + 1..]);
            }
            let result = Some(self.slot(info.signature.ret.clone())?);
            let state = self.state_count;
            self.state_count += 1;
            self.emit(Op::Call {
                indirect: None,
                name,
                args: captures,
                result: result.clone(),
                state: Some(state),
            });
            if let Some(value) = result.as_ref().filter(|v| v.ty.is_owner()) {
                self.register_owner(value.clone(), None);
            }
            return Ok(Some(Flow::Value(result)));
        }
        let value_candidate = candidate && (checking || !inputs.iter().all(scalar));
        let value_request =
            request && (checking || !inputs.iter().all(scalar) || !scalar(&target.signature.ret));
        if value_candidate || value_request {
            captures.extend_from_slice(&values[index + 1..]);
        } else if !inputs.is_empty() {
            captures.push(Some(self.pack_domain_scalar(values[index + 1].clone())?));
        }
        let name = match kind {
            DomainCreate => "__domain_create".to_owned(),
            DomainSnapshot | DomainSnapshotPair => {
                match (pair, !checking && scalar(&target.signature.ret)) {
                    (false, true) => "__domain_snapshot",
                    (true, true) => "__domain_snapshot_pair",
                    (false, false) => "__domain_snapshot_values",
                    (true, false) => "__domain_snapshot_pair_values",
                }
                .to_owned()
            }
            DomainCandidate | DomainCandidatePair => {
                let mut context = self.generics.borrow_mut();
                let name = context
                    .templates
                    .iter()
                    .find(
                        |(_, f)| match (matches!(kind, DomainCandidatePair), value_candidate) {
                            (true, true) => {
                                matches!(f.compiler_intrinsic, Some(DomainCandidatePairValueFinish))
                            }
                            (false, true) => {
                                matches!(f.compiler_intrinsic, Some(DomainCandidateValueFinish))
                            }
                            (true, false) => {
                                matches!(f.compiler_intrinsic, Some(DomainCandidatePairFinish))
                            }
                            (false, false) => {
                                matches!(f.compiler_intrinsic, Some(DomainCandidateFinish))
                            }
                        },
                    )
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        type_error(
                            &span,
                            ErrorCode::InvalidCall,
                            "Missing domain candidate adapter",
                        )
                    })?;
                let Type::Record(snapshot) = (if matches!(kind, DomainCandidatePair) {
                    &info.signature.ret
                } else {
                    &info.signature.params[0]
                }) else {
                    unreachable!()
                };
                let mut arguments = snapshot.arguments.clone();
                if matches!(kind, DomainCandidate) {
                    arguments.push(target.signature.ret.clone().into());
                }
                let input_type = if value_candidate {
                    arguments.push(inputs[0].clone().into());
                    inputs[0].clone()
                } else {
                    Type::UInt64
                };
                context.instantiate(
                    &name,
                    arguments,
                    crate::FunctionSignature::new(
                        vec![
                            crate::runtime::domain::module_type(),
                            info.signature.params[0].clone(),
                            crate::runtime::domain::handle_type(),
                            Type::String,
                            input_type,
                        ],
                        info.signature.ret.clone(),
                    ),
                    self.instantiation_depth + 1,
                    span,
                )?
            }
            DomainRequest => {
                let mut context = self.generics.borrow_mut();
                let name = context
                    .templates
                    .iter()
                    .find(|(_, f)| {
                        if value_request {
                            matches!(f.compiler_intrinsic, Some(DomainRequestValueFinish))
                        } else {
                            matches!(f.compiler_intrinsic, Some(DomainFinish))
                        }
                    })
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        type_error(
                            &span,
                            ErrorCode::InvalidCall,
                            "Missing domain result adapter",
                        )
                    })?;
                context.instantiate(
                    &name,
                    if value_request {
                        vec![
                            inputs[0].clone().into(),
                            target.signature.ret.clone().into(),
                        ]
                    } else {
                        vec![target.signature.ret.clone().into()]
                    },
                    crate::FunctionSignature::new(
                        if value_request {
                            vec![
                                crate::runtime::domain::module_type(),
                                crate::runtime::domain::handle_type(),
                                Type::String,
                                inputs[0].clone(),
                            ]
                        } else {
                            crate::runtime::domain::Operation::Request
                                .signature()
                                .params
                        },
                        info.signature.ret.clone(),
                    ),
                    self.instantiation_depth + 1,
                    span,
                )?
            }
            _ => unreachable!(),
        };
        let result = self.slot(info.signature.ret.clone())?;
        let output = if (create && !candidate) || snapshot {
            Slot {
                index: result.index,
                ty: if snapshot {
                    crate::runtime::domain::snapshot_type()
                } else {
                    crate::runtime::domain::owner_type()
                },
            }
        } else {
            result.clone()
        };
        let state = self.state_count;
        self.state_count += 1;
        self.emit(Op::Call {
            indirect: None,
            name,
            args: captures,
            result: Some(output),
            state: Some(state),
        });
        if result.ty.is_owner() {
            self.register_owner(result.clone(), None);
        }
        Ok(Some(Flow::Value(Some(result))))
    }
    fn pack_domain_scalar(&mut self, value: Option<Slot>) -> Result<Slot, Error> {
        let dst = self.slot(Type::UInt64)?;
        if let Some(src) = value {
            self.emit(Op::DomainBits {
                dst: dst.clone(),
                src,
                decode: false,
            });
        } else {
            self.emit(Op::Literal(dst.clone(), Constant::UInt64(0)));
        }
        Ok(dst)
    }
    fn unpack_domain_scalar(&mut self, src: Slot, ty: &Type) -> Result<Option<Slot>, Error> {
        if *ty == Type::Unit {
            return Ok(None);
        }
        let dst = self.slot(ty.clone())?;
        self.emit(Op::DomainBits {
            dst: dst.clone(),
            src,
            decode: true,
        });
        Ok(Some(dst))
    }
}
