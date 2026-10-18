//! Enum construction and exhaustive matching, lowered to typed scalar storage.
use super::*;
use crate::parser::ast::EnumType;

use super::generics::ConstructorContext;

pub(super) struct Constructor<'a> {
    pub enumeration: Arc<EnumType>,
    pub name: String,
    pub args: Option<&'a [Expr]>,
    pub type_args: &'a [crate::parser::ast::GenericArgument],
    span: Span,
}

impl FunctionPlan {
    // Local values shadow constructor roots, as they shadow dotted calls.
    pub(super) fn enum_constructor<'a>(
        &self,
        expr: &'a Expr,
        vars: &HashMap<String, Binding>,
    ) -> Result<Option<Constructor<'a>>, Error> {
        let Some((path, payload, type_args)) = (|| {
            let parts: (String, _, &[crate::parser::ast::GenericArgument]) = match expr {
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
            Some(parts)
        })() else {
            return Ok(None);
        };
        if vars.contains_key(path.split('.').next().unwrap()) {
            return Ok(None);
        }
        let qualified = path
            .rsplit_once('.')
            .is_some_and(|(root, _)| matches!(self.named_type(root), Some(Type::Enum(_))));
        if !qualified
            && self
                .scope
                .constructors
                .get(&path)
                .is_some_and(|bindings| bindings.len() > 1)
        {
            return Err(type_error(
                &expr.span(),
                ErrorCode::UnknownName,
                format!("Ambiguous enum constructor '{path}'; qualify it with its enum type"),
            ));
        }
        let Some((enumeration, variant)) = self.constructor_binding(&path) else {
            return Ok(None);
        };

        let ty = Type::Enum(enumeration.clone());
        let context = self.generics.borrow();
        if context.members.method(&ty, &variant).is_some()
            || context.members.constants.iter().any(|c| {
                c.owner
                    .target()
                    .is_some_and(|target| super::members::same_owner(target, &ty))
                    && c.declaration.name == variant
            })
        {
            return Ok(None);
        }
        Ok(Some(Constructor {
            enumeration,
            name: variant,
            args: payload,
            type_args,
            span: expr.span(),
        }))
    }

    pub(super) fn constructor_binding(&self, path: &str) -> Option<(Arc<EnumType>, String)> {
        if let Some((root, variant)) = path.rsplit_once('.') {
            if let Some(Type::Enum(enumeration)) = self.named_type(root) {
                return Some((enumeration.clone(), variant.into()));
            }
        }
        let [binding] = self.scope.constructors.get(path)?.as_slice() else {
            return None;
        };
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
        let mut inference = generics::seed_inference(
            &template.parameters,
            constructor.type_args,
            constructor.span,
        )?;
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
        let arguments = template.parameters.iter().map(|p| inference.substitutions.argument(p)
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
        if result.ty.is_place() {
            self.emit(Op::Place(PlaceOp::Allocate(result.clone())));
            let tag_slot = self.slot(Type::Int32)?;
            self.emit(Op::Literal(tag_slot.clone(), Constant::Int32(tag as i32)));
            self.place_write(&result, 0, Some(tag_slot));
        }
        for scalar in result.scalars().filter(|_| !result.ty.is_place()) {
            if matches!(scalar.ty, Type::Parameter(_) | Type::Projection(_)) {
                continue;
            }
            let value = match scalar.ty {
                Type::Int32 => Constant::Int32(if scalar.index == result.index {
                    tag as i32
                } else {
                    0
                }),
                Type::Bool => Constant::Bool(false),
                Type::Int8 => Constant::Int8(0),
                Type::Int16 => Constant::Int16(0),
                Type::Int64 => Constant::Int64(0),
                Type::UInt8 => Constant::UInt8(0),
                Type::UInt16 => Constant::UInt16(0),
                Type::UInt32 => Constant::UInt32(0),
                Type::Rune => Constant::Rune(0),
                ref ty if ty.is_domain_capability() => Constant::UInt64(0),
                Type::UInt64 => Constant::UInt64(0),
                Type::Float32 => Constant::Float32(0.0),
                Type::Float64 => Constant::Float64(0.0),
                Type::Ptr
                | Type::Function(_)
                | Type::Buffer(_)
                | Type::String
                | Type::Resource(_) => Constant::NullPtr,
                Type::Reference(_) => {
                    if cfg!(target_pointer_width = "64") {
                        Constant::UInt64(0)
                    } else {
                        Constant::UInt32(0)
                    }
                }
                _ => unreachable!("enum layout contains only supported scalar lanes"),
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
            if result.ty.is_place() {
                self.place_write(&result, result.ty.place_child(tag).0, value);
            } else if let Some(value) = value {
                self.emit(Op::Copy(
                    Slot {
                        index: result.index + variant.offset,
                        ty: ty.clone(),
                    },
                    value,
                ));
            }
        }
        if result.ty.is_owner() {
            self.register_owner(result.clone(), None);
        }
        Ok(Flow::Value(Some(result)))
    }

    pub(super) fn match_type(
        &self,
        ty: &Type,
        types: &TypeRegistry,
        span: Span,
    ) -> Result<(Type, Option<bool>), Error> {
        match ty {
            Type::Enum(_) | Type::FixedArray(_) | Type::Bool | Type::Rune => Ok((ty.clone(), None)),
            _ if ty.integer().is_some() || matches!(ty, Type::Record(r) if r.array) => {
                Ok((ty.clone(), None))
            }
            Type::Reference(r) => {
                let target = self.pointee(&r.target, types)?;
                if matches!(target, Type::Enum(_) | Type::Bool | Type::Rune)
                    || target.integer().is_some()
                    || super::arrays::element_type(&target).is_some()
                {
                    return Ok((target, Some(r.mutable)));
                }
                Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "match requires an enum, integer, Bool, Rune, or sequence value or reference",
                ))
            }
            _ => Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                "match requires an enum, integer, Bool, Rune, or sequence value or reference",
            )),
        }
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
        let Expr::Match {
            value,
            arms,
            span,
            conditional,
        } = expr
        else {
            unreachable!()
        };
        self.local_scopes.push(Vec::new());
        let mut value = self.expr(value, &mut vars.clone(), functions, suspends, types)?;
        if let Flow::Value(Some(slot)) = &value {
            if slot.ty.is_owner() || slot.ty.is_place() {
                value = Flow::Value(self.snapshot(&Some(slot.clone()))?);
                self.escape_owner(&value);
            }
        }
        self.end_local_scope();
        let Flow::Value(value) = value else {
            return match arms.first() {
                Some(arm) => Err(unreachable_error(&arm.body)),
                None => Ok(Flow::Diverged),
            };
        };
        let Some(mut value) = value else {
            return Err(type_error(
                span,
                ErrorCode::TypeMismatch,
                "match requires a supported value",
            ));
        };
        let (matched_type, borrowed) = self.match_type(&value.ty, types, *span)?;
        if let Some(mutable) = borrowed {
            value.ty = Type::reference(matched_type.clone(), mutable);
            // Validate and reborrow even a wildcard-only match. Matching never
            // consumes the caller's exclusive reference capability.
            value = self.reference_value(Some(value), false)?.unwrap();
        }
        let patterns = if *conditional {
            // Both explicit bodies remain type checked even if the first
            // pattern is irrefutable; no user-written redundant arm is accepted.
            arms.iter()
                .map(|arm| self.check_match_pattern(&arm.pattern, &matched_type, types))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            patterns::validate(self, &matched_type, arms, *span, types)?
        };
        let hint = [false, true].into_iter().find_map(|default| {
            arms.iter().zip(&patterns).find_map(|(arm, pattern)| {
                let mut local = vars.clone();
                pattern.hints(&mut local, borrowed);
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
        let expected = if expected.is_some_and(|ty| {
            ty.dyn_interface().is_some() || super::arrays::element_type(ty).is_some()
        }) {
            expected
        } else {
            hint.as_ref().or(expected)
        };
        let exit = self.label_count;
        self.label_count += 1;
        // The outer Option tracks normal completion, including a Unit value.
        let mut result: Option<Option<Slot>> = None;
        for (arm_index, (arm, pattern)) in arms.iter().zip(patterns).enumerate() {
            self.current_span = arm.pattern.span();
            let mut local = vars.clone();
            let next = if arm_index + 1 < arms.len() {
                let next = self.label_count;
                self.label_count += 1;
                if borrowed.is_some() {
                    self.borrowed_pattern_tests(&pattern, &value, 0, next)?;
                } else {
                    self.match_pattern_tests(&pattern, value.index, next)?;
                }
                Some(next)
            } else {
                None
            };
            self.local_scopes.push(Vec::new());
            if value.ty.is_owner() || value.ty.is_place() {
                self.local_scopes.last_mut().unwrap().push(value.clone());
            }
            if borrowed.is_some() {
                self.bind_borrowed_pattern(
                    &pattern,
                    &value,
                    0,
                    &mut Vec::new(),
                    false,
                    &mut local,
                )?;
            } else {
                self.bind_match_pattern(&pattern, Some(value.clone()), None, &mut local)?;
            }
            let flow =
                self.expr_expected(&arm.body, &mut local, functions, suspends, types, expected)?;
            // Copy metadata may still address the consumed enum's payload.
            // Preserve the arm result before ending that owner and its storage.
            let flow = if value.ty.is_owner() {
                match flow {
                    Flow::Value(result) => Flow::Value(self.snapshot(&result)?),
                    flow => flow,
                }
            } else {
                flow
            };
            self.escape_owner(&flow);
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
