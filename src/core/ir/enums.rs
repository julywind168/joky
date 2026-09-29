//! Enum construction and exhaustive matching, lowered to typed scalar storage.
use super::*;
use crate::parser::ast::{EnumType, MatchArm, Pattern};
use std::collections::HashSet;

use super::generics::ConstructorContext;

pub(super) struct Constructor<'a> {
    pub enumeration: Arc<EnumType>,
    pub name: String,
    pub args: Option<&'a [Expr]>,
    pub type_args: &'a [Type],
    span: Span,
}

impl FunctionPlan {
    // Local values shadow constructor roots, as they shadow dotted calls.
    pub(super) fn enum_constructor<'a>(
        &self,
        expr: &'a Expr,
        vars: &HashMap<String, Binding>,
    ) -> Option<Constructor<'a>> {
        let (path, payload, type_args): (String, _, &[Type]) = match expr {
            Expr::Call {
                callee,
                args,
                type_args,
                ..
            } => (callee.clone(), Some(args.as_slice()), type_args),
            Expr::GenericName(name) => (name.name.clone(), None, &name.type_args),
            Expr::Ident { name, .. } => (name.clone(), None, &[]),
            Expr::Field {
                base,
                name,
                type_args,
                ..
            } => {
                if let Some(arguments) = super::expression_type_arguments(base) {
                    if !type_args.is_empty() {
                        return None;
                    }
                    (
                        format!("{}.{name}", super::expression_path(base)?),
                        None,
                        arguments,
                    )
                } else {
                    (super::expression_path(expr)?, None, type_args)
                }
            }
            Expr::MethodCall(call) => {
                let arguments = super::expression_type_arguments(&call.receiver)?;
                if !call.type_args.is_empty() {
                    return None;
                }
                (
                    format!("{}.{}", super::expression_path(&call.receiver)?, call.name),
                    Some(call.args.as_slice()),
                    arguments,
                )
            }
            _ => return None,
        };
        if vars.contains_key(path.split('.').next()?) {
            return None;
        }
        let (enumeration, variant) = self.constructor_binding(&path)?;
        let ty = Type::Enum(enumeration.clone());
        let context = self.generics.borrow();
        if context.members.method(&ty, &variant).is_some()
            || context.members.constants.iter().any(|c| {
                super::members::same_owner(&c.target, &ty) && c.declaration.name == variant
            })
        {
            return None;
        }
        Some(Constructor {
            enumeration,
            name: variant,
            args: payload,
            type_args,
            span: expr.span(),
        })
    }

    fn constructor_binding(&self, path: &str) -> Option<(Arc<EnumType>, String)> {
        if let Some((root, variant)) = path.rsplit_once('.') {
            if let Some(Type::Enum(enumeration)) = self.named_type(root) {
                return Some((enumeration.clone(), variant.into()));
            }
        }
        let binding = self.scope.constructors.get(path)?;
        let Type::Enum(enumeration) = self.source_nominals().get(&binding.enumeration.name)? else {
            return None;
        };
        (Some(enumeration.id) == binding.enumeration.declaration)
            .then(|| (enumeration.clone(), binding.variant.clone()))
    }

    pub(super) fn pattern_variant(&self, enumeration: &EnumType, name: &str) -> Option<String> {
        if let Some((bound, variant)) = self.constructor_binding(name) {
            (bound.id == enumeration.id).then_some(variant)
        } else if !name.contains('.') {
            Some(name.into())
        } else {
            None
        }
    }

    pub(super) fn constructor_type(
        &self,
        constructor: &Constructor<'_>,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        context: ConstructorContext<'_>,
    ) -> Result<Arc<EnumType>, Error> {
        let template = &constructor.enumeration;
        if template.parameters.is_empty() {
            if !constructor.type_args.is_empty() {
                return Err(type_error(
                    &constructor.span,
                    ErrorCode::InvalidCall,
                    "Non-generic enum constructors do not accept type arguments",
                ));
            }
            return Ok(template.clone());
        }
        let variant = template
            .variants
            .iter()
            .find(|v| v.name == constructor.name)
            .ok_or_else(|| {
                type_error(
                    &constructor.span,
                    ErrorCode::UnknownName,
                    format!(
                        "Enum `{}` has no variant `{}`",
                        template.name, constructor.name
                    ),
                )
            })?;
        if !constructor.type_args.is_empty()
            && constructor.type_args.len() != template.parameters.len()
        {
            return Err(type_error(
                &constructor.span,
                ErrorCode::InvalidCall,
                format!("Type argument count mismatch for enum `{}`", template.name),
            ));
        }
        let mut inference = generics::seed_inference(&template.parameters, constructor.type_args);
        if let Some(Type::Enum(target)) = context.expected {
            if target.id == template.id {
                generics::unify(
                    &Type::Enum(template.clone()),
                    &Type::Enum(target.clone()),
                    &template.parameters,
                    &mut inference.substitutions,
                    constructor.span,
                )?;
            }
        }
        generics::inference_passes(&mut inference, context.default, |inference| {
            if let (Some(formal), Some([arg])) = (&variant.payload, constructor.args) {
                self.infer_generic_argument(formal, arg, vars, functions, types, inference)?;
            }
            Ok(())
        })?;
        self.materialize_inference(&mut inference, types, constructor.span)?;
        let arguments = template.parameters.iter().map(|p| inference.substitutions.get(&p.id).cloned()
            .ok_or_else(|| type_error(&constructor.span, ErrorCode::TypeMismatch,
                format!("Cannot infer type parameter `{}` for `{}.{}`; add a type annotation or explicit type arguments", p.name, template.name, constructor.name))))
            .collect::<Result<Vec<_>, _>>()?;
        let ty = types
            .instantiate_enum(template, arguments)
            .map_err(|e| e.with_span(constructor.span))?;
        validate_nominal_type(&ty, &self.nominals, &constructor.span)?;
        let Type::Enum(enumeration) = ty else {
            unreachable!()
        };
        Ok(enumeration)
    }

    pub(super) fn enum_expr(
        &mut self,
        mut constructor: Constructor<'_>,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        constructor.enumeration = self.constructor_type(
            &constructor,
            vars,
            functions,
            types,
            ConstructorContext {
                expected,
                default: true,
            },
        )?;
        self.construct_enum(constructor, vars, functions, suspends, types)
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
            ..
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
            if matches!(scalar.ty, Type::Parameter(_)) {
                continue;
            }
            let value = match scalar.ty {
                Type::Int32 => Constant::Int32(if scalar.index == result.index {
                    tag as i32
                } else {
                    0
                }),
                Type::Bool => Constant::Bool(false),
                Type::Int64 => Constant::Int64(0),
                Type::UInt32 => Constant::UInt32(0),
                Type::UInt64 => Constant::UInt64(0),
                Type::Reference(_) => {
                    if cfg!(target_pointer_width = "64") {
                        Constant::UInt64(0)
                    } else {
                        Constant::UInt32(0)
                    }
                }
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
        self.local_scopes.push(Vec::new());
        let value = self.expr(value, &mut vars.clone(), functions, suspends, types)?;
        self.end_local_scope();
        let Flow::Value(value) = value else {
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
        let patterns = validate_patterns(self, enumeration, arms, span)?;
        let hint = [false, true].into_iter().find_map(|default| {
            arms.iter().zip(&patterns).find_map(|(arm, variant)| {
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
                        local.insert(name.clone(), payload_binding(ty, 0));
                    }
                }
                if default {
                    if expected.is_none() {
                        self.aggregate_default_hint(&arm.body, &local, functions, types)
                    } else {
                        None
                    }
                } else {
                    self.type_hint(&arm.body, &local, functions, types)
                }
            })
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
                        local.insert(
                            name.clone(),
                            payload_binding(ty, value.index + variant.offset),
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
            self.local_scopes.push(Vec::new());
            if let Pattern::Variant {
                binding: Some(name),
                ..
            } = &arm.pattern
            {
                if let Some(binding) = local.get(name).filter(|_| name != "_").cloned() {
                    let binding = self.local_binding(name, binding)?;
                    local.insert(name.clone(), binding);
                }
            }
            let flow =
                self.expr_expected(&arm.body, &mut local, functions, suspends, types, expected)?;
            self.end_local_scope();
            if let Flow::Value(arm_value) = flow {
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
    plan: &FunctionPlan,
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
                let variant_name = plan.pattern_variant(enumeration, name).ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::InvalidPattern,
                        format!(
                            "Pattern '{name}' does not belong to enum '{}'",
                            enumeration.name
                        ),
                    )
                })?;
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
