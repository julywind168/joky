//! Typed for-step expansion for compiler-owned borrowed cursors.
use super::*;
use crate::parser::ast::{MatchArm, Pattern, PayloadPattern};

impl FunctionPlan {
    pub(super) fn borrowed_iteration(
        &self,
        value: &Expr,
        arms: &[MatchArm],
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        span: Span,
    ) -> Option<Expr> {
        // Only the parser-authored step has this shape. Ordinary Iterator calls
        // and user matches retain the protocol's Option and @noescape contract.
        let Expr::Call { args, .. } = value else {
            return None;
        };
        let [Expr::Unary { operand, .. }] = args.as_slice() else {
            return None;
        };
        let Type::Reference(reference) = self.type_hint(operand, vars, functions, types)? else {
            return None;
        };
        if !matches!(reference.target, Type::BufferIterator(_)) {
            return None;
        }
        let [success, fallback] = arms else {
            return None;
        };
        let Pattern::Variant {
            binding: Some(binding),
            ..
        } = &success.pattern
        else {
            return None;
        };
        let PayloadPattern::Name {
            name,
            span: binding_span,
        } = binding.as_ref()
        else {
            return None;
        };
        let call = |method: &str| Expr::Call {
            callee: format!("$joky_iter.{method}"),
            type_args: vec![],
            args: vec![*operand.clone()],
            span: value.span(),
        };
        // References stay in direct local bindings, never in Option payloads.
        // Calls use the same provenance, reborrow and suspension checks as any
        // explicit use of advance/current; the for syntax grants no exemptions.
        Some(Expr::If {
            condition: Box::new(call("advance")),
            then_branch: Box::new(Expr::Sequence {
                exprs: vec![
                    Expr::Let {
                        name: name.clone(),
                        annotation: None,
                        value: Box::new(call(if reference.mutable {
                            "current_mut"
                        } else {
                            "current"
                        })),
                        span: *binding_span,
                    },
                    success.body.clone(),
                ],
                trailing: true,
                span,
            }),
            else_branch: Some(Box::new(fallback.body.clone())),
            span,
        })
    }
}
