//! Tuple construction and irrefutable bindings over flattened scalar slots.
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
                if result.ty.is_owner() {
                    self.register_owner(result.clone(), None);
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
                self.destructure_binding(pattern, *mutable, value, suspends, types, vars)?;
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
        BindingPattern::Record { inner, .. } => {
            for field in &inner.fields {
                validate_names(&field.pattern, names)?;
            }
        }
        BindingPattern::Discard { .. } => {}
    }
    Ok(())
}
