//! Function calls and integer conversion intrinsics, checked before backend lowering.
use super::*;

impl FunctionPlan {
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
        check_call_root(callee, vars, span)?;
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
        if info.suspends && !suspends {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("Caller of `{callee}` must be marked @suspends"),
            ));
        }
        if info.signature.params.len() != args.len() {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("Argument count mismatch for `{callee}`"),
            ));
        }
        let mut values = Vec::new();
        for (index, (arg, ty)) in args.iter().zip(&info.signature.params).enumerate() {
            let Flow::Value(value) =
                self.expr_expected(arg, vars, functions, suspends, types, Some(ty))?
            else {
                if let Some(next) = args.get(index + 1) {
                    return Err(unreachable_error(next));
                }
                return Ok(Flow::Diverged);
            };
            if &slot_type(&value) != ty {
                return Err(type_error(
                    &arg.span(),
                    ErrorCode::TypeMismatch,
                    format!("Argument type mismatch for `{callee}`"),
                ));
            }
            extend_argument(&mut values, value);
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
            name,
            args: values,
            result: result.clone(),
            state,
        });
        Ok(Flow::Value(result))
    }
}
