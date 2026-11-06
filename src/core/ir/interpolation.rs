//! Interpolation borrows existing places and owns only evaluated temporaries.
use super::*;

impl FunctionPlan {
    pub(super) fn interpolation_append(
        &mut self,
        part: (&Expr, &Expr, Span, &str),
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let (value, output, span, callee) = part;
        let hint = self.type_hint(value, vars, functions, types);
        let reference = hint
            .as_ref()
            .is_some_and(|ty| ty.reference_type().is_some());
        let place = expression_path(value)
            .is_some_and(|path| vars.contains_key(path.split('.').next().unwrap()));
        let name = format!("$interpolation_value_{}", span.start());
        let mut exprs = Vec::new();
        let argument = if reference {
            value.clone()
        } else if place && !hint.as_ref().is_some_and(Type::is_copy_value) {
            Expr::Unary {
                op: UnaryOp::Borrow,
                operand: Box::new(value.clone()),
                span,
            }
        } else {
            self.borrowed_names.require_place(name.clone());
            exprs.push(Expr::Let {
                name: name.clone(),
                annotation: None,
                value: Box::new(value.clone()),
                span,
            });
            Expr::Unary {
                op: UnaryOp::Borrow,
                operand: Box::new(Expr::Ident { name, span }),
                span,
            }
        };
        exprs.push(Expr::Call {
            callee: callee.into(),
            type_args: vec![],
            args: vec![argument, output.clone()],
            span,
        });
        self.expr(
            &Expr::Block { exprs, span },
            vars,
            functions,
            suspends,
            types,
        )
    }
}
