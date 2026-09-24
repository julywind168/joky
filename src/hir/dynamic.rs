use super::*;

pub(crate) fn dynamic_method_name(dynamic: usize, concrete: Type, slot: usize) -> String {
    format!("@dyn/{dynamic}/{concrete:?}/{slot}")
}

pub(super) fn materialize_dynamic_methods<'a>(
    functions: &mut Vec<CoreFunction>,
    types: &CheckedTypes,
    defaults: impl IntoIterator<Item = &'a CoreExpr>,
) {
    fn collect(expr: &CoreExpr, found: &mut Vec<(usize, Type, NodeId)>) {
        if let CoreExprKind::Call {
            callee, arguments, ..
        } = &expr.kind
        {
            if matches!(&callee.kind, CoreExprKind::Name(name) if name == "@dyn") {
                if let Type::Dyn(id) = expr.ty {
                    if !matches!(arguments[0].value.ty, Type::Dyn(_)) {
                        found.push((id, arguments[0].value.ty, expr.id));
                    }
                }
            }
        }
        super::analysis::visit_core_children(expr, &mut |child| collect(child, found));
    }
    let mut found = Vec::new();
    for function in functions.iter() {
        collect(&function.body, &mut found);
    }
    for default in defaults {
        collect(default, &mut found);
    }
    for (dynamic, concrete, node) in found {
        for (slot, method) in types.dynamic_types[dynamic].methods.iter().enumerate() {
            let name = dynamic_method_name(dynamic, concrete, slot);
            if functions.iter().any(|f| f.name == name) {
                continue;
            }
            let signature = types.function_type(method.signature);
            let mut parameters = vec![CoreParameter {
                name: "self".into(),
                ty: concrete,
            }];
            parameters.extend(
                signature
                    .parameter_names
                    .iter()
                    .zip(&signature.parameters)
                    .map(|(name, ty)| CoreParameter {
                        name: name.clone(),
                        ty: *ty,
                    }),
            );
            let receiver = CoreExpr {
                id: node,
                ty: concrete,
                kind: CoreExprKind::Name("self".into()),
            };
            let arguments = parameters[1..]
                .iter()
                .map(|p| CoreCallArgument {
                    label: Some(p.name.clone()),
                    value: CoreExpr {
                        id: node,
                        ty: p.ty,
                        kind: CoreExprKind::Name(p.name.clone()),
                    },
                })
                .collect();
            let (callee, arguments) = if method.implementation_name == crate::sema::DEBUG_METHOD
                && types.dynamic_types[dynamic].includes("Debug")
                && !types.has_explicit_debug(concrete)
            {
                (
                    CoreExprKind::Name("@debug".into()),
                    vec![CoreCallArgument {
                        label: None,
                        value: receiver,
                    }],
                )
            } else if let Some(symbol) = types
                .interface
                .method_symbols
                .get(&(concrete, method.implementation_name.clone()))
            {
                let mut args: Vec<CoreCallArgument> = arguments;
                args.insert(
                    0,
                    CoreCallArgument {
                        label: None,
                        value: receiver,
                    },
                );
                (CoreExprKind::ExternalSymbol(symbol.clone()), args)
            } else {
                (
                    CoreExprKind::Field {
                        value: Box::new(receiver),
                        access: FieldAccess::Name(method.implementation_name.clone()),
                    },
                    arguments,
                )
            };
            let ownership = if method.receiver == crate::syntax::ReceiverMode::Owned {
                CoreParameterOwnership::Owned
            } else {
                CoreParameterOwnership::Borrowed
            };
            let mut parameter_ownership = vec![(method.receiver
                == crate::syntax::ReceiverMode::Borrowed
                || types.is_owned(concrete))
            .then_some(ownership)];
            parameter_ownership.extend(
                method
                    .parameter_borrows
                    .iter()
                    .zip(&signature.parameters)
                    .map(|(borrowed, ty)| {
                        (*borrowed && types.is_owned(*ty))
                            .then_some(CoreParameterOwnership::Borrowed)
                    }),
            );
            let mut declared_effects = crate::sema::EffectGroupSet::new();
            for operation in signature.effects.iter() {
                declared_effects.insert(operation.effect);
            }
            functions.push(CoreFunction {
                closure_state: None,
                foreign: None,
                id: CoreFunctionId(functions.len()),
                module: CoreModuleId(0),
                name,
                visibility: Visibility::Private,
                receiver: None,
                receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                parameters,
                borrowed_parameters: usize::from(
                    method.receiver == crate::syntax::ReceiverMode::Borrowed,
                ),
                parameter_ownership,
                return_type: signature.return_type,
                declared_effects,
                used_effects: signature.effects.clone(),
                may_suspend: true,
                body: CoreExpr {
                    id: node,
                    ty: signature.return_type,
                    kind: CoreExprKind::Call {
                        callee: Box::new(CoreExpr {
                            id: node,
                            ty: Type::Unit,
                            kind: callee,
                        }),
                        type_arguments: Vec::new(),
                        arguments,
                        effect_operation: None,
                    },
                },
            });
        }
    }
}
