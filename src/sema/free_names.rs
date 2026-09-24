//! Lexical free names shared by closure and repeated-task capture checking.
use crate::syntax::{Expr, ExprKind, ExprVisitor, Pattern};
use crate::Span;
use std::collections::{BTreeMap, HashSet};

pub(super) fn free_names(
    expression: &Expr,
    bound: impl IntoIterator<Item = String>,
) -> BTreeMap<String, Span> {
    let mut collector = Collector {
        bound: vec![bound.into_iter().collect()],
        names: BTreeMap::new(),
    };
    collector.visit_expr(expression);
    collector.names
}

struct Collector {
    bound: Vec<HashSet<String>>,
    names: BTreeMap<String, Span>,
}

impl Collector {
    fn bind_pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Binding { name, .. } => {
                self.bound.last_mut().unwrap().insert(name.clone());
            }
            Pattern::EnumVariant { fields, .. } => {
                for field in fields {
                    self.bind_pattern(&field.pattern);
                }
            }
            Pattern::Tuple { elements, .. } => {
                for element in elements {
                    self.bind_pattern(element);
                }
            }
            Pattern::Wildcard { .. } => {}
        }
    }
    fn scoped(&mut self, expression: &Expr) {
        self.bound.push(HashSet::new());
        self.visit_expr(expression);
        self.bound.pop();
    }
}

impl ExprVisitor for Collector {
    type Output = ();
    fn default_output(&self) {}
    fn visit_expr(&mut self, expression: &Expr) {
        match &expression.kind {
            ExprKind::Name(name) => {
                if !self.bound.iter().rev().any(|scope| scope.contains(name)) {
                    self.names.entry(name.clone()).or_insert(expression.span);
                }
            }
            ExprKind::Let { name, value, .. } => {
                self.visit_expr(value);
                self.bound.last_mut().unwrap().insert(name.clone());
            }
            ExprKind::LetPattern { pattern, value, .. } => {
                self.visit_expr(value);
                self.bind_pattern(pattern);
            }
            ExprKind::Block(expressions) => {
                self.bound.push(HashSet::new());
                for expression in expressions {
                    self.visit_expr(expression);
                }
                self.bound.pop();
            }
            ExprKind::For {
                index,
                item,
                iterable,
                body,
                limit,
            } => {
                self.visit_expr(iterable);
                if let Some(limit) = limit {
                    self.visit_expr(limit);
                }
                self.bound
                    .push(std::iter::once(item.clone()).chain(index.clone()).collect());
                self.visit_expr(body);
                self.bound.pop();
            }
            ExprKind::Closure {
                parameters, body, ..
            } => {
                self.bound.push(
                    parameters
                        .iter()
                        .map(|parameter| parameter.name.clone())
                        .collect(),
                );
                self.visit_expr(body);
                self.bound.pop();
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.visit_expr(condition);
                self.scoped(then_branch);
                self.scoped(else_branch);
            }
            ExprKind::Match { value, arms } => {
                self.visit_expr(value);
                for arm in arms {
                    self.bound.push(HashSet::new());
                    self.bind_pattern(&arm.pattern);
                    self.visit_expr(&arm.value);
                    self.bound.pop();
                }
            }
            ExprKind::Do { body, handlers } => {
                self.scoped(body);
                for handler in handlers {
                    self.bound.push(HashSet::new());
                    for parameter in &handler.parameters {
                        self.bind_pattern(parameter);
                    }
                    self.visit_expr(&handler.value);
                    self.bound.pop();
                }
            }
            ExprKind::Parallel(arms) | ExprKind::Race(arms) => {
                for arm in arms {
                    self.scoped(arm);
                }
            }
            ExprKind::When {
                cowns,
                bindings,
                until,
                body,
            } => {
                for cown in cowns {
                    self.visit_expr(cown);
                }
                self.bound.push(HashSet::new());
                for binding in bindings.iter().flatten() {
                    self.bind_pattern(binding);
                }
                if let Some(until) = until {
                    self.visit_expr(until);
                }
                self.visit_expr(body);
                self.bound.pop();
            }
            _ => crate::syntax::walk_expr(self, expression),
        }
    }
}
