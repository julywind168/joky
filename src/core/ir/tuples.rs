//! Copy tuple construction and irrefutable bindings over flattened scalar slots.
use super::*;
use crate::parser::ast::BindingPattern;
use std::collections::HashSet;

impl FunctionPlan {
    pub(super) fn tuple_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        match expr {
            Expr::Tuple { elements, span } => {
                let mut values = Vec::with_capacity(elements.len());
                for (index, element) in elements.iter().enumerate() {
                    let Flow::Value(value) = self.expr_expected(
                        element,
                        vars,
                        functions,
                        suspends,
                        types,
                        match expected {
                            Some(Type::Tuple(tuple)) => tuple.elements.get(index),
                            _ => None,
                        },
                    )?
                    else {
                        if let Some(next) = elements.get(index + 1) {
                            return Err(unreachable_error(next));
                        }
                        return Ok(Flow::Diverged);
                    };
                    values.push(value);
                }
                let ty =
                    crate::core::types::tuple_type(values.iter().map(slot_type).collect(), types)
                        .map_err(|error| error.with_span(*span))?;
                let result = self.slot(ty)?;
                let mut offset = 0;
                for value in values.into_iter().flatten() {
                    let dst = Slot {
                        index: result.index + offset,
                        ty: value.ty.clone(),
                    };
                    offset += value.ty.scalar_types().len();
                    self.emit(Op::Copy(dst, value));
                }
                Ok(Flow::Value(Some(result)))
            }
            Expr::Destructure {
                pattern,
                mutable,
                annotation,
                value,
                span,
            } => {
                validate_names(pattern, &mut HashSet::new())?;
                if let Some(annotation) = annotation {
                    validate_nominal_type(annotation, &self.nominals, span)?;
                    types
                        .validate(annotation)
                        .map_err(|error| error.with_span(*span))?;
                }
                let Flow::Value(value) = self.expr_expected(
                    value,
                    vars,
                    functions,
                    suspends,
                    types,
                    annotation.as_ref(),
                )?
                else {
                    return Ok(Flow::Diverged);
                };
                if let Some(expected) = annotation {
                    if *expected != slot_type(&value) {
                        return Err(type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            format!(
                                "Binding annotation expects {expected}, got {}",
                                slot_type(&value)
                            ),
                        ));
                    }
                }
                // Resolve every leaf before publishing bindings. This preserves
                // simultaneous shadowing and prevents partially applied patterns.
                let mut leaves = Vec::new();
                destructure(pattern, value, &mut leaves)?;
                for (name, value) in leaves {
                    let slot = if *mutable {
                        self.snapshot(&value)?
                    } else {
                        value
                    };
                    vars.insert(
                        name.to_owned(),
                        Binding {
                            slot,
                            mutable: *mutable,
                        },
                    );
                }
                Ok(Flow::Value(None))
            }
            _ => unreachable!(),
        }
    }
}

fn validate_names<'a>(
    pattern: &'a BindingPattern,
    names: &mut HashSet<&'a str>,
) -> Result<(), Error> {
    match pattern {
        BindingPattern::Name { name, span } => {
            if !names.insert(name) {
                return Err(type_error(
                    span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate binding `{name}` in pattern"),
                ));
            }
        }
        BindingPattern::Tuple { elements, .. } => {
            for element in elements {
                validate_names(element, names)?;
            }
        }
        BindingPattern::Discard { .. } => {}
    }
    Ok(())
}

fn destructure<'a>(
    pattern: &'a BindingPattern,
    value: Option<Slot>,
    leaves: &mut Vec<(&'a str, Option<Slot>)>,
) -> Result<(), Error> {
    match pattern {
        BindingPattern::Discard { .. } => {}
        BindingPattern::Name { name, .. } => leaves.push((name, value)),
        BindingPattern::Tuple { elements, span } => {
            let ty = slot_type(&value);
            let Type::Tuple(tuple) = &ty else {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!("Tuple pattern requires a tuple, got {ty}"),
                ));
            };
            if elements.len() != tuple.elements.len() {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "Tuple pattern has {} elements, value has {}",
                        elements.len(),
                        tuple.elements.len()
                    ),
                ));
            }
            let mut offset = value.unwrap().index;
            for (pattern, ty) in elements.iter().zip(&tuple.elements) {
                let value = (*ty != Type::Unit).then(|| Slot {
                    index: offset,
                    ty: ty.clone(),
                });
                offset += ty.scalar_types().len();
                destructure(pattern, value, leaves)?;
            }
        }
    }
    Ok(())
}
