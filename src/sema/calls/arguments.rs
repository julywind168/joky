use crate::diagnostic::SemanticError;
use crate::syntax::{CallArgument, Expr};
use crate::Span;

use super::super::checker::Checker;
use super::super::types::Type;

impl Checker {
    pub(in crate::sema) fn order_arguments<'a>(
        &self,
        arguments: &'a [CallArgument],
        parameters: &[(String, Type)],
        function: &str,
        span: Span,
    ) -> Result<Vec<&'a Expr>, SemanticError> {
        if arguments.len() != parameters.len() {
            return Err(SemanticError::WrongArgumentCount {
                function: function.to_owned(),
                expected: parameters.len(),
                span,
            });
        }
        let mut ordered = vec![None; parameters.len()];
        let mut next_positional = 0;
        for argument in arguments {
            let index = if let Some(label) = &argument.label {
                parameters
                    .iter()
                    .position(|(name, _)| name == label)
                    .ok_or_else(|| SemanticError::UnknownFunction {
                        name: label.clone(),
                        span: argument.value.span,
                    })?
            } else {
                while next_positional < ordered.len() && ordered[next_positional].is_some() {
                    next_positional += 1;
                }
                if next_positional == ordered.len() {
                    return Err(SemanticError::WrongArgumentCount {
                        function: function.to_owned(),
                        expected: parameters.len(),
                        span: argument.value.span,
                    });
                }
                let index = next_positional;
                next_positional += 1;
                index
            };
            if ordered[index].is_some() {
                return Err(SemanticError::UnknownFunction {
                    name: parameters[index].0.clone(),
                    span: argument.value.span,
                });
            }
            ordered[index] = Some(&argument.value);
        }
        if ordered.iter().any(Option::is_none) {
            return Err(SemanticError::WrongArgumentCount {
                function: function.to_owned(),
                expected: parameters.len(),
                span,
            });
        }
        Ok(ordered.into_iter().map(Option::unwrap).collect())
    }

    pub(super) fn order_arguments_with_defaults<'a>(
        &self,
        arguments: &'a [CallArgument],
        parameters: &[(String, Type)],
        defaults: &[Option<Expr>],
        function: &str,
        span: Span,
    ) -> Result<Vec<Option<&'a Expr>>, SemanticError> {
        if arguments.len() > parameters.len() {
            return Err(SemanticError::WrongArgumentCount {
                function: function.to_owned(),
                expected: parameters.len(),
                span,
            });
        }
        let mut ordered = vec![None; parameters.len()];
        let mut next_positional = 0;
        for argument in arguments {
            let index = if let Some(label) = &argument.label {
                parameters
                    .iter()
                    .position(|(name, _)| name == label)
                    .ok_or_else(|| SemanticError::UnknownFunction {
                        name: label.clone(),
                        span: argument.value.span,
                    })?
            } else {
                while next_positional < ordered.len() && ordered[next_positional].is_some() {
                    next_positional += 1;
                }
                let index = next_positional;
                next_positional += 1;
                index
            };
            if index >= ordered.len() || ordered[index].is_some() {
                return Err(SemanticError::WrongArgumentCount {
                    function: function.to_owned(),
                    expected: parameters.len(),
                    span: argument.value.span,
                });
            }
            ordered[index] = Some(&argument.value);
        }
        for (index, value) in ordered.iter().enumerate() {
            if value.is_none() && defaults.get(index).and_then(Option::as_ref).is_none() {
                return Err(SemanticError::WrongArgumentCount {
                    function: function.to_owned(),
                    expected: parameters.len(),
                    span,
                });
            }
        }
        Ok(ordered)
    }
}
