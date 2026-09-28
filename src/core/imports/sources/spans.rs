//! Attribute embedded declarations to their import without reparsing cached ASTs.
use crate::{parser::ast::*, Span};

pub(super) fn relocate(module: &mut Module, span: Span) {
    for declaration in &mut module.enums {
        declaration.span = span;
        declaration.name_span = span;
        for variant in &mut declaration.variants {
            variant.span = span;
        }
    }
    for declaration in &mut module.records {
        declaration.span = span;
        declaration.name_span = span;
        for field in &mut declaration.fields {
            field.span = span;
        }
    }
    for implementation in &mut module.implementations {
        implementation.span = span;
        for method in &mut implementation.methods {
            function(method, span);
        }
        for constant in &mut implementation.constants {
            constant.span = span;
            expression(&mut constant.value, span);
        }
    }
    for item in &mut module.items {
        if let Item::FnDef(definition) = item {
            function(definition, span);
        }
    }
}

fn function(definition: &mut FnDef, span: Span) {
    definition.span = span;
    for parameter in &mut definition.params {
        parameter.span = span;
    }
    expression(&mut definition.body, span);
}

fn expression(expr: &mut Expr, span: Span) {
    *expr = std::mem::replace(expr, Expr::Break { span }).with_span(span);
    match expr {
        Expr::MethodCall(call) => {
            expression(&mut call.receiver, span);
            for arg in &mut call.args {
                expression(arg, span);
            }
        }
        Expr::Match { value, arms, .. } => {
            expression(value, span);
            for arm in arms {
                arm.span = span;
                match &mut arm.pattern {
                    Pattern::Wildcard { span: location }
                    | Pattern::Variant { span: location, .. } => *location = span,
                }
                expression(&mut arm.body, span);
            }
        }
        Expr::Record { fields, .. } => {
            for field in fields {
                field.span = span;
                expression(&mut field.value, span);
            }
        }
        Expr::Field { base, .. } => expression(base, span),
        Expr::FieldAssign { target, value, .. } => {
            expression(target, span);
            expression(value, span);
        }
        Expr::Destructure { pattern, value, .. } => {
            let mut pending = vec![pattern];
            while let Some(pattern) = pending.pop() {
                match pattern {
                    BindingPattern::Name { span: location, .. }
                    | BindingPattern::Discard { span: location } => *location = span,
                    BindingPattern::Tuple {
                        elements,
                        span: location,
                    } => {
                        *location = span;
                        pending.extend(elements);
                    }
                }
            }
            expression(value, span);
        }
        Expr::Let { value, .. }
        | Expr::Var { value, .. }
        | Expr::Assign { value, .. }
        | Expr::Unary { operand: value, .. }
        | Expr::Return {
            value: Some(value), ..
        } => expression(value, span),
        Expr::While {
            condition, body, ..
        } => {
            expression(condition, span);
            expression(body, span);
        }
        Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            expression(condition, span);
            expression(then_branch, span);
            if let Some(branch) = else_branch {
                expression(branch, span);
            }
        }
        Expr::BinOp { lhs, rhs, .. } | Expr::Logical { lhs, rhs, .. } => {
            expression(lhs, span);
            expression(rhs, span);
        }
        Expr::Call { args: exprs, .. }
        | Expr::Tuple {
            elements: exprs, ..
        }
        | Expr::Block { exprs, .. }
        | Expr::Sequence { exprs, .. }
        | Expr::Scope {
            branches: exprs, ..
        } => {
            for expr in exprs {
                expression(expr, span);
            }
        }
        Expr::Literal { .. }
        | Expr::Ident { .. }
        | Expr::GenericName(_)
        | Expr::FunctionRef { .. }
        | Expr::Return { value: None, .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => {}
    }
}
