//! Non-capturing language functions and conservative indirect-call contracts.
use super::*;
use crate::core::types::substitute;

impl FunctionPlan {
    pub(super) fn local_callee(
        &self,
        name: &str,
        vars: &HashMap<String, Binding>,
        span: Span,
    ) -> Option<Expr> {
        let mut segments = name.split('.');
        let root = segments.next()?;
        if !vars.contains_key(root) {
            return None;
        }
        let mut value = Expr::Ident {
            name: root.into(),
            span,
        };
        for name in segments {
            value = Expr::Field {
                base: Box::new(value),
                name: name.into(),
                type_args: Vec::new(),
                span,
            };
        }
        Some(value)
    }

    pub(super) fn function_value_info(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        instantiate: bool,
    ) -> Result<Option<(String, FunctionInfo)>, Error> {
        let Some(path) = expression_path(expr) else {
            return Ok(None);
        };
        if vars.contains_key(path.split('.').next().unwrap()) {
            return Ok(None);
        }
        let Some(name) = self.function_name(&path) else {
            return Ok(None);
        };
        let Some(mut info) = functions.get(&name).cloned() else {
            return Ok(None);
        };
        let span = expr.span();
        if matches!(
            info.compiler_intrinsic,
            Some(
                CompilerIntrinsic::ScriptSchema(_)
                    | CompilerIntrinsic::DomainValuePack
                    | CompilerIntrinsic::DomainValueUnpack
                    | CompilerIntrinsic::ScriptPack
                    | CompilerIntrinsic::ScriptUnpack
                    | CompilerIntrinsic::DomainAccess(_)
                    | CompilerIntrinsic::DomainReadOnly
                    | CompilerIntrinsic::DomainModule
                    | CompilerIntrinsic::TimerStart(_)
                    | CompilerIntrinsic::TimerValueFinish
                    | CompilerIntrinsic::TimerFinish
                    | CompilerIntrinsic::DirectoryType(_)
                    | CompilerIntrinsic::DirectoryWitness
                    | CompilerIntrinsic::DomainCreate
                    | CompilerIntrinsic::DomainSnapshot
                    | CompilerIntrinsic::DomainSnapshotPair
                    | CompilerIntrinsic::DomainSetRead
                    | CompilerIntrinsic::DomainSetEdit(_)
                    | CompilerIntrinsic::DomainSetEditFinish(_)
                    | CompilerIntrinsic::DomainSetCandidate(_)
                    | CompilerIntrinsic::DomainSetFinish(_)
                    | CompilerIntrinsic::DomainCandidate
                    | CompilerIntrinsic::DomainCandidateFinish
                    | CompilerIntrinsic::DomainCandidatePair
                    | CompilerIntrinsic::DomainCandidatePairFinish
                    | CompilerIntrinsic::DomainAt(_)
                    | CompilerIntrinsic::DomainPair(_)
                    | CompilerIntrinsic::DomainRequest
                    | CompilerIntrinsic::DirectoryRemoveIf
                    | CompilerIntrinsic::DirectoryRemoveIfFinish
                    | CompilerIntrinsic::DomainPathsFinish
                    | CompilerIntrinsic::DomainWhenFinish
                    | CompilerIntrinsic::DomainValueFinish(_)
                    | CompilerIntrinsic::DomainScalarFinish(_)
                    | CompilerIntrinsic::DomainRequestValueFinish
                    | CompilerIntrinsic::DomainFinish
                    | CompilerIntrinsic::DomainPack
                    | CompilerIntrinsic::DomainUnpack
            )
        ) {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Domain access functions must be called directly",
            ));
        }
        let context = self.generics.borrow();
        if !context.definitions.contains(&name) {
            return Err(type_error(&span, ErrorCode::InvalidCall, "Function values require a language function; wrap host calls in a language function"));
        }
        let explicit = expression_type_arguments(expr).unwrap_or_default();
        let Some(template) = context.templates.get(&name) else {
            if !explicit.is_empty() {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "Non-generic function does not accept type arguments",
                ));
            }
            return Ok(Some((name, info)));
        };
        if explicit.len() != template.type_params.len() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Generic function values require all explicit type arguments",
            ));
        }
        for (parameter, ty) in template.type_params.iter().zip(explicit) {
            types
                .validate_argument(parameter, ty)
                .map_err(|e| e.with_span(span))?;
            let Some(ty) = ty.as_type() else {
                continue;
            };
            types.validate(ty).map_err(|e| e.with_span(span))?;
            validate_nominal_type(ty, &self.nominals, &span)?;
            if !ty.is_copy_storage_value() && !ty.is_owner() {
                return Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Generic function values require Copy or owning type arguments",
                ));
            }
            context.members.traits.check_bounds(parameter, ty, span)?;
        }
        let substitutions = template
            .type_params
            .iter()
            .zip(explicit)
            .map(|(p, t)| (p.id, t.clone()))
            .collect();
        info.signature = crate::FunctionSignature::new(
            template
                .params
                .iter()
                .map(|p| substitute(&p.ty, &substitutions, types))
                .collect::<Result<_, _>>()?,
            substitute(&template.ret_ty, &substitutions, types)?,
        );
        drop(context);
        if !instantiate {
            return Ok(Some((name, info)));
        }
        let name = self.generics.borrow_mut().instantiate(
            &name,
            explicit.to_vec(),
            info.signature.clone(),
            self.instantiation_depth + 1,
            span,
        )?;
        Ok(Some((name, info)))
    }

    pub(super) fn indirect_contract(&self, signature: &crate::core::types::FunctionType) -> String {
        let mut context = self.generics.borrow_mut();
        let index = if let Some(index) = context.callables.iter().position(|s| s == signature) {
            index
        } else {
            let index = context.callables.len();
            context.callables.push(signature.clone());
            index
        };
        format!("__joky_indirect_{index}")
    }

    pub(super) fn callable_value(
        &mut self,
        callee: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        if let Expr::Ident { name, .. } = callee {
            if let Some(value) = vars.get(name).and_then(|b| b.slot.as_ref()).filter(|v| {
                super::closures::callable_signature(&v.ty).is_some()
                    && !matches!(v.ty, Type::Function(_))
            }) {
                return Ok(Flow::Value(Some(value.clone())));
            }
        }
        self.expr(callee, vars, functions, suspends, types)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn invoke_expr(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Flow::Value(value) = self.callable_value(callee, vars, functions, suspends, types)?
        else {
            if let Some(arg) = args.first() {
                return Err(unreachable_error(arg));
            }
            return Ok(Flow::Diverged);
        };
        let Some(value) = value else {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Unit is not callable",
            ));
        };
        if super::closures::callable_signature(&value.ty).is_some()
            && !matches!(value.ty, Type::Function(_))
        {
            return self.invoke_callable(value, args, span, vars, functions, suspends, types);
        }
        self.invoke_function_value(value, args, span, vars, functions, suspends, types)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn invoke_function_value(
        &mut self,
        value: Slot,
        args: &[Expr],
        span: Span,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Type::Function(signature) = &value.ty else {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "The callee is not a function value",
            ));
        };
        let signature = signature.clone();
        if signature.suspends && !suspends {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Caller of a suspending function value must be marked @suspends",
            ));
        }
        // Snapshot before evaluating arguments: an argument may mutate the callee's binding.
        let target = self.snapshot(&Some(value))?.unwrap();
        let contract_name = self.indirect_contract(&signature);
        let mut contract =
            FunctionInfo::indirect(signature.signature.clone(), signature.suspends, span);
        contract.noescape = signature.noescape;
        let Some(args) = self.checked_arguments(
            "function value",
            args,
            &signature.signature.params,
            Some((&contract_name, &contract)),
            span,
            |i| args.get(i + 1),
            vars,
            functions,
            suspends,
            types,
        )?
        else {
            return Ok(Flow::Diverged);
        };
        let result = if signature.signature.ret == Type::Unit {
            None
        } else {
            Some(self.slot(signature.signature.ret.clone())?)
        };
        let name = self.indirect_contract(&signature);
        let state = signature.suspends.then(|| {
            let state = self.state_count;
            self.state_count += 1;
            state
        });
        if let Some(dst) = result.as_ref().filter(|s| s.ty.is_place()) {
            self.emit(Op::Place(PlaceOp::Allocate(dst.clone())));
        }
        self.emit(Op::Call {
            indirect: Some(target),
            name,
            args,
            result: result.clone(),
            state,
        });
        Ok(Flow::Value(result))
    }
}
