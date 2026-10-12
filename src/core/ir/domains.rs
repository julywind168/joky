//! Call-site captures contain checked entry identities and scalar inputs only.
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
        if matches!(kind, DomainModule) {
            let slot = self.slot(crate::runtime::domain::module_type())?;
            self.emit(Op::DomainModule(slot.clone()));
            return Ok(Some(Flow::Value(Some(slot))));
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
        if !matches!(
            kind,
            DomainCreate
                | DomainSnapshot
                | DomainSnapshotPair
                | DomainCandidate
                | DomainAt(_)
                | DomainPair(_)
                | DomainRequest
                | DomainAccess(_)
        ) {
            return Ok(None);
        }
        let ambient = matches!(kind, DomainAccess(_));
        let pair = matches!(kind, DomainPair(_) | DomainSnapshotPair);
        let index = if pair {
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
        let candidate = matches!(kind, DomainCandidate);
        let create = matches!(kind, DomainCreate) || candidate;
        let snapshot = matches!(kind, DomainSnapshot | DomainSnapshotPair);
        let write = matches!(kind, DomainAt(true) | DomainAccess(true) | DomainPair(true));
        let inputs = &target.signature.params[if pair {
            2
        } else {
            usize::from(!request && !create)
        }..];
        if inputs.len() != usize::from(request || create || write)
            || !inputs.iter().all(scalar)
            || (!create && !scalar(&target.signature.ret))
            || target.suspends != request
        {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Domain operation requires a checked scalar signature",
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
        if index == 1 {
            let handle = values[0].as_ref().unwrap();
            captures.push(Some(if candidate {
                handle.clone()
            } else {
                Slot {
                    index: handle.index,
                    ty: crate::runtime::domain::handle_type(),
                }
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
        if !inputs.is_empty() {
            captures.push(Some(self.pack_domain_scalar(values[index + 1].clone())?));
        }
        if pair && !snapshot && inputs.is_empty() {
            captures.push(Some(self.pack_domain_scalar(None)?));
        }
        let name = match kind {
            DomainCreate => "__domain_create".to_owned(),
            DomainPair(_) => "__domain_pair".to_owned(),
            DomainSnapshot => "__domain_snapshot".to_owned(),
            DomainSnapshotPair => "__domain_snapshot_pair".to_owned(),
            DomainAt(false) => "__domain_read_at".to_owned(),
            DomainAt(true) => "__domain_write_at".to_owned(),
            DomainAccess(false) => "__domain_read_scalar".to_owned(),
            DomainAccess(true) => "__domain_write_scalar".to_owned(),
            DomainCandidate => {
                let mut context = self.generics.borrow_mut();
                let name = context
                    .templates
                    .iter()
                    .find(|(_, f)| matches!(f.compiler_intrinsic, Some(DomainCandidateFinish)))
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        type_error(
                            &span,
                            ErrorCode::InvalidCall,
                            "Missing domain candidate adapter",
                        )
                    })?;
                let Type::Record(snapshot) = &info.signature.params[0] else {
                    unreachable!()
                };
                context.instantiate(
                    &name,
                    snapshot.arguments.clone(),
                    crate::FunctionSignature::new(
                        vec![
                            crate::runtime::domain::module_type(),
                            info.signature.params[0].clone(),
                            Type::String,
                            Type::UInt64,
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
                    .find(|(_, f)| matches!(f.compiler_intrinsic, Some(DomainFinish)))
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
                    vec![target.signature.ret.clone()],
                    crate::FunctionSignature::new(
                        crate::runtime::domain::Operation::Request
                            .signature()
                            .params,
                        info.signature.ret.clone(),
                    ),
                    self.instantiation_depth + 1,
                    span,
                )?
            }
            _ => unreachable!(),
        };
        let result = self.slot(if create || request || snapshot {
            info.signature.ret.clone()
        } else {
            Type::UInt64
        })?;
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
        if create || snapshot {
            self.register_owner(result.clone(), None);
        }
        if create || request || snapshot {
            Ok(Some(Flow::Value(Some(result))))
        } else {
            Ok(Some(Flow::Value(
                self.unpack_domain_scalar(result, &info.signature.ret)?,
            )))
        }
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
