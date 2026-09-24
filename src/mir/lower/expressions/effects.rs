use super::super::*;
use crate::hir::{CoreExpr, CoreExprKind, CoreFunction};

pub(super) fn contains_aborting_effect(
    expression: &CoreExpr,
    function_bodies: &HashMap<String, &CoreFunction>,
    types: &CheckedTypes,
) -> bool {
    fn callee_has_aborting_effect(
        callee: &CoreExpr,
        function_bodies: &HashMap<String, &CoreFunction>,
        types: &CheckedTypes,
    ) -> bool {
        let effects = match &callee.kind {
            CoreExprKind::Name(name) => function_bodies
                .get(name)
                .map(|function| &function.used_effects),
            CoreExprKind::Field {
                value,
                access: crate::syntax::FieldAccess::Name(method),
            } => {
                let key = match value.ty {
                    Type::Struct(id) => format!("struct_{id}_{method}"),
                    Type::Class(id) => format!("class_{id}_{method}"),
                    _ => return false,
                };
                function_bodies
                    .get(&key)
                    .map(|function| &function.used_effects)
            }
            CoreExprKind::Closure { effects, .. } => Some(effects),
            _ => None,
        };
        effects
            .or_else(|| match callee.ty {
                Type::Function(id) => Some(&types.function_type(id).effects),
                _ => None,
            })
            .is_some_and(|effects| {
                effects.iter().any(|operation| {
                    !matches!(
                        types.effects().operation_mode(operation),
                        Some(crate::sema::EffectMode::Normal)
                    )
                })
            })
    }

    match &expression.kind {
        CoreExprKind::Call {
            effect_operation: Some(operation),
            arguments,
            ..
        } => {
            !matches!(
                types.effects().operation_mode(*operation),
                Some(crate::sema::EffectMode::Normal)
            ) || arguments
                .iter()
                .any(|argument| contains_aborting_effect(&argument.value, function_bodies, types))
        }
        CoreExprKind::Call {
            callee, arguments, ..
        } => {
            callee_has_aborting_effect(callee, function_bodies, types)
                || contains_aborting_effect(callee, function_bodies, types)
                || arguments.iter().any(|argument| {
                    contains_aborting_effect(&argument.value, function_bodies, types)
                })
        }
        CoreExprKind::Binary { left, right, .. } => {
            contains_aborting_effect(left, function_bodies, types)
                || contains_aborting_effect(right, function_bodies, types)
        }
        CoreExprKind::Unary { expression, .. }
        | CoreExprKind::Field {
            value: expression, ..
        }
        | CoreExprKind::LetPattern {
            value: expression, ..
        }
        | CoreExprKind::Let {
            value: expression, ..
        } => contains_aborting_effect(expression, function_bodies, types),
        CoreExprKind::Block(values) | CoreExprKind::Tuple(values) => values
            .iter()
            .any(|value| contains_aborting_effect(value, function_bodies, types)),
        CoreExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            contains_aborting_effect(condition, function_bodies, types)
                || contains_aborting_effect(then_branch, function_bodies, types)
                || contains_aborting_effect(else_branch, function_bodies, types)
        }
        _ => false,
    }
}
