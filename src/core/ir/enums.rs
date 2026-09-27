//! Enum construction and exhaustive matching, lowered to typed scalar storage.
use super::*;
use crate::parser::ast::{EnumType, MatchArm, Pattern};
use std::collections::HashSet;

pub(super) struct Constructor<'a> {
    enumeration: Arc<EnumType>,
    name: &'a str,
    args: Option<&'a [Expr]>,
}

impl FunctionPlan {
    // Local values shadow constructor roots, as they shadow dotted calls.
    pub(super) fn enum_constructor<'a>(
        &self,
        expr: &'a Expr,
        vars: &HashMap<String, Binding>,
    ) -> Option<Constructor<'a>> {
        let (root, variant, payload) = match expr {
            Expr::Call { callee, args, .. } => {
                let (root, variant) = callee.split_once('.')?;
                (root, variant, Some(args.as_slice()))
            }
            Expr::Field { base, name, .. } => {
                let Expr::Ident { name: root, .. } = base.as_ref() else {
                    return None;
                };
                (root.as_str(), name.as_str(), None)
            }
            _ => return None,
        };
        if vars.contains_key(root) {
            return None;
        }
        let Type::Enum(enumeration) = self.nominals.get(root)? else {
            return None;
        };
        Some(Constructor {
            enumeration: enumeration.clone(),
            name: variant,
            args: payload,
        })
    }

    pub(super) fn construct_enum(
        &mut self,
        constructor: Constructor<'_>,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Constructor {
            enumeration,
            name,
            args,
        } = constructor;
        let (tag, variant) = enumeration
            .variants
            .iter()
            .enumerate()
            .find(|(_, v)| v.name == name)
            .ok_or_else(|| {
                type_error(
                    &self.current_span,
                    ErrorCode::UnknownName,
                    format!("Enum `{}` has no variant `{name}`", enumeration.name),
                )
            })?;
        let valid = match (&variant.payload, args) {
            (None, None) => true,
            (Some(_), Some(args)) => args.len() == 1,
            _ => false,
        };
        if !valid {
            let expected = if variant.payload.is_some() {
                "exactly one payload argument"
            } else {
                "no parentheses or payload"
            };
            return Err(type_error(
                &self.current_span,
                ErrorCode::InvalidCall,
                format!("Variant `{}.{name}` requires {expected}", enumeration.name),
            ));
        }
        let result = self.slot(Type::Enum(enumeration.clone()))?;
        for scalar in result.scalars() {
            let value = match scalar.ty {
                Type::Int32 => Constant::Int32(if scalar.index == result.index {
                    tag as i32
                } else {
                    0
                }),
                Type::Bool => Constant::Bool(false),
                Type::Int64 => Constant::Int64(0),
                _ => unreachable!("enum layout contains only copyable scalar lanes"),
            };
            self.emit(Op::Literal(scalar, value));
        }
        if let (Some(ty), Some(args)) = (&variant.payload, args) {
            let Flow::Value(value) =
                self.expr_expected(&args[0], vars, functions, suspends, types, Some(ty))?
            else {
                return Ok(Flow::Diverged);
            };
            if slot_type(&value) != *ty {
                return Err(type_error(
                    &args[0].span(),
                    ErrorCode::TypeMismatch,
                    format!(
                        "Variant `{}.{name}` expects {ty}, got {}",
                        enumeration.name,
                        slot_type(&value)
                    ),
                ));
            }
            if let Some(value) = value {
                self.emit(Op::Copy(
                    Slot {
                        index: result.index + variant.offset,
                        ty: ty.clone(),
                    },
                    value,
                ));
            }
        }
        Ok(Flow::Value(Some(result)))
    }

    pub(super) fn match_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let Expr::Match { value, arms, span } = expr else {
            unreachable!()
        };
        let Flow::Value(value) = self.expr(value, &mut vars.clone(), functions, suspends, types)?
        else {
            return match arms.first() {
                Some(arm) => Err(unreachable_error(&arm.body)),
                None => Ok(Flow::Diverged),
            };
        };
        let Some(value) = value.filter(|slot| matches!(slot.ty, Type::Enum(_))) else {
            return Err(type_error(
                span,
                ErrorCode::TypeMismatch,
                "match requires an enum value",
            ));
        };
        let Type::Enum(enumeration) = &value.ty else {
            unreachable!()
        };
        let patterns = validate_patterns(enumeration, arms, span)?;
        let hint = arms.iter().zip(&patterns).find_map(|(arm, variant)| {
            let mut local = vars.clone();
            if let (
                Some(index),
                Pattern::Variant {
                    binding: Some(name),
                    ..
                },
            ) = (variant, &arm.pattern)
            {
                if let Some(ty) = &enumeration.variants[*index].payload {
                    local.insert(
                        name.clone(),
                        Binding {
                            slot: Some(Slot {
                                index: 0,
                                ty: ty.clone(),
                            }),
                            mutable: false,
                        },
                    );
                }
            }
            self.type_hint(&arm.body, &local, functions, types)
        });
        let expected = hint.as_ref().or(expected);
        let exit = self.label_count;
        self.label_count += 1;
        let tag = Slot {
            index: value.index,
            ty: Type::Int32,
        };
        // The outer Option tracks normal completion, including a Unit value.
        let mut result: Option<Option<Slot>> = None;
        for (arm_index, (arm, variant)) in arms.iter().zip(patterns).enumerate() {
            self.current_span = arm.pattern.span();
            let mut local = vars.clone();
            let next = if let Some(index) = variant {
                let variant = &enumeration.variants[index];
                if let Pattern::Variant {
                    binding: Some(name),
                    ..
                } = &arm.pattern
                {
                    if name != "_" {
                        let ty = variant.payload.as_ref().unwrap();
                        let slot = (*ty != Type::Unit).then(|| Slot {
                            index: value.index + variant.offset,
                            ty: ty.clone(),
                        });
                        local.insert(
                            name.clone(),
                            Binding {
                                slot,
                                mutable: false,
                            },
                        );
                    }
                }
                if arm_index + 1 < arms.len() {
                    let expected = self.slot(Type::Int32)?;
                    self.emit(Op::Literal(expected.clone(), Constant::Int32(index as i32)));
                    let condition = self.slot(Type::Bool)?;
                    self.emit(Op::Binary(
                        condition.clone(),
                        BinOp::Eq,
                        tag.clone(),
                        expected,
                    ));
                    let body = self.label_count;
                    let next = body + 1;
                    self.label_count += 2;
                    self.emit(Op::Branch {
                        condition,
                        then_label: body,
                        else_label: next,
                    });
                    self.emit(Op::Label(body));
                    Some(next)
                } else {
                    None
                }
            } else {
                None
            };
            if let Flow::Value(arm_value) =
                self.expr_expected(&arm.body, &mut local, functions, suspends, types, expected)?
            {
                if let Some(result) = &result {
                    if slot_type(result) != slot_type(&arm_value) {
                        return Err(type_error(
                            &arm.body.span(),
                            ErrorCode::TypeMismatch,
                            format!(
                                "match arms must have the same type: expected {}, got {}",
                                slot_type(result),
                                slot_type(&arm_value)
                            ),
                        ));
                    }
                    if let (Some(dst), Some(src)) = (result, arm_value) {
                        self.emit(Op::Copy(dst.clone(), src));
                    }
                } else {
                    result = Some(self.snapshot(&arm_value)?);
                }
                self.emit(Op::Jump(exit));
            }
            if let Some(next) = next {
                self.emit(Op::Label(next));
            }
        }
        self.current_span = *span;
        match result {
            Some(result) => {
                self.emit(Op::Label(exit));
                Ok(Flow::Value(result))
            }
            None => Ok(Flow::Diverged),
        }
    }
}

fn validate_patterns(
    enumeration: &EnumType,
    arms: &[MatchArm],
    span: &Span,
) -> Result<Vec<Option<usize>>, Error> {
    let mut seen = HashSet::new();
    let mut wildcard = false;
    let mut patterns = Vec::with_capacity(arms.len());
    for arm in arms {
        let span = arm.pattern.span();
        if wildcard || seen.len() == enumeration.variants.len() {
            return Err(type_error(
                &span,
                ErrorCode::UnreachableCode,
                "Unreachable match arm after complete coverage",
            ));
        }
        match &arm.pattern {
            Pattern::Wildcard { .. } => {
                wildcard = true;
                patterns.push(None);
            }
            Pattern::Variant { name, binding, .. } => {
                let variant_name = if let Some((qualifier, variant)) = name.split_once('.') {
                    if qualifier != enumeration.name {
                        return Err(type_error(
                            &span,
                            ErrorCode::InvalidPattern,
                            format!(
                                "Pattern `{name}` does not belong to enum `{}`",
                                enumeration.name
                            ),
                        ));
                    }
                    variant
                } else {
                    name.as_str()
                };
                let (index, variant) = enumeration
                    .variants
                    .iter()
                    .enumerate()
                    .find(|(_, v)| v.name == variant_name)
                    .ok_or_else(|| {
                        type_error(
                            &span,
                            ErrorCode::InvalidPattern,
                            format!(
                                "Enum `{}` has no variant `{variant_name}`",
                                enumeration.name
                            ),
                        )
                    })?;
                if !seen.insert(index) {
                    return Err(type_error(
                        &span,
                        ErrorCode::UnreachableCode,
                        format!("Duplicate match arm for `{name}`"),
                    ));
                }
                if variant.payload.is_some() != binding.is_some() {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        format!("Pattern `{name}` must match its variant's payload arity"),
                    ));
                }
                patterns.push(Some(index));
            }
        }
    }
    if !wildcard && seen.len() != enumeration.variants.len() {
        let missing: Vec<_> = enumeration
            .variants
            .iter()
            .enumerate()
            .filter(|(index, _)| !seen.contains(index))
            .map(|(_, v)| v.name.as_str())
            .collect();
        return Err(type_error(
            span,
            ErrorCode::NonExhaustiveMatch,
            format!(
                "Non-exhaustive match; missing variants: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(patterns)
}
