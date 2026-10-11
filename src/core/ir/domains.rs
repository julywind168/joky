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
        let Some(kind) = &info.compiler_intrinsic else {
            return Ok(None);
        };
        if matches!(kind, DomainModule) {
            let slot = self.slot(crate::runtime::domain::module_type())?;
            self.emit(Op::DomainModule(slot.clone()));
            return Ok(Some(Flow::Value(Some(slot))));
        }
        if !matches!(kind, DomainCreate | DomainAt(_) | DomainRequest) {
            return Ok(None);
        }
        let index = usize::from(!matches!(kind, DomainCreate));
        let (operation, target) = self
            .function_value_info(&args[index], vars, functions, types, true)?
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Domain access requires a named language operation",
                )
            })?;
        if !matches!(kind, DomainRequest) {
            self.domain_calls
                .push((operation.clone(), args[index].span()));
        }
        if matches!(kind, DomainRequest)
            && (!target.suspends
                || target.signature.params != [Type::Int32]
                || target.signature.ret != Type::Int32)
        {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Domain request requires an Int32 suspending entry",
            ));
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
        let mut captures = vec![Some(module)];
        if index == 1 {
            let handle = values[0].as_ref().unwrap();
            captures.push(Some(Slot {
                index: handle.index,
                ty: crate::runtime::domain::handle_type(),
            }));
        }
        captures.push(text);
        if !matches!(kind, DomainAt(false)) {
            captures.push(values[index + 1].clone());
        }
        let name = match kind {
            DomainCreate => "__domain_create".to_owned(),
            DomainAt(false) => "__domain_read_at".to_owned(),
            DomainAt(true) => "__domain_write_at".to_owned(),
            DomainRequest => functions
                .iter()
                .find(|(_, f)| matches!(f.compiler_intrinsic, Some(DomainFinish)))
                .map(|(n, _)| n.clone())
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::InvalidCall,
                        "Missing domain result adapter",
                    )
                })?,
            _ => unreachable!(),
        };
        let result = self.slot(info.signature.ret.clone())?;
        let output = if matches!(kind, DomainCreate) {
            Slot {
                index: result.index,
                ty: crate::runtime::domain::owner_type(),
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
        if matches!(kind, DomainCreate) {
            self.register_owner(result.clone(), None);
        }
        Ok(Some(Flow::Value(Some(result))))
    }
}
