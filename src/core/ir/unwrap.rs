//! Postfix extraction shares consuming patterns, cleanup, and function exits.
use super::*;
use crate::core::types::{EnumType, GenericArgument};
use patterns::{CheckedPattern, Kind};

// Declaration identity survives aliases and disabled preludes. User enums with
// the same spelling or variant shape do not acquire compiler semantics.
pub(super) fn success_variant(enumeration: &EnumType, types: &TypeRegistry) -> Option<usize> {
    let name = types.standard_success_variant(enumeration.id)?;
    enumeration.variants.iter().position(|v| v.name == name)
}

fn variant_pattern(enumeration: &Arc<EnumType>, tag: usize, span: Span) -> CheckedPattern {
    CheckedPattern::nested(
        Type::Enum(enumeration.clone()),
        Kind::Variant {
            tag,
            payload: enumeration.variants[tag].payload.as_ref().map(|ty| {
                Box::new(CheckedPattern::nested(
                    ty.clone(),
                    Kind::Any(Some("$payload".into())),
                    span,
                ))
            }),
        },
        span,
    )
}

impl FunctionPlan {
    // Supply the extracted value's context to constructors, and for `?` also
    // supply the enclosing Result's error type. Explicit arguments still win.
    fn unwrap_operand_hint(
        &self,
        operand: &Expr,
        propagate: bool,
        expected: Option<&Type>,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Option<Type>, Error> {
        let Some(constructor) = self.enum_constructor(operand, vars)? else {
            return Ok(None);
        };
        let enumeration = &constructor.enumeration;
        if !constructor.type_args.is_empty() || enumeration.parameters.is_empty() {
            return Ok(None);
        }
        let Some(success) = success_variant(enumeration, types) else {
            return Ok(None);
        };
        let returned = match &self.return_type {
            Type::Enum(target) if propagate && target.id == enumeration.id => Some(target),
            _ => None,
        };
        let payload = constructor
            .args
            .and_then(|args| args.first())
            .and_then(|arg| self.hint(arg, vars, functions, types, true));
        let is_success = constructor.name == enumeration.variants[success].name;
        let value = expected
            .cloned()
            .or_else(|| is_success.then(|| payload.clone()).flatten())
            .or_else(|| returned.and_then(|r| r.arguments.first()?.as_type().cloned()));
        let Some(value) = value else { return Ok(None) };
        let mut arguments = vec![GenericArgument::Type(value)];
        if enumeration.parameters.len() == 2 {
            let error = returned
                .and_then(|r| r.arguments.get(1)?.as_type().cloned())
                .or_else(|| {
                    matches!(constructor.name.as_str(), "Err" | "Rejected")
                        .then_some(payload)
                        .flatten()
                });
            let Some(error) = error else { return Ok(None) };
            arguments.push(GenericArgument::Type(error));
        }
        types.instantiate_enum(enumeration, arguments).map(Some)
    }

    pub(super) fn unwrap_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let Expr::Unary { op, operand, span } = expr else {
            unreachable!()
        };
        let propagate = *op == UnaryOp::Try;
        // Evaluate exactly once, and move owned temporaries out of the operand
        // scope just as match does before inspecting a discriminant.
        let hint =
            self.unwrap_operand_hint(operand, propagate, expected, vars, functions, types)?;
        self.local_scopes.push(Vec::new());
        let mut flow = self.expr_expected(
            operand,
            &mut vars.clone(),
            functions,
            suspends,
            types,
            hint.as_ref(),
        )?;
        if let Flow::Value(Some(value)) = &flow {
            if value.ty.is_owner() || value.ty.is_place() {
                flow = Flow::Value(self.snapshot(&Some(value.clone()))?);
                self.escape_owner(&flow);
            }
        }
        self.end_local_scope();
        let Flow::Value(value) = flow else {
            return Ok(Flow::Diverged);
        };
        let invalid = || {
            type_error(
                span,
                ErrorCode::TypeMismatch,
                "Postfix `!` and `?` require a standard Option, Result or Outcome value",
            )
        };
        let value = value.ok_or_else(invalid)?;
        let Type::Enum(enumeration) = &value.ty else {
            return Err(invalid());
        };
        let success = success_variant(enumeration, types).ok_or_else(invalid)?;
        let target = if propagate {
            let valid = match &self.return_type {
                Type::Enum(target) => {
                    target.id == enumeration.id
                        && enumeration
                            .variants
                            .iter()
                            .enumerate()
                            .all(|(tag, variant)| {
                                tag == success || target.variants[tag].payload == variant.payload
                            })
                }
                _ => false,
            };
            if !valid {
                return Err(type_error(span, ErrorCode::TypeMismatch,
                    "Postfix `?` requires the enclosing function to return the same Option, Result or Outcome kind with the same error type"));
            }
            let Type::Enum(target) = &self.return_type else {
                unreachable!()
            };
            Some(target.clone())
        } else {
            None
        };

        for failure in (0..enumeration.variants.len()).filter(|&tag| tag != success) {
            let next = self.label_count;
            self.label_count += 1;
            let failed = variant_pattern(enumeration, failure, *span);
            self.match_pattern_tests(&failed, value.index, next)?;
            self.local_scopes.push(Vec::new());
            if value.ty.is_owner() || value.ty.is_place() {
                self.local_scopes.last_mut().unwrap().push(value.clone());
            }
            let mut local = vars.clone();
            if let Some(target) = target.clone() {
                self.bind_match_pattern(&failed, Some(value.clone()), None, &mut local)?;
                let payload = [Expr::Ident {
                    name: "$payload".into(),
                    span: *span,
                }];
                let returned = self.construct_enum(
                    enums::Constructor {
                        name: target.variants[failure].name.clone(),
                        args: target.variants[failure]
                            .payload
                            .as_ref()
                            .map(|_| payload.as_slice()),
                        enumeration: target,
                        type_args: &[],
                        span: *span,
                    },
                    &mut local,
                    functions,
                    suspends,
                    types,
                )?;
                let Flow::Value(returned) = returned else {
                    unreachable!()
                };
                self.return_value(returned, *span)?;
            } else {
                let message = format!("Postfix `!` on {}", enumeration.variants[failure].name);
                let literal = Expr::Literal {
                    literal: Literal::String(message),
                    span: *span,
                };
                let Flow::Value(message) =
                    self.expr(&literal, &mut local, functions, suspends, types)?
                else {
                    unreachable!()
                };
                local.insert(
                    "$message".into(),
                    Binding {
                        slot: message,
                        mutable: false,
                        place: None,
                    },
                );
                let Flow::Value(Some(message)) = self.borrow_local(
                    &Expr::Ident {
                        name: "$message".into(),
                        span: *span,
                    },
                    false,
                    &local,
                    suspends,
                    *span,
                )?
                else {
                    unreachable!()
                };
                self.emit(Op::Panic { message });
            }
            self.end_local_scope();
            self.emit(Op::Label(next));
        }
        let mut local = vars.clone();
        self.local_scopes.push(Vec::new());
        if value.ty.is_owner() || value.ty.is_place() {
            self.local_scopes.last_mut().unwrap().push(value.clone());
        }
        self.bind_match_pattern(
            &variant_pattern(enumeration, success, *span),
            Some(value),
            None,
            &mut local,
        )?;
        let payload = self.expr(
            &Expr::Ident {
                name: "$payload".into(),
                span: *span,
            },
            &mut local,
            functions,
            suspends,
            types,
        )?;
        let Flow::Value(payload) = payload else {
            unreachable!()
        };
        let flow = Flow::Value(self.snapshot(&payload)?);
        self.escape_owner(&flow);
        self.end_local_scope();
        Ok(flow)
    }
}
