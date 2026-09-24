//! Intern concrete body types before freezing the semantic table for HIR.

use std::collections::HashSet;

use crate::diagnostic::SemanticError;
use crate::syntax::{walk_expr, Expr, ExprVisitor, NodeId, Program};

use super::checker::Checker;

#[derive(Default)]
struct BodyNodes(Vec<NodeId>);

impl ExprVisitor for BodyNodes {
    type Output = ();

    fn visit_expr(&mut self, expression: &Expr) {
        self.0.push(expression.id);
        walk_expr(self, expression);
    }

    fn default_output(&self) {}
}

impl Checker {
    pub(super) fn prepare_instance_types(
        &mut self,
        program: &Program,
    ) -> Result<(), SemanticError> {
        let mut calls = self
            .generic_calls
            .iter()
            .map(|(id, call)| (call.clone(), self.generic_call_spans[id]))
            .collect::<Vec<_>>();
        calls.sort_by_key(|(_, span)| std::cmp::Reverse(span.start()));
        let mut pending = Vec::new();
        for (call, span) in calls {
            if call
                .arguments
                .iter()
                .all(|ty| self.substitute_type(*ty, &[]).is_some())
            {
                pending.push((call.function, call.arguments, span));
            }
        }
        let mut visited = HashSet::new();
        while let Some((name, arguments, span)) = pending.pop() {
            if !visited.insert((name.clone(), arguments.clone())) {
                continue;
            }
            let signature = self.functions[&name].clone();
            for (bounds, ty) in signature.type_parameter_bounds.iter().zip(&arguments) {
                for bound in bounds {
                    self.check_generic_bound_effects(*ty, bound, span)?;
                }
            }
            let substitutions = arguments.iter().copied().map(Some).collect::<Vec<_>>();
            self.check_associated_bounds(&signature.associated_bounds, &substitutions, span)?;
            if let Some(definition) = self.imported_templates.get(&name).cloned() {
                let parameters = signature
                    .parameters
                    .iter()
                    .map(|(name, ty)| {
                        (
                            name.clone(),
                            self.substitute_type(*ty, &substitutions)
                                .expect("concrete generic parameter"),
                        )
                    })
                    .collect();
                let return_type = self
                    .substitute_type(signature.return_type, &substitutions)
                    .expect("concrete generic result");
                self.generic_requests.push((
                    name,
                    crate::module::generics::GenericRequest {
                        definition,
                        arguments,
                        abi: super::ExternalFunction {
                            region_contract: None,
                            parameters,
                            return_type,
                            parameter_borrows: signature.parameter_borrows,
                            // Instantiation can reveal internal task waits or
                            // suspending trait implementations. The exported
                            // instance protocol always permits Pending.
                            suspends: true,
                        },
                    },
                ));
                continue;
            }
            let definition = program
                .functions
                .iter()
                .find(|f| f.name == name)
                .expect("a checked generic call has a definition");
            let substitutions = arguments.into_iter().map(Some).collect::<Vec<_>>();
            let mut nodes = BodyNodes::default();
            nodes.visit_expr(&definition.body);
            // Annotation spans identify source occurrences, not spellings:
            // two shadowed aliases with the same name can denote different types.
            let annotations = self
                .annotation_types
                .iter()
                .filter(|(span, _)| {
                    span.start() >= definition.span.start() && span.end() <= definition.span.end()
                })
                .map(|(span, ty)| (*span, *ty))
                .collect::<Vec<_>>();
            for (span, template) in annotations {
                let ty = self
                    .substitute_type(template, &substitutions)
                    .ok_or_else(|| SemanticError::UnknownType {
                        name: "generic annotation could not be instantiated".to_owned(),
                        span,
                    })?;
                self.check_list_types(ty, span)?;
            }
            for id in nodes.0 {
                if let Some(template) = self.types.get(&id).copied() {
                    if let Some(ty) = self.substitute_type(template, &substitutions) {
                        // Body-only collection types need the same concrete key
                        // checks as annotations, regardless of declaration order.
                        self.check_list_types(ty, definition.span)?;
                    }
                }
                if let Some(call) = self.generic_calls.get(&id).cloned() {
                    let concrete = call
                        .arguments
                        .iter()
                        .map(|ty| self.substitute_type(*ty, &substitutions))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| SemanticError::UnknownType {
                            name: "generic call could not be instantiated".to_owned(),
                            span: definition.span,
                        })?;
                    pending.push((call.function, concrete, self.generic_call_spans[&id]));
                }
            }
        }
        Ok(())
    }
}
