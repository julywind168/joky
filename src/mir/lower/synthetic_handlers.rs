use std::collections::{hash_map::Entry, HashMap, HashSet};

use crate::hir::{
    CoreCollectionLiteral, CoreExpr, CoreExprKind, CoreFunction, CoreFunctionId, CoreHandlerArm,
    CoreModuleId, CoreParameter, CoreParameterOwnership, CorePattern,
};
use crate::mir::{ownership_for_type, MirFunctionId, MirOwnership};
use crate::sema::{CheckedTypes, EffectGroupSet, EffectSet, Type};
use crate::syntax::{FieldAccess, Visibility};

pub(super) struct SyntheticHandlerSpec {
    pub(super) id: MirFunctionId,
    pub(super) function: CoreFunction,
}

pub(super) fn collect_synthetic_handlers(
    expression: &CoreExpr,
    module: CoreModuleId,
    types: &CheckedTypes,
    known_function_names: &HashSet<String>,
    ids: &mut HashMap<crate::syntax::NodeId, MirFunctionId>,
    specs: &mut Vec<SyntheticHandlerSpec>,
    next_function_id: &mut usize,
) {
    match &expression.kind {
        CoreExprKind::Do { body, handlers } => {
            collect_synthetic_handlers(
                body,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            for handler in handlers {
                if synthetic_handler_supported(handler, types, known_function_names) {
                    match ids.entry(handler.value.id) {
                        Entry::Occupied(_) => {}
                        Entry::Vacant(entry) => {
                            let id = MirFunctionId(*next_function_id);
                            *next_function_id += 1;
                            let name = format!("__handler_{}", id.0);
                            entry.insert(id);
                            let parameters: Vec<CoreParameter> = types
                                .effects()
                                .operation_info(handler.operation)
                                .map(|info| {
                                    info.parameters
                                        .iter()
                                        .enumerate()
                                        .map(|(index, ty)| CoreParameter {
                                            name: handler
                                                .parameters
                                                .get(index)
                                                .and_then(|pattern| match pattern {
                                                    CorePattern::Binding { name, .. } => {
                                                        Some(name.clone())
                                                    }
                                                    _ => None,
                                                })
                                                .unwrap_or_else(|| format!("__handler_arg{index}")),
                                            ty: *ty,
                                        })
                                        .chain(
                                            synthetic_handler_captures(
                                                handler,
                                                types,
                                                known_function_names,
                                            )
                                            .into_iter()
                                            .map(|(name, ty)| CoreParameter { name, ty }),
                                        )
                                        .collect()
                                })
                                .unwrap_or_default();
                            let operation_parameter_count = types
                                .effects()
                                .operation_info(handler.operation)
                                .map_or(0, |info| info.parameters.len());
                            let parameter_ownership = parameters
                                .iter()
                                .enumerate()
                                .map(|(index, parameter)| {
                                    if index < operation_parameter_count
                                        && types.is_owned(parameter.ty)
                                        && types
                                            .effects()
                                            .operation_info(handler.operation)
                                            .is_some_and(|info| {
                                                info.parameter_borrows.get(index) != Some(&true)
                                            })
                                    {
                                        Some(CoreParameterOwnership::Owned)
                                    } else {
                                        Some(CoreParameterOwnership::Borrowed)
                                    }
                                })
                                .collect();
                            specs.push(SyntheticHandlerSpec {
                                id,
                                function: CoreFunction {
                                    closure_state: None,
                                    foreign: None,
                                    id: CoreFunctionId(id.0),
                                    module,
                                    name,
                                    visibility: Visibility::Private,
                                    receiver: None,
                                    receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                                    parameters,
                                    borrowed_parameters: 0,
                                    parameter_ownership,
                                    return_type: handler.value.ty,
                                    declared_effects: EffectGroupSet::default(),
                                    used_effects: EffectSet::new(),
                                    may_suspend: false,
                                    body: handler.value.clone(),
                                },
                            });
                        }
                    }
                }
                collect_synthetic_handlers(
                    &handler.value,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
        }
        CoreExprKind::Let { value, .. }
        | CoreExprKind::LetPattern { value, .. }
        | CoreExprKind::Unary {
            expression: value, ..
        }
        | CoreExprKind::Unwrap { value, .. }
        | CoreExprKind::Cast { value, .. }
        | CoreExprKind::Abort { value }
        | CoreExprKind::Region(value)
        | CoreExprKind::Branch(value) => collect_synthetic_handlers(
            value,
            module,
            types,
            known_function_names,
            ids,
            specs,
            next_function_id,
        ),
        CoreExprKind::Binary { left, right, .. } => {
            collect_synthetic_handlers(
                left,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            collect_synthetic_handlers(
                right,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
        }
        CoreExprKind::Call {
            callee, arguments, ..
        } => {
            collect_synthetic_handlers(
                callee,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            for argument in arguments {
                collect_synthetic_handlers(
                    &argument.value,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
        }
        CoreExprKind::Closure { body, .. } => collect_synthetic_handlers(
            body,
            module,
            types,
            known_function_names,
            ids,
            specs,
            next_function_id,
        ),
        CoreExprKind::Tuple(values)
        | CoreExprKind::Parallel(values)
        | CoreExprKind::Race(values)
        | CoreExprKind::Block(values) => {
            for value in values {
                collect_synthetic_handlers(
                    value,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
        }
        CoreExprKind::CollectionLiteral(literal) => match literal {
            CoreCollectionLiteral::List(values)
            | CoreCollectionLiteral::MutList(values)
            | CoreCollectionLiteral::Set(values)
            | CoreCollectionLiteral::MutSet(values) => {
                for value in values {
                    collect_synthetic_handlers(
                        value,
                        module,
                        types,
                        known_function_names,
                        ids,
                        specs,
                        next_function_id,
                    );
                }
            }
            CoreCollectionLiteral::Map(entries) | CoreCollectionLiteral::MutMap(entries) => {
                for entry in entries {
                    collect_synthetic_handlers(
                        &entry.key,
                        module,
                        types,
                        known_function_names,
                        ids,
                        specs,
                        next_function_id,
                    );
                    collect_synthetic_handlers(
                        &entry.value,
                        module,
                        types,
                        known_function_names,
                        ids,
                        specs,
                        next_function_id,
                    );
                }
            }
        },
        CoreExprKind::StructInit { fields, .. } => {
            for (_, value) in fields {
                collect_synthetic_handlers(
                    value,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
        }
        CoreExprKind::Field { value, .. } => collect_synthetic_handlers(
            value,
            module,
            types,
            known_function_names,
            ids,
            specs,
            next_function_id,
        ),
        CoreExprKind::Assign { target, value }
        | CoreExprKind::CompoundAssign { target, value, .. } => {
            collect_synthetic_handlers(
                target,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            collect_synthetic_handlers(
                value,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
        }
        CoreExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_synthetic_handlers(
                condition,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            collect_synthetic_handlers(
                then_branch,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            collect_synthetic_handlers(
                else_branch,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
        }
        CoreExprKind::Match { value, arms } => {
            collect_synthetic_handlers(
                value,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            for arm in arms {
                collect_synthetic_handlers(
                    &arm.value,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
        }
        CoreExprKind::For {
            iterable,
            body,
            limit,
            ..
        } => {
            for child in std::iter::once(iterable.as_ref())
                .chain(limit.as_deref())
                .chain(std::iter::once(body.as_ref()))
            {
                collect_synthetic_handlers(
                    child,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
        }
        CoreExprKind::ForWorker { body, .. } => collect_synthetic_handlers(
            body,
            module,
            types,
            known_function_names,
            ids,
            specs,
            next_function_id,
        ),
        CoreExprKind::While { condition, body } => {
            collect_synthetic_handlers(
                condition,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
            collect_synthetic_handlers(
                body,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
        }
        CoreExprKind::Loop { body } => collect_synthetic_handlers(
            body,
            module,
            types,
            known_function_names,
            ids,
            specs,
            next_function_id,
        ),
        CoreExprKind::Break { value } => {
            if let Some(value) = value {
                collect_synthetic_handlers(
                    value,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
        }
        CoreExprKind::When {
            cowns, until, body, ..
        } => {
            if let Some(until) = until {
                collect_synthetic_handlers(
                    until,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
            for cown in cowns {
                collect_synthetic_handlers(
                    cown,
                    module,
                    types,
                    known_function_names,
                    ids,
                    specs,
                    next_function_id,
                );
            }
            collect_synthetic_handlers(
                body,
                module,
                types,
                known_function_names,
                ids,
                specs,
                next_function_id,
            );
        }
        CoreExprKind::Unit
        | CoreExprKind::Integer(_)
        | CoreExprKind::Float(_)
        | CoreExprKind::Duration(_)
        | CoreExprKind::String(_)
        | CoreExprKind::Bytes(_)
        | CoreExprKind::Boolean(_)
        | CoreExprKind::Name(_)
        | CoreExprKind::ExternalSymbol(_)
        | CoreExprKind::Continue => {}
        CoreExprKind::InterpolatedString(parts) => {
            for (_, expression) in parts {
                if let Some(expression) = expression {
                    collect_synthetic_handlers(
                        expression,
                        module,
                        types,
                        known_function_names,
                        ids,
                        specs,
                        next_function_id,
                    );
                }
            }
        }
    }
}

fn synthetic_handler_supported(
    handler: &CoreHandlerArm,
    types: &CheckedTypes,
    known_function_names: &HashSet<String>,
) -> bool {
    let Some(operation) = types.effects().operation_info(handler.operation) else {
        return false;
    };
    // Copy-only forwarded parameters use a compact runtime thunk. Managed
    // parameters need the typed synthetic function so request and result
    // ownership can move through the dynamic handler frame.
    let uses_copy_forward_thunk = handler.resumes
        && super::resumable_forward_parameter(&handler.value, &handler.parameters, types)
            .is_some_and(|index| {
                operation
                    .parameters
                    .get(index)
                    .is_some_and(|ty| ownership_for_type(*ty, types) == MirOwnership::Copy)
            });
    if handler.parameters.len() != operation.parameters.len()
        || uses_copy_forward_thunk
        || !synthetic_handler_expression(&handler.value, types, known_function_names)
    {
        return false;
    }
    if handler.parameters.iter().any(|pattern| {
        !matches!(
            pattern,
            CorePattern::Binding { .. } | CorePattern::Wildcard { .. }
        )
    }) {
        return false;
    }
    synthetic_handler_captures(handler, types, known_function_names)
        .iter()
        .all(|(_, ty)| {
            matches!(ty, Type::I32 | Type::I64 | Type::F64 | Type::Bool)
                || matches!(ty, Type::Class(_))
                || types.is_shared(*ty)
                || types.is_owned(*ty)
        })
}

pub(super) fn synthetic_handler_captures(
    handler: &CoreHandlerArm,
    types: &CheckedTypes,
    known_function_names: &HashSet<String>,
) -> Vec<(String, Type)> {
    let parameter_names = handler
        .parameters
        .iter()
        .filter_map(|pattern| match pattern {
            CorePattern::Binding { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    crate::hir::task_captures(&handler.value)
        .into_iter()
        .filter(|(name, _)| {
            !parameter_names.contains(name.as_str())
                && !known_function_names.contains(name)
                && types.struct_type(name).is_none()
                && types.class_type(name).is_none()
                && types.enum_type(name).is_none()
        })
        .collect()
}

fn synthetic_handler_expression(
    expression: &CoreExpr,
    types: &CheckedTypes,
    known_function_names: &HashSet<String>,
) -> bool {
    match &expression.kind {
        CoreExprKind::Integer(_)
        | CoreExprKind::Float(_)
        | CoreExprKind::Duration(_)
        | CoreExprKind::String(_)
        | CoreExprKind::Bytes(_)
        | CoreExprKind::Boolean(_)
        | CoreExprKind::Name(_) => true,
        CoreExprKind::InterpolatedString(parts) => parts.iter().all(|(_, expression)| {
            expression.as_deref().is_none_or(|expression| {
                synthetic_handler_expression(expression, types, known_function_names)
            })
        }),
        CoreExprKind::Unary { expression, .. }
        | CoreExprKind::Unwrap {
            value: expression, ..
        }
        | CoreExprKind::Cast {
            value: expression, ..
        } => synthetic_handler_expression(expression, types, known_function_names),
        CoreExprKind::Binary { left, right, .. } => {
            synthetic_handler_expression(left, types, known_function_names)
                && synthetic_handler_expression(right, types, known_function_names)
        }
        CoreExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            synthetic_handler_expression(condition, types, known_function_names)
                && synthetic_handler_expression(then_branch, types, known_function_names)
                && synthetic_handler_expression(else_branch, types, known_function_names)
        }
        CoreExprKind::Let { value, .. } | CoreExprKind::LetPattern { value, .. } => {
            synthetic_handler_expression(value, types, known_function_names)
        }
        CoreExprKind::Block(values) => values
            .iter()
            .all(|value| synthetic_handler_expression(value, types, known_function_names)),
        CoreExprKind::Tuple(values) => values
            .iter()
            .all(|value| synthetic_handler_expression(value, types, known_function_names)),
        CoreExprKind::CollectionLiteral(collection) => match collection {
            CoreCollectionLiteral::List(values)
            | CoreCollectionLiteral::MutList(values)
            | CoreCollectionLiteral::Set(values)
            | CoreCollectionLiteral::MutSet(values) => values
                .iter()
                .all(|value| synthetic_handler_expression(value, types, known_function_names)),
            CoreCollectionLiteral::Map(entries) | CoreCollectionLiteral::MutMap(entries) => {
                entries.iter().all(|entry| {
                    synthetic_handler_expression(&entry.key, types, known_function_names)
                        && synthetic_handler_expression(&entry.value, types, known_function_names)
                })
            }
        },
        CoreExprKind::StructInit { fields, .. } => fields
            .iter()
            .all(|(_, value)| synthetic_handler_expression(value, types, known_function_names)),
        CoreExprKind::Match { value, arms } => {
            synthetic_handler_expression(value, types, known_function_names)
                && arms.iter().all(|arm| {
                    synthetic_handler_expression(&arm.value, types, known_function_names)
                })
        }
        CoreExprKind::Assign { target, value }
        | CoreExprKind::CompoundAssign { target, value, .. } => {
            synthetic_handler_expression(target, types, known_function_names)
                && synthetic_handler_expression(value, types, known_function_names)
        }
        CoreExprKind::Call {
            callee,
            arguments,
            effect_operation,
            ..
        } => {
            if effect_operation.is_some() {
                return false;
            }
            let callee_is_pure = match &callee.kind {
                CoreExprKind::Name(name) => {
                    (matches!(name.as_str(), "print" | "println")
                        || (known_function_names.contains(name)
                            && types
                                .function_effects(name)
                                .is_some_and(|effects| effects.is_empty())))
                        || types.struct_type(name).is_some()
                        || types.class_type(name).is_some()
                        || types.enum_type(name).is_some()
                        || matches!(name.as_str(), "Some" | "None" | "Ok" | "Err")
                }
                CoreExprKind::Field { value, access } => {
                    let FieldAccess::Name(method) = access else {
                        return false;
                    };
                    let key = match value.ty {
                        Type::Struct(id) => format!("struct_{id}_{method}"),
                        Type::Class(id) => format!("class_{id}_{method}"),
                        _ => return false,
                    };
                    types
                        .function_effects(&key)
                        .is_some_and(|effects| effects.is_empty())
                }
                CoreExprKind::Closure { effects, .. } => effects.is_empty(),
                _ => false,
            };
            callee_is_pure
                && synthetic_handler_expression(callee, types, known_function_names)
                && arguments.iter().all(|argument| {
                    synthetic_handler_expression(&argument.value, types, known_function_names)
                })
        }
        CoreExprKind::Field { value, .. } => {
            synthetic_handler_expression(value, types, known_function_names)
        }
        _ => false,
    }
}
