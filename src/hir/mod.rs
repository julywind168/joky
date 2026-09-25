//! First stage of the typed HIR / Core IR implementation.
//!
//! CoreProgram is the stable boundary between semantic analysis and code
//! generation: a normalized tree with Types for every expression, which the
//! CFG-form MIR then continues lowering.

use crate::diagnostic::Diagnostic;
use crate::module::SymbolId;
use crate::sema::{CheckedTypes, EffectOperationId, EffectSet, Type};
use crate::syntax::{
    BinaryOp, CallArgument, CastMode, CollectionLiteral, Expr, ExprKind, FieldAccess, MatchArm,
    NodeId, Pattern, UnaryOp, Visibility,
};
use crate::Span;

mod analysis;
mod constants;
mod debug;
mod dynamic;
mod sorting;
pub(crate) use debug::default_debug_name;
pub(crate) use sorting::sort_function_name;
mod retain;
pub(crate) use retain::retain_function_name;
mod lower;
use constants::lower_imported_constant;
pub(crate) use dynamic::dynamic_method_name;

pub(crate) use analysis::{
    is_single_effect_call, iteration_has_branches, task_captures, task_function_name,
};
#[cfg(test)]
pub(super) use lower::module_id_for_function;
#[cfg(test)]
mod tests;

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct CoreProgram {
    pub(crate) source_spans: std::collections::HashMap<NodeId, Span>,
    types: CheckedTypes,
    functions: Vec<CoreFunction>,
    struct_defaults: Vec<Vec<Option<CoreExpr>>>,
    class_defaults: Vec<Vec<Option<CoreExpr>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CoreFunctionId(pub(crate) usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CoreModuleId(pub(crate) usize);

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreClosureState {
    pub(crate) ty: Type,
    pub(crate) captures: Vec<crate::sema::ClosureCaptureBinding>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreFunction {
    pub(crate) closure_state: Option<CoreClosureState>,
    pub(crate) foreign: Option<crate::syntax::ForeignFunction>,
    pub(crate) id: CoreFunctionId,
    pub(crate) module: CoreModuleId,
    pub(crate) name: String,
    pub(crate) visibility: Visibility,
    pub(crate) receiver: Option<Type>,
    pub(crate) receiver_mode: crate::syntax::ReceiverMode,
    pub(crate) parameters: Vec<CoreParameter>,
    pub(crate) borrowed_parameters: usize,
    /// Explicit ownership for generated functions whose parameter list mixes
    /// operation arguments and captured values. `None` keeps the legacy
    /// borrowed-prefix/inferred behavior.
    pub(crate) parameter_ownership: Vec<Option<CoreParameterOwnership>>,
    pub(crate) return_type: Type,
    pub(crate) declared_effects: crate::sema::EffectGroupSet,
    pub(crate) used_effects: EffectSet,
    pub(crate) may_suspend: bool,
    pub(crate) body: CoreExpr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreParameterOwnership {
    Borrowed,
    Owned,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreParameter {
    pub(crate) name: String,
    pub(crate) ty: Type,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreHandlerArm {
    pub(crate) operation: EffectOperationId,
    pub(crate) parameters: Vec<CorePattern>,
    /// A resumable handler's returned value is used to continue the operation
    /// at its call site; MIR carries the control transfer explicitly.
    pub(crate) resumes: bool,
    /// `abort(value)` is represented by its value; the handler terminates the
    /// current `do` expression instead of resuming the operation.
    pub(crate) aborts: bool,
    pub(crate) value: CoreExpr,
    pub(crate) span: Span,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreExpr {
    pub(crate) id: NodeId,
    pub(crate) ty: Type,
    pub(crate) kind: CoreExprKind,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum CoreExprKind {
    Unit,
    Integer(u64),
    Float(f64),
    Duration(u64),
    String(String),
    /// Byte string literal; lowered to a String constant followed by the
    /// `BytesFromString` runtime call
    Bytes(String),
    InterpolatedString(Vec<(String, Option<Box<CoreExpr>>)>),
    Boolean(bool),
    Name(String),
    ExternalSymbol(SymbolId),
    Let {
        name: String,
        mutable: bool,
        annotation: Option<Type>,
        value: Box<CoreExpr>,
    },
    LetPattern {
        pattern: Box<CorePattern>,
        binding_ids: Vec<(String, NodeId)>,
        mutable: bool,
        value: Box<CoreExpr>,
    },
    Unary {
        op: UnaryOp,
        expression: Box<CoreExpr>,
    },
    Unwrap {
        value: Box<CoreExpr>,
        propagate: bool,
    },
    Cast {
        value: Box<CoreExpr>,
        mode: CastMode,
        /// The conversion target as checked by sema; for the checked mode
        /// this is the `Ok` payload, not the expression's `Result` type.
        target: Type,
    },
    Binary {
        op: BinaryOp,
        left: Box<CoreExpr>,
        right: Box<CoreExpr>,
    },
    Call {
        callee: Box<CoreExpr>,
        type_arguments: Vec<Type>,
        arguments: Vec<CoreCallArgument>,
        effect_operation: Option<EffectOperationId>,
    },
    Closure {
        move_capture: bool,
        function_name: String,
        captures: Vec<(String, Type)>,
        capture_bindings: Vec<crate::sema::ClosureCaptureBinding>,
        parameters: Vec<(String, Type)>,
        return_type: Type,
        effects: EffectSet,
        body: Box<CoreExpr>,
    },
    Tuple(Vec<CoreExpr>),
    CollectionLiteral(CoreCollectionLiteral),
    StructInit {
        name: String,
        fields: Vec<(String, CoreExpr)>,
    },
    Field {
        value: Box<CoreExpr>,
        access: FieldAccess,
    },
    Assign {
        target: Box<CoreExpr>,
        value: Box<CoreExpr>,
    },
    CompoundAssign {
        target: Box<CoreExpr>,
        operator: BinaryOp,
        value: Box<CoreExpr>,
    },
    If {
        condition: Box<CoreExpr>,
        then_branch: Box<CoreExpr>,
        else_branch: Box<CoreExpr>,
    },
    Match {
        value: Box<CoreExpr>,
        arms: Vec<CoreMatchArm>,
    },
    For {
        index: Option<String>,
        item: String,
        iterable: Box<CoreExpr>,
        body: Box<CoreExpr>,
        limit: Option<Box<CoreExpr>>,
    },
    ForWorker {
        index: Option<String>,
        item: String,
        input_type: Type,
        state: String,
        body: Box<CoreExpr>,
        collect: bool,
    },
    While {
        condition: Box<CoreExpr>,
        body: Box<CoreExpr>,
    },
    Loop {
        body: Box<CoreExpr>,
    },
    Break {
        value: Option<Box<CoreExpr>>,
    },
    Abort {
        value: Box<CoreExpr>,
    },
    Continue,
    When {
        cowns: Vec<CoreExpr>,
        bindings: Option<Vec<CorePattern>>,
        until: Option<Box<CoreExpr>>,
        body: Box<CoreExpr>,
    },
    Do {
        body: Box<CoreExpr>,
        handlers: Vec<CoreHandlerArm>,
    },
    Parallel(Vec<CoreExpr>),
    Race(Vec<CoreExpr>),
    Branch(Box<CoreExpr>),
    Region(Box<CoreExpr>),
    Block(Vec<CoreExpr>),
}

type ClosureFunction = (
    String,
    Vec<(String, Type)>,
    Vec<crate::sema::ClosureCaptureBinding>,
    Vec<(String, Type)>,
    Type,
    EffectSet,
    CoreExpr,
);
type TaskFunction = (String, CoreModuleId, Vec<(String, Type)>, Type, CoreExpr);

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum CoreCollectionLiteral {
    List(Vec<CoreExpr>),
    MutList(Vec<CoreExpr>),
    Set(Vec<CoreExpr>),
    MutSet(Vec<CoreExpr>),
    Map(Vec<CoreMapLiteralEntry>),
    MutMap(Vec<CoreMapLiteralEntry>),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreMapLiteralEntry {
    pub(crate) key: CoreExpr,
    pub(crate) value: CoreExpr,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreCallArgument {
    pub(crate) label: Option<String>,
    pub(crate) value: CoreExpr,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct CoreMatchArm {
    pub(crate) pattern: CorePattern,
    pub(crate) value: CoreExpr,
    pub(crate) span: Span,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CorePattern {
    Wildcard {
        span: Span,
    },
    EnumVariant {
        enum_name: String,
        variant: String,
        fields: Vec<CorePatternField>,
        span: Span,
    },
    Binding {
        name: String,
        span: Span,
    },
    Tuple {
        elements: Vec<CorePattern>,
        span: Span,
    },
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CorePatternField {
    pub(crate) label: Option<String>,
    pub(crate) pattern: CorePattern,
    pub(crate) span: Span,
}

fn lower_expr_type(
    expression: &Expr,
    types: &CheckedTypes,
    substitutions: &[Type],
) -> Result<Type, Diagnostic> {
    types
        .substitute(
            types.get_optional(expression).unwrap_or(Type::Unit),
            substitutions,
        )
        .ok_or_else(|| Diagnostic::codegen("generic expression type was not concrete"))
}

fn lower_binary_expr(
    expression: &Expr,
    types: &CheckedTypes,
    substitutions: &[Type],
    type_parameters: &[String],
) -> Result<CoreExpr, Diagnostic> {
    let mut stack = Vec::new();
    let mut current = expression;
    while let ExprKind::Binary { op, left, right } = &current.kind {
        stack.push((current, *op, right.as_ref()));
        current = left.as_ref();
    }
    let mut left = lower_expr(current, types, substitutions, type_parameters)?;
    for (node, op, right) in stack.into_iter().rev() {
        let right = lower_expr(right, types, substitutions, type_parameters)?;
        let kind = if op.is_comparison() {
            lower_comparison(node.id, op, left, right, types)
        } else {
            CoreExprKind::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        };
        left = CoreExpr {
            id: node.id,
            ty: lower_expr_type(node, types, substitutions)?,
            kind,
        };
    }
    Ok(left)
}

fn lower_expr(
    expression: &Expr,
    types: &CheckedTypes,
    substitutions: &[Type],
    type_parameters: &[String],
) -> Result<CoreExpr, Diagnostic> {
    if let Some(value) = types.imported_constants.get(&expression.id) {
        return Ok(lower_imported_constant(value, expression.id, types));
    }
    let kind = match &expression.kind {
        ExprKind::Integer(value) | ExprKind::TypedInteger(value, _) => {
            CoreExprKind::Integer(*value)
        }
        ExprKind::Float(value) => CoreExprKind::Float(*value),
        ExprKind::Duration(value) => CoreExprKind::Duration(*value),
        ExprKind::String(value) => CoreExprKind::String(value.clone()),
        ExprKind::Bytes(value) => CoreExprKind::Bytes(value.clone()),
        ExprKind::InterpolatedString(parts) => CoreExprKind::InterpolatedString(
            parts
                .iter()
                .map(|(literal, expression)| {
                    Ok((
                        literal.clone(),
                        expression
                            .as_ref()
                            .map(|expression| {
                                lower_expr(expression, types, substitutions, type_parameters)
                            })
                            .transpose()?
                            .map(Box::new),
                    ))
                })
                .collect::<Result<Vec<_>, Diagnostic>>()?,
        ),
        ExprKind::Boolean(value) => CoreExprKind::Boolean(*value),
        ExprKind::Name(name) => {
            if let Some(symbol) = types.external_symbol(expression.id) {
                CoreExprKind::ExternalSymbol(symbol.clone())
            } else if let Some(constant) = types
                .constant_reference(expression.id)
                .and_then(|name| types.constant_value(name))
            {
                return lower_expr(constant, types, substitutions, type_parameters);
            } else {
                CoreExprKind::Name(name.clone())
            }
        }
        ExprKind::Field { .. } if types.external_symbol(expression.id).is_some() => {
            CoreExprKind::ExternalSymbol(
                types
                    .external_symbol(expression.id)
                    .expect("field import was type-checked")
                    .clone(),
            )
        }
        ExprKind::Let { .. } if types.is_compile_time_binding(expression.id) => CoreExprKind::Unit,
        ExprKind::Let {
            name,
            mutable,
            annotation,
            value,
        } => {
            let value = lower_expr(value, types, substitutions, type_parameters)?;
            CoreExprKind::Let {
                name: name.clone(),
                mutable: *mutable,
                annotation: annotation.as_ref().map(|_| value.ty),
                value: Box::new(value),
            }
        }
        ExprKind::LetPattern {
            pattern,
            binding_ids,
            mutable,
            value,
        } => CoreExprKind::LetPattern {
            pattern: Box::new(lower_pattern(pattern)),
            binding_ids: binding_ids.clone(),
            mutable: *mutable,
            value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
        },
        ExprKind::AnonymousStruct { .. } => {
            return Err(Diagnostic::codegen(
                "anonymous struct type escaped compile-time evaluation",
            ));
        }
        ExprKind::AnonymousEnum { .. } => {
            return Err(Diagnostic::codegen(
                "anonymous enum type escaped compile-time evaluation",
            ));
        }
        ExprKind::Unary { op, expression } => CoreExprKind::Unary {
            op: *op,
            expression: Box::new(lower_expr(
                expression,
                types,
                substitutions,
                type_parameters,
            )?),
        },
        ExprKind::Unwrap { value, propagate } => CoreExprKind::Unwrap {
            value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
            propagate: *propagate,
        },
        ExprKind::Cast { value, mode, .. } => {
            // Sema stores the cast node's type as `target` for the lossless
            // and wrapping modes, and as `Result(target, String)` for the
            // checked mode; recover the target from the checked node type.
            let node_ty = lower_expr_type(expression, types, substitutions)?;
            let target = match node_ty {
                Type::Result(id) => types.result_types(id).0,
                ty => ty,
            };
            CoreExprKind::Cast {
                value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
                mode: *mode,
                target,
            }
        }
        ExprKind::Range {
            start,
            end,
            step,
            inclusive,
        } => {
            let start = lower_expr(start, types, substitutions, type_parameters)?;
            let end = lower_expr(end, types, substitutions, type_parameters)?;
            let step = match step {
                Some(step) => lower_expr(step, types, substitutions, type_parameters)?,
                None => CoreExpr {
                    id: expression.id,
                    ty: start.ty,
                    kind: CoreExprKind::Integer(1),
                },
            };
            CoreExprKind::StructInit {
                name: types
                    .struct_name(match types.get(expression) {
                        Type::Struct(id) => id,
                        _ => unreachable!("range type"),
                    })
                    .to_owned(),
                fields: vec![
                    ("@current".into(), start),
                    ("@end".into(), end),
                    ("@step".into(), step),
                    (
                        "@inclusive".into(),
                        CoreExpr {
                            id: expression.id,
                            ty: Type::Bool,
                            kind: CoreExprKind::Boolean(*inclusive),
                        },
                    ),
                    (
                        "@exhausted".into(),
                        CoreExpr {
                            id: expression.id,
                            ty: Type::Bool,
                            kind: CoreExprKind::Boolean(false),
                        },
                    ),
                ],
            }
        }
        ExprKind::Binary { .. } => {
            return lower_binary_expr(expression, types, substitutions, type_parameters);
        }
        ExprKind::Call { callee, arguments } => {
            if let Some(resolved) = types.resolved_method_calls.get(&expression.id) {
                if let Some(target) = resolved.static_target {
                    let target = types.substitute(target, substitutions).unwrap_or(target);
                    let mut args = if resolved.qualified {
                        arguments
                            .iter()
                            .skip(1)
                            .map(|argument| {
                                lower_call_argument(argument, types, substitutions, type_parameters)
                            })
                            .collect::<Result<Vec<_>, _>>()?
                    } else {
                        let ExprKind::Field { value, .. } = &callee.kind else {
                            return Err(Diagnostic::codegen("parse requires a String receiver"));
                        };
                        vec![CoreCallArgument {
                            label: None,
                            value: lower_expr(value, types, substitutions, type_parameters)?,
                        }]
                    };
                    let kind = if resolved.name == crate::sema::FROM_STRING_METHOD
                        && target.has_builtin_from_string()
                    {
                        CoreExprKind::Field {
                            value: Box::new(args.remove(0).value),
                            access: FieldAccess::Name("parse".into()),
                        }
                    } else if let Some(symbol) = types
                        .interface
                        .method_symbols
                        .get(&(target, resolved.name.clone()))
                    {
                        CoreExprKind::ExternalSymbol(symbol.clone())
                    } else {
                        CoreExprKind::Name(types.static_method_name(target, &resolved.name))
                    };
                    return Ok(CoreExpr {
                        id: expression.id,
                        ty: types
                            .substitute(types.get(expression), substitutions)
                            .unwrap_or(types.get(expression)),
                        kind: CoreExprKind::Call {
                            callee: Box::new(CoreExpr {
                                id: callee.id,
                                ty: Type::Unit,
                                kind,
                            }),
                            type_arguments: vec![target],
                            arguments: args,
                            effect_operation: None,
                        },
                    });
                }
                let (receiver, arguments) = if resolved.qualified {
                    (&arguments[0].value, &arguments[1..])
                } else {
                    let ExprKind::Field { value, .. } = &callee.kind else {
                        return Err(Diagnostic::codegen("resolved method requires a receiver"));
                    };
                    (value.as_ref(), arguments.as_slice())
                };
                let receiver_value = lower_expr(receiver, types, substitutions, type_parameters)?;
                let receiver_type = receiver_value.ty;
                let mut args = arguments
                    .iter()
                    .map(|argument| {
                        lower_call_argument(argument, types, substitutions, type_parameters)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let kind = if resolved.name == crate::sema::DEBUG_METHOD
                    && matches!(types.get_optional(receiver), Some(Type::Param(_)))
                {
                    args.insert(
                        0,
                        CoreCallArgument {
                            label: None,
                            value: receiver_value,
                        },
                    );
                    CoreExprKind::Name("@debug".into())
                } else if let Some(symbol) = types
                    .interface
                    .method_symbols
                    .get(&(receiver_type, resolved.name.clone()))
                {
                    args.insert(
                        0,
                        CoreCallArgument {
                            label: Some("self".into()),
                            value: receiver_value,
                        },
                    );
                    CoreExprKind::ExternalSymbol(symbol.clone())
                } else {
                    CoreExprKind::Field {
                        value: Box::new(receiver_value),
                        access: FieldAccess::Name(resolved.name.clone()),
                    }
                };
                return Ok(CoreExpr {
                    id: expression.id,
                    ty: types
                        .substitute(types.get(expression), substitutions)
                        .unwrap_or(types.get(expression)),
                    kind: CoreExprKind::Call {
                        callee: Box::new(CoreExpr {
                            id: callee.id,
                            ty: Type::Unit,
                            kind,
                        }),
                        type_arguments: vec![],
                        arguments: args,
                        effect_operation: None,
                    },
                });
            }
            if let Some(Type::Dyn(id)) = types.get_optional(callee) {
                return Ok(CoreExpr {
                    id: expression.id,
                    ty: Type::Dyn(id),
                    kind: CoreExprKind::Call {
                        callee: Box::new(CoreExpr {
                            id: callee.id,
                            ty: Type::Unit,
                            kind: CoreExprKind::Name("@dyn".into()),
                        }),
                        type_arguments: Vec::new(),
                        effect_operation: None,
                        arguments: arguments
                            .iter()
                            .map(|arg| {
                                Ok(CoreCallArgument {
                                    label: arg.label.clone(),
                                    value: lower_expr(
                                        &arg.value,
                                        types,
                                        substitutions,
                                        type_parameters,
                                    )?,
                                })
                            })
                            .collect::<Result<_, Diagnostic>>()?,
                    },
                });
            }
            if let ExprKind::Field {
                value,
                access: FieldAccess::Name(name),
            } = &callee.kind
            {
                let receiver_type = types
                    .get_optional(value)
                    .and_then(|ty| types.substitute(ty, substitutions))
                    .unwrap_or(Type::Unit);
                if let Some(symbol) = types
                    .interface
                    .method_symbols
                    .get(&(receiver_type, name.clone()))
                {
                    let mut args = vec![CoreCallArgument {
                        label: Some("self".into()),
                        value: lower_expr(value, types, substitutions, type_parameters)?,
                    }];
                    args.extend(
                        arguments
                            .iter()
                            .map(|argument| {
                                lower_call_argument(argument, types, substitutions, type_parameters)
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    return Ok(CoreExpr {
                        id: expression.id,
                        ty: types
                            .substitute(types.get(expression), substitutions)
                            .unwrap_or(types.get(expression)),
                        kind: CoreExprKind::Call {
                            callee: Box::new(CoreExpr {
                                id: callee.id,
                                ty: Type::Unit,
                                kind: CoreExprKind::ExternalSymbol(symbol.clone()),
                            }),
                            type_arguments: vec![],
                            arguments: args,
                            effect_operation: None,
                        },
                    });
                }
            }
            if matches!(&callee.kind, ExprKind::Name(name) if name == "print" || name == "println")
                && arguments.len() == 1
            {
                let argument = &arguments[0].value;
                let receiver_type = types
                    .substitute(types.get(argument), substitutions)
                    .unwrap_or(types.get(argument));
                if let Some(symbol) = types
                    .interface
                    .method_symbols
                    .get(&(receiver_type, crate::sema::SHOW_METHOD.into()))
                {
                    let value = CoreExpr {
                        id: argument.id,
                        ty: Type::String,
                        kind: CoreExprKind::Call {
                            callee: Box::new(CoreExpr {
                                id: callee.id,
                                ty: Type::Unit,
                                kind: CoreExprKind::ExternalSymbol(symbol.clone()),
                            }),
                            type_arguments: vec![],
                            arguments: vec![CoreCallArgument {
                                label: Some("self".into()),
                                value: lower_expr(argument, types, substitutions, type_parameters)?,
                            }],
                            effect_operation: None,
                        },
                    };
                    return Ok(CoreExpr {
                        id: expression.id,
                        ty: Type::Unit,
                        kind: CoreExprKind::Call {
                            callee: Box::new(lower_expr(
                                callee,
                                types,
                                substitutions,
                                type_parameters,
                            )?),
                            type_arguments: vec![],
                            arguments: vec![CoreCallArgument { label: None, value }],
                            effect_operation: None,
                        },
                    });
                }
            }
            if let ExprKind::Field {
                value: enum_value,
                access: FieldAccess::Name(variant),
            } = &callee.kind
            {
                if let Some(Type::Enum(enum_id)) = types.get_optional(enum_value).filter(|ty| {
                    matches!(ty, Type::Enum(id) if types.enum_variants(*id).iter().any(|entry| entry.name == *variant))
                }) {
                    let enum_name = types.enum_name(enum_id).to_owned();
                    let lowered_callee = CoreExpr {
                        id: callee.id,
                        ty: Type::Enum(enum_id),
                        kind: CoreExprKind::Field {
                            value: Box::new(CoreExpr {
                                id: enum_value.id,
                                ty: Type::Enum(enum_id),
                                kind: CoreExprKind::Name(enum_name),
                            }),
                            access: FieldAccess::Name(variant.clone()),
                        },
                    };
                    return Ok(CoreExpr {
                        id: expression.id,
                        ty: Type::Enum(enum_id),
                        kind: CoreExprKind::Call {
                            callee: Box::new(lowered_callee),
                            type_arguments: Vec::new(),
                            arguments: arguments
                                .iter()
                                .map(|argument| {
                                    lower_call_argument(
                                        argument,
                                        types,
                                        substitutions,
                                        type_parameters,
                                    )
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                            effect_operation: None,
                        },
                    });
                }
                if let Some(enum_type @ (Type::Option(_) | Type::Result(_))) =
                    types.get_optional(enum_value)
                {
                    if matches!(
                        (enum_type, variant.as_str()),
                        (Type::Option(_), "Some" | "None") | (Type::Result(_), "Ok" | "Err")
                    ) {
                        let name = match (enum_type, variant.as_str()) {
                            (Type::Option(_), "Some") => "Some",
                            (Type::Option(_), "None") => "None",
                            (Type::Result(_), "Ok") => "Ok",
                            (Type::Result(_), "Err") => "Err",
                            _ => {
                                return Err(Diagnostic::codegen(
                                    "built-in enum variant was not resolved",
                                ))
                            }
                        };
                        return Ok(CoreExpr {
                            id: expression.id,
                            ty: enum_type,
                            kind: CoreExprKind::Call {
                                callee: Box::new(CoreExpr {
                                    id: callee.id,
                                    ty: enum_type,
                                    kind: CoreExprKind::Name(name.to_owned()),
                                }),
                                type_arguments: Vec::new(),
                                arguments: arguments
                                    .iter()
                                    .map(|argument| {
                                        lower_call_argument(
                                            argument,
                                            types,
                                            substitutions,
                                            type_parameters,
                                        )
                                    })
                                    .collect::<Result<Vec<_>, _>>()?,
                                effect_operation: None,
                            },
                        });
                    }
                }
            }
            if let ExprKind::Call { .. } = callee.kind {
                let constructor_type = types
                    .substitute(types.get(expression), substitutions)
                    .unwrap_or_else(|| types.get(expression));
                let constructor_types = match constructor_type {
                    Type::List(id) => vec![types.list_type(id)],
                    Type::MutList(id) => vec![types.mut_list_type(id)],
                    Type::Map(id) => {
                        let info = types.map_info(id);
                        vec![info.key, info.value]
                    }
                    Type::MutMap(id) => {
                        let info = types.map_info(id);
                        vec![info.key, info.value]
                    }
                    Type::MutSet(id) => vec![types.map_info(id).key],
                    _ => Vec::new(),
                };
                if !constructor_types.is_empty() {
                    let ExprKind::Call {
                        callee: type_callee,
                        ..
                    } = &callee.kind
                    else {
                        unreachable!();
                    };
                    let CoreExprKind::Name(name) =
                        lower_expr(type_callee, types, substitutions, type_parameters)?.kind
                    else {
                        return Err(Diagnostic::codegen(
                            "intrinsic type constructor must have a named callee",
                        ));
                    };
                    let lowered_arguments = arguments
                        .iter()
                        .map(|argument| {
                            Ok(CoreCallArgument {
                                label: argument.label.clone(),
                                value: lower_expr(
                                    &argument.value,
                                    types,
                                    substitutions,
                                    type_parameters,
                                )?,
                            })
                        })
                        .collect::<Result<Vec<_>, Diagnostic>>()?;
                    let (normalized_callee, type_arguments) = match constructor_type {
                        Type::List(_) => (
                            CoreExprKind::Field {
                                value: Box::new(CoreExpr {
                                    id: type_callee.id,
                                    ty: Type::Unit,
                                    kind: CoreExprKind::Name(name),
                                }),
                                access: FieldAccess::Name("empty".to_owned()),
                            },
                            constructor_types,
                        ),
                        Type::Map(_) | Type::MutMap(_) | Type::MutSet(_) => (
                            CoreExprKind::Field {
                                value: Box::new(CoreExpr {
                                    id: type_callee.id,
                                    ty: Type::Unit,
                                    kind: CoreExprKind::Name(name),
                                }),
                                access: FieldAccess::Name("empty".to_owned()),
                            },
                            constructor_types,
                        ),
                        Type::MutList(_) => (CoreExprKind::Name(name), constructor_types),
                        _ => unreachable!("known intrinsic type constructor"),
                    };
                    return Ok(CoreExpr {
                        id: expression.id,
                        ty: constructor_type,
                        kind: CoreExprKind::Call {
                            callee: Box::new(CoreExpr {
                                id: callee.id,
                                ty: Type::Unit,
                                kind: normalized_callee,
                            }),
                            type_arguments,
                            arguments: if matches!(constructor_type, Type::MutList(_)) {
                                lowered_arguments
                            } else {
                                Vec::new()
                            },
                            effect_operation: None,
                        },
                    });
                }
            }
            if let Type::Struct(id) = types.get(expression) {
                if matches!(callee.kind, ExprKind::Call { .. }) {
                    return Ok(CoreExpr {
                        id: expression.id,
                        ty: Type::Struct(id),
                        kind: CoreExprKind::StructInit {
                            name: types.struct_name(id).to_owned(),
                            fields: arguments
                                .iter()
                                .map(|argument| {
                                    Ok((
                                        argument.label.clone().ok_or_else(|| {
                                            Diagnostic::codegen(
                                                "generated struct fields require labels",
                                            )
                                        })?,
                                        lower_expr(
                                            &argument.value,
                                            types,
                                            substitutions,
                                            type_parameters,
                                        )?,
                                    ))
                                })
                                .collect::<Result<Vec<_>, Diagnostic>>()?,
                        },
                    });
                }
            }
            if let Some(symbol) = types.external_symbol(expression.id) {
                return Ok(CoreExpr {
                    id: expression.id,
                    ty: types.get(expression),
                    kind: CoreExprKind::Call {
                        callee: Box::new(CoreExpr {
                            id: callee.id,
                            ty: Type::Unit,
                            kind: CoreExprKind::ExternalSymbol(symbol.clone()),
                        }),
                        type_arguments: Vec::new(),
                        arguments: arguments
                            .iter()
                            .map(|argument| {
                                lower_call_argument(argument, types, substitutions, type_parameters)
                            })
                            .collect::<Result<Vec<_>, Diagnostic>>()?,
                        effect_operation: types.effect_operation(expression.id),
                    },
                });
            }
            let imported_generic = types
                .generic_call(expression)
                .filter(|instance| types.imported_templates.contains_key(&instance.function));
            let source_method = types
                .generic_call(expression)
                .filter(|instance| instance.function.starts_with("@prelude/"))
                .and_then(|_| match &callee.kind {
                    ExprKind::Field { value, .. } => Some(value.as_ref()),
                    _ => None,
                });
            let nominal_constructor = match (types.get_optional(callee), &callee.kind) {
                (Some(Type::Struct(id)), ExprKind::Field { .. }) => Some(types.struct_name(id)),
                (Some(Type::Class(id)), ExprKind::Field { .. }) => Some(types.class_name(id)),
                _ => None,
            };
            let mut lowered_callee = if let Some(name) = nominal_constructor {
                CoreExpr {
                    id: callee.id,
                    ty: types.get(callee),
                    kind: CoreExprKind::Name(name.to_owned()),
                }
            } else if source_method.is_some() {
                CoreExpr {
                    id: callee.id,
                    ty: Type::Unit,
                    kind: CoreExprKind::Name(String::new()),
                }
            } else if imported_generic.is_some() {
                CoreExpr {
                    id: callee.id,
                    ty: Type::Unit,
                    kind: CoreExprKind::Unit,
                }
            } else {
                lower_expr(callee, types, substitutions, type_parameters)?
            };
            let mut instantiated_arguments = None;
            if let Some(instance) = types.generic_call(expression) {
                let arguments = instance
                    .arguments
                    .iter()
                    .map(|argument| {
                        types.substitute(*argument, substitutions).ok_or_else(|| {
                            Diagnostic::codegen("generic call type argument was not concrete")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                instantiated_arguments = Some(arguments.clone());
                if imported_generic.is_some() {
                    let key = types.instance_name(&instance.function, &arguments);
                    lowered_callee.kind = CoreExprKind::ExternalSymbol(
                        types
                            .interface
                            .generic_symbols
                            .get(&key)
                            .ok_or_else(|| {
                                Diagnostic::codegen("generic import was not instantiated")
                            })?
                            .clone(),
                    );
                } else if let CoreExprKind::Name(_) = lowered_callee.kind {
                    lowered_callee.kind =
                        CoreExprKind::Name(types.instance_name(&instance.function, &arguments));
                }
            }
            let type_value_arguments = types.type_value_call(expression.id).map(|arguments| {
                arguments
                    .iter()
                    .map(|argument| {
                        types.substitute(*argument, substitutions).ok_or_else(|| {
                            Diagnostic::codegen("compile-time type argument was not concrete")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            });
            let type_value_arguments = type_value_arguments.transpose()?;
            CoreExprKind::Call {
                callee: Box::new(lowered_callee),
                type_arguments: if let Some(ref arguments) = type_value_arguments {
                    arguments.clone()
                } else {
                    instantiated_arguments.unwrap_or_default()
                },
                arguments: source_method
                    .map(|receiver| {
                        lower_expr(receiver, types, substitutions, type_parameters)
                            .map(|value| CoreCallArgument { label: None, value })
                    })
                    .into_iter()
                    .chain(
                        arguments
                            .iter()
                            .skip(type_value_arguments.as_ref().map_or_else(
                                || {
                                    types
                                        .generic_call(expression)
                                        .map_or(0, |instance| instance.compile_time_arguments)
                                },
                                Vec::len,
                            ))
                            .map(|argument| {
                                lower_call_argument(argument, types, substitutions, type_parameters)
                            }),
                    )
                    .collect::<Result<Vec<_>, _>>()?,
                effect_operation: types.effect_operation(expression.id),
            }
        }
        ExprKind::Closure {
            move_capture,
            parameters,
            return_type: _,
            body,
        } => {
            let Type::Function(signature_id) = types
                .substitute(types.get(expression), substitutions)
                .ok_or_else(|| Diagnostic::codegen("closure type was not concrete"))?
            else {
                return Err(Diagnostic::codegen("closure does not have a function type"));
            };
            let signature = types.function_type(signature_id);
            let function_name = format!("__closure_{:?}", expression.id);
            let function_name = if substitutions.is_empty() {
                function_name
            } else {
                types.instance_name(&function_name, substitutions)
            };
            CoreExprKind::Closure {
                move_capture: *move_capture,
                function_name,
                captures: types
                    .closure_captures(expression.id)
                    .iter()
                    .map(|(name, ty)| {
                        types
                            .substitute(*ty, substitutions)
                            .map(|ty| (name.clone(), ty))
                            .ok_or_else(|| {
                                Diagnostic::codegen("closure capture type was not concrete")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                capture_bindings: types.closure_capture_bindings(expression.id).to_vec(),
                parameters: parameters
                    .iter()
                    .zip(&signature.parameters)
                    .map(|(parameter, ty)| Ok((parameter.name.clone(), *ty)))
                    .collect::<Result<Vec<_>, Diagnostic>>()?,
                return_type: signature.return_type,
                effects: signature.effects.clone(),
                body: Box::new(lower_expr(body, types, substitutions, type_parameters)?),
            }
        }
        ExprKind::Tuple(elements) => CoreExprKind::Tuple(
            elements
                .iter()
                .map(|element| lower_expr(element, types, substitutions, type_parameters))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ExprKind::CollectionLiteral(literal) => CoreExprKind::CollectionLiteral(match literal {
            CollectionLiteral::List(elements) => CoreCollectionLiteral::List(
                elements
                    .iter()
                    .map(|element| lower_expr(element, types, substitutions, type_parameters))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            CollectionLiteral::MutList(elements) => CoreCollectionLiteral::MutList(
                elements
                    .iter()
                    .map(|element| lower_expr(element, types, substitutions, type_parameters))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            CollectionLiteral::Set(elements) => CoreCollectionLiteral::Set(
                elements
                    .iter()
                    .map(|element| lower_expr(element, types, substitutions, type_parameters))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            CollectionLiteral::MutSet(elements) => CoreCollectionLiteral::MutSet(
                elements
                    .iter()
                    .map(|element| lower_expr(element, types, substitutions, type_parameters))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            CollectionLiteral::Map(entries) => CoreCollectionLiteral::Map(
                entries
                    .iter()
                    .map(|entry| {
                        Ok(CoreMapLiteralEntry {
                            key: lower_expr(&entry.key, types, substitutions, type_parameters)?,
                            value: lower_expr(&entry.value, types, substitutions, type_parameters)?,
                        })
                    })
                    .collect::<Result<Vec<_>, Diagnostic>>()?,
            ),
            CollectionLiteral::MutMap(entries) => CoreCollectionLiteral::MutMap(
                entries
                    .iter()
                    .map(|entry| {
                        Ok(CoreMapLiteralEntry {
                            key: lower_expr(&entry.key, types, substitutions, type_parameters)?,
                            value: lower_expr(&entry.value, types, substitutions, type_parameters)?,
                        })
                    })
                    .collect::<Result<Vec<_>, Diagnostic>>()?,
            ),
        }),
        ExprKind::StructInit { name, fields } => CoreExprKind::StructInit {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(name, value)| {
                    Ok((
                        name.clone(),
                        lower_expr(value, types, substitutions, type_parameters)?,
                    ))
                })
                .collect::<Result<Vec<_>, Diagnostic>>()?,
        },
        ExprKind::Field { value, access } => {
            if let (
                Some(enum_type @ (Type::Option(_) | Type::Result(_))),
                FieldAccess::Name(name),
            ) = (types.get_optional(value), access)
            {
                if matches!(
                    (enum_type, name.as_str()),
                    (Type::Option(_), "Some" | "None") | (Type::Result(_), "Ok" | "Err")
                ) {
                    let name = match (enum_type, name.as_str()) {
                        (Type::Option(_), "None") => "None",
                        (Type::Option(_), "Some") | (Type::Result(_), "Ok" | "Err") => {
                            return Err(Diagnostic::codegen(
                                "built-in enum variants with payloads require a call",
                            ));
                        }
                        _ => unreachable!(),
                    };
                    CoreExprKind::Name(name.to_owned())
                } else {
                    CoreExprKind::Field {
                        value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
                        access: access.clone(),
                    }
                }
            } else if let Some(Type::Enum(enum_id)) = types.get_optional(value).filter(|ty| {
                matches!((ty, access), (Type::Enum(id), FieldAccess::Name(name))
                    if types.enum_variants(*id).iter().any(|variant| variant.name == *name))
            }) {
                CoreExprKind::Field {
                    value: Box::new(CoreExpr {
                        id: value.id,
                        ty: Type::Enum(enum_id),
                        kind: CoreExprKind::Name(types.enum_name(enum_id).to_owned()),
                    }),
                    access: access.clone(),
                }
            } else {
                CoreExprKind::Field {
                    value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
                    access: access.clone(),
                }
            }
        }
        ExprKind::Assign { target, value } => CoreExprKind::Assign {
            target: Box::new(lower_expr(target, types, substitutions, type_parameters)?),
            value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
        },
        ExprKind::CompoundAssign {
            target,
            operator,
            value,
        } => CoreExprKind::CompoundAssign {
            target: Box::new(lower_expr(target, types, substitutions, type_parameters)?),
            operator: *operator,
            value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
        },
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => CoreExprKind::If {
            condition: Box::new(lower_expr(
                condition,
                types,
                substitutions,
                type_parameters,
            )?),
            then_branch: Box::new(lower_expr(
                then_branch,
                types,
                substitutions,
                type_parameters,
            )?),
            else_branch: Box::new(lower_expr(
                else_branch,
                types,
                substitutions,
                type_parameters,
            )?),
        },
        ExprKind::Match { value, arms } => CoreExprKind::Match {
            value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
            arms: arms
                .iter()
                .map(|arm| lower_match_arm(arm, types, substitutions, type_parameters))
                .collect::<Result<Vec<_>, _>>()?,
        },
        ExprKind::For {
            index,
            item,
            iterable,
            body,
            limit,
        } => CoreExprKind::For {
            index: index.clone(),
            item: item.clone(),
            iterable: Box::new(lower_expr(iterable, types, substitutions, type_parameters)?),
            body: Box::new(lower_expr(body, types, substitutions, type_parameters)?),
            limit: limit
                .as_deref()
                .map(|expr| lower_expr(expr, types, substitutions, type_parameters).map(Box::new))
                .transpose()?,
        },
        ExprKind::While { condition, body } => CoreExprKind::While {
            condition: Box::new(lower_expr(
                condition,
                types,
                substitutions,
                type_parameters,
            )?),
            body: Box::new(lower_expr(body, types, substitutions, type_parameters)?),
        },
        ExprKind::Loop { body } => CoreExprKind::Loop {
            body: Box::new(lower_expr(body, types, substitutions, type_parameters)?),
        },
        ExprKind::Break { value } => CoreExprKind::Break {
            value: value
                .as_deref()
                .map(|value| lower_expr(value, types, substitutions, type_parameters).map(Box::new))
                .transpose()?,
        },
        ExprKind::Abort { value } => CoreExprKind::Abort {
            value: Box::new(lower_expr(value, types, substitutions, type_parameters)?),
        },
        ExprKind::Continue => CoreExprKind::Continue,
        ExprKind::When {
            cowns,
            bindings,
            until,
            body,
        } => CoreExprKind::When {
            cowns: cowns
                .iter()
                .map(|cown| lower_expr(cown, types, substitutions, type_parameters))
                .collect::<Result<Vec<_>, _>>()?,
            bindings: bindings
                .as_ref()
                .map(|patterns| patterns.iter().map(lower_pattern).collect()),
            until: until
                .as_deref()
                .map(|guard| lower_expr(guard, types, substitutions, type_parameters).map(Box::new))
                .transpose()?,
            body: Box::new(lower_expr(body, types, substitutions, type_parameters)?),
        },
        ExprKind::Do { body, handlers } => CoreExprKind::Do {
            body: Box::new(lower_expr(body, types, substitutions, type_parameters)?),
            handlers: handlers
                .iter()
                .map(|handler| {
                    let operation = types
                        .handler_operation(handler.id)
                        .ok_or_else(|| Diagnostic::codegen("handler operation was not resolved"))?;
                    let resumes = matches!(
                        types.effects().operation_mode(operation),
                        Some(crate::sema::EffectMode::Resumable)
                    );
                    let aborts = matches!(
                        types.effects().operation_mode(operation),
                        Some(crate::sema::EffectMode::Aborts)
                    ) || terminal_abort_payload(&handler.value).is_some();
                    let value = if matches!(
                        types.effects().operation_mode(operation),
                        Some(crate::sema::EffectMode::Aborts)
                    ) {
                        lower_abort_handler_value(
                            &handler.value,
                            types,
                            substitutions,
                            type_parameters,
                        )?
                    } else {
                        lower_handler_action(
                            &handler.value,
                            resumes,
                            types,
                            substitutions,
                            type_parameters,
                        )?
                    };
                    Ok(CoreHandlerArm {
                        operation,
                        parameters: handler.parameters.iter().map(lower_pattern).collect(),
                        resumes,
                        aborts,
                        value,
                        span: handler.span,
                    })
                })
                .collect::<Result<Vec<_>, Diagnostic>>()?,
        },
        ExprKind::Parallel(arms) => CoreExprKind::Parallel(
            arms.iter()
                .map(|arm| lower_expr(arm, types, substitutions, type_parameters))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ExprKind::Race(arms) => CoreExprKind::Race(
            arms.iter()
                .map(|arm| lower_expr(arm, types, substitutions, type_parameters))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ExprKind::Region(body) => CoreExprKind::Region(Box::new(lower_expr(
            body,
            types,
            substitutions,
            type_parameters,
        )?)),
        ExprKind::Branch(body) => CoreExprKind::Branch(Box::new(lower_expr(
            body,
            types,
            substitutions,
            type_parameters,
        )?)),
        ExprKind::Block(expressions) => CoreExprKind::Block(
            expressions
                .iter()
                .map(|expression| lower_expr(expression, types, substitutions, type_parameters))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    Ok(CoreExpr {
        id: expression.id,
        // A call target is syntax rather than a first-class value in Joky, so
        // sema does not assign it a standalone type. Unit is the neutral type
        // for that placeholder; the Call node itself has the checked result.
        ty: types
            .substitute(
                types.get_optional(expression).unwrap_or(Type::Unit),
                substitutions,
            )
            .ok_or_else(|| Diagnostic::codegen("generic expression type was not concrete"))?,
        kind,
    })
}

fn lower_call_argument(
    argument: &CallArgument,
    types: &CheckedTypes,
    substitutions: &[Type],
    type_parameters: &[String],
) -> Result<CoreCallArgument, Diagnostic> {
    Ok(CoreCallArgument {
        label: argument.label.clone(),
        value: lower_expr(&argument.value, types, substitutions, type_parameters)?,
    })
}

fn lower_handler_action(
    expression: &Expr,
    controlled: bool,
    types: &CheckedTypes,
    substitutions: &[Type],
    type_parameters: &[String],
) -> Result<CoreExpr, Diagnostic> {
    if controlled {
        if let ExprKind::Call {
            callee, arguments, ..
        } = &expression.kind
        {
            if matches!(&callee.kind, ExprKind::Name(name) if name == "resume")
                && arguments.len() == 1
            {
                return lower_expr(&arguments[0].value, types, substitutions, type_parameters);
            }
        }
    }
    lower_expr(expression, types, substitutions, type_parameters)
}

fn lower_abort_handler_value(
    expression: &Expr,
    types: &CheckedTypes,
    substitutions: &[Type],
    type_parameters: &[String],
) -> Result<CoreExpr, Diagnostic> {
    if let ExprKind::Abort { value } = &expression.kind {
        return lower_expr(value, types, substitutions, type_parameters);
    }
    if let ExprKind::Block(expressions) = &expression.kind {
        if let Some(Expr {
            kind: ExprKind::Abort { value },
            ..
        }) = expressions.last()
        {
            let mut lowered = expressions[..expressions.len() - 1]
                .iter()
                .map(|expression| lower_expr(expression, types, substitutions, type_parameters))
                .collect::<Result<Vec<_>, _>>()?;
            lowered.push(lower_expr(value, types, substitutions, type_parameters)?);
            let ty = lowered
                .last()
                .map(|expression| expression.ty)
                .unwrap_or(Type::Unit);
            return Ok(CoreExpr {
                id: expression.id,
                ty,
                kind: CoreExprKind::Block(lowered),
            });
        }
    }
    lower_expr(expression, types, substitutions, type_parameters)
}

fn terminal_abort_payload(expression: &Expr) -> Option<&Expr> {
    match &expression.kind {
        ExprKind::Abort { value } => Some(value),
        ExprKind::Block(expressions) => expressions.last().and_then(terminal_abort_payload),
        _ => None,
    }
}

fn lower_match_arm(
    arm: &MatchArm,
    types: &CheckedTypes,
    substitutions: &[Type],
    type_parameters: &[String],
) -> Result<CoreMatchArm, Diagnostic> {
    Ok(CoreMatchArm {
        pattern: lower_pattern(&arm.pattern),
        value: lower_expr(&arm.value, types, substitutions, type_parameters)?,
        span: arm.span,
    })
}

fn lower_pattern(pattern: &Pattern) -> CorePattern {
    match pattern {
        Pattern::Wildcard { span } => CorePattern::Wildcard { span: *span },
        Pattern::Binding { name, span } => CorePattern::Binding {
            name: name.clone(),
            span: *span,
        },
        Pattern::EnumVariant {
            enum_name,
            variant,
            fields,
            span,
        } => CorePattern::EnumVariant {
            enum_name: enum_name.clone(),
            variant: variant.clone(),
            fields: fields
                .iter()
                .map(|field| CorePatternField {
                    label: field.label.clone(),
                    pattern: lower_pattern(&field.pattern),
                    span: field.span,
                })
                .collect(),
            span: *span,
        },
        Pattern::Tuple { elements, span } => CorePattern::Tuple {
            elements: elements.iter().map(lower_pattern).collect(),
            span: *span,
        },
    }
}

/// Both source expressions and imported constants use the same trait identity.
pub(crate) fn lower_comparison(
    id: NodeId,
    op: BinaryOp,
    receiver: CoreExpr,
    other: CoreExpr,
    types: &CheckedTypes,
) -> CoreExprKind {
    let equality = matches!(op, BinaryOp::Equal | BinaryOp::NotEqual);
    let method = if equality {
        crate::sema::PARTIAL_EQ_METHOD
    } else {
        crate::sema::PARTIAL_ORD_METHOD
    };
    let mut arguments = vec![CoreCallArgument {
        label: Some("other".into()),
        value: other,
    }];
    let callee = if let Some(symbol) = types
        .interface
        .method_symbols
        .get(&(receiver.ty, method.into()))
    {
        arguments.insert(
            0,
            CoreCallArgument {
                label: Some("self".into()),
                value: receiver,
            },
        );
        CoreExprKind::ExternalSymbol(symbol.clone())
    } else {
        CoreExprKind::Field {
            value: Box::new(receiver),
            access: FieldAccess::Name(method.into()),
        }
    };
    let call = CoreExprKind::Call {
        callee: Box::new(CoreExpr {
            id,
            ty: Type::Unit,
            kind: callee,
        }),
        type_arguments: vec![],
        arguments,
        effect_operation: None,
    };
    if !equality {
        CoreExprKind::Call {
            callee: Box::new(CoreExpr {
                id,
                ty: Type::Unit,
                kind: CoreExprKind::Name(format!("@ordering/{op:?}")),
            }),
            type_arguments: vec![],
            arguments: vec![CoreCallArgument {
                label: None,
                value: CoreExpr {
                    id,
                    ty: types.partial_ordering_type(),
                    kind: call,
                },
            }],
            effect_operation: None,
        }
    } else if op == BinaryOp::NotEqual {
        CoreExprKind::Unary {
            op: UnaryOp::Not,
            expression: Box::new(CoreExpr {
                id,
                ty: Type::Bool,
                kind: call,
            }),
        }
    } else {
        call
    }
}
