use super::*;
use std::collections::HashSet;

pub(crate) fn default_debug_name(ty: Type) -> String {
    format!("@debug/default/{ty:?}")
}

pub(super) fn materialize_default_debug<'a>(
    functions: &mut Vec<CoreFunction>,
    types: &CheckedTypes,
    defaults: impl IntoIterator<Item = &'a CoreExpr>,
) {
    fn collect(expr: &CoreExpr, roots: &mut Vec<(Type, NodeId)>) {
        if let CoreExprKind::Call {
            callee, arguments, ..
        } = &expr.kind
        {
            match &callee.kind {
                CoreExprKind::Name(name) if name == "echo" || name == "@debug" => {
                    roots.push((arguments[0].value.ty, expr.id))
                }
                CoreExprKind::Field {
                    value,
                    access: FieldAccess::Name(name),
                } if name == crate::sema::DEBUG_METHOD => roots.push((value.ty, expr.id)),
                _ => {}
            }
        }
        super::analysis::visit_core_children(expr, &mut |child| collect(child, roots));
    }
    let mut roots = Vec::new();
    for f in functions.iter() {
        collect(&f.body, &mut roots);
    }
    for expr in defaults {
        collect(expr, &mut roots);
    }
    let mut seen = HashSet::new();
    let mut cursor = 0;
    while cursor < roots.len() {
        let (ty, node) = roots[cursor];
        cursor += 1;
        if !seen.insert(ty) || types.has_explicit_debug(ty) || !types.supports_debug(ty) {
            continue;
        }
        if let Some(members) = types.debug_members(ty) {
            roots.extend(members.into_iter().map(|ty| (ty, node)));
        }
        if !types.has_default_debug(ty) {
            continue;
        }
        functions.push(CoreFunction {
            closure_state: None,
            foreign: None,
            id: CoreFunctionId(functions.len()),
            module: CoreModuleId(0),
            name: default_debug_name(ty),
            visibility: Visibility::Private,
            receiver: None,
            receiver_mode: crate::syntax::ReceiverMode::Borrowed,
            parameters: vec![
                CoreParameter {
                    name: "@debug/value".into(),
                    ty,
                },
                CoreParameter {
                    name: "@debug/parent".into(),
                    ty: Type::Bytes,
                },
            ],
            borrowed_parameters: 0,
            parameter_ownership: vec![Some(CoreParameterOwnership::Borrowed), None],
            return_type: Type::String,
            declared_effects: crate::sema::EffectGroupSet::new(),
            used_effects: EffectSet::new(),
            may_suspend: false,
            body: CoreExpr {
                id: node,
                ty: Type::String,
                kind: CoreExprKind::Call {
                    callee: Box::new(CoreExpr {
                        id: node,
                        ty: Type::Unit,
                        kind: CoreExprKind::Name("@debug/body".into()),
                    }),
                    arguments: vec![],
                    type_arguments: vec![],
                    effect_operation: None,
                },
            },
        });
    }
}
