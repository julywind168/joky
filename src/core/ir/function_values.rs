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
        let context = self.generics.borrow();
        if !context.definitions.contains(&name) || info.suspends {
            return Err(type_error(&span, ErrorCode::InvalidCall, "Function values require a synchronous language function; wrap host calls in a language function"));
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
            types.validate(ty).map_err(|e| e.with_span(span))?;
            validate_nominal_type(ty, &self.nominals, &span)?;
            if !ty.is_copy_storage_value() {
                return Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Generic function values require Copy type arguments",
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
        let Flow::Value(value) = self.expr(callee, vars, functions, suspends, types)? else {
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
        let Type::Function(signature) = &value.ty else {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "The callee is not a function value",
            ));
        };
        let signature = signature.clone();
        // Snapshot before evaluating arguments: an argument may mutate the callee's binding.
        let target = self.snapshot(&Some(value))?.unwrap();
        let Some(args) = self.checked_arguments(
            "function value",
            args,
            &signature.params,
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
        let result = if signature.ret == Type::Unit {
            None
        } else {
            Some(self.slot(signature.ret.clone())?)
        };
        let index = {
            let mut context = self.generics.borrow_mut();
            if let Some(index) = context
                .callables
                .iter()
                .position(|s| s == signature.as_ref())
            {
                index
            } else {
                let index = context.callables.len();
                context.callables.push(signature.as_ref().clone());
                index
            }
        };
        self.emit(Op::Call {
            indirect: Some(target),
            name: format!("__joky_indirect_{index}"),
            args,
            result: result.clone(),
            state: None,
        });
        Ok(Flow::Value(result))
    }
}
