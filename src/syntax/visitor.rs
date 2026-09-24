//! Visitor pattern - traverse and transform the AST

use super::{BinaryOp, Expr, ExprKind, FieldAccess, Function, Program, TypeAnnotation, UnaryOp};

/// Expression visitor trait
///
/// Implement this trait to traverse the AST and perform operations on nodes.
///
/// # Examples
///
/// ```ignore
/// struct ExprCounter {
///     count: usize,
/// }
///
/// impl ExprVisitor for ExprCounter {
///     type Output = ();
///
///     fn visit_expr(&mut self, expr: &Expr) {
///         self.count += 1;
///         walk_expr(self, expr);
///     }
/// }
/// ```
pub trait ExprVisitor {
    /// Output type of visit operations
    type Output;

    /// Visit an expression node
    fn visit_expr(&mut self, expr: &Expr) -> Self::Output;

    /// Visit an integer literal
    fn visit_integer(&mut self, _value: u64, _expr: &Expr) -> Self::Output {
        self.default_output()
    }

    /// Visit a float literal
    fn visit_float(&mut self, _value: f64, _expr: &Expr) -> Self::Output {
        self.default_output()
    }

    /// Visit a duration literal (milliseconds)
    fn visit_duration(&mut self, _value: u64, _expr: &Expr) -> Self::Output {
        self.default_output()
    }

    /// Visit a string literal
    fn visit_string(&mut self, _value: &str, _expr: &Expr) -> Self::Output {
        self.default_output()
    }

    /// Visit a boolean literal
    fn visit_boolean(&mut self, _value: bool, _expr: &Expr) -> Self::Output {
        self.default_output()
    }

    /// Visit a name reference
    fn visit_name(&mut self, _name: &str, _expr: &Expr) -> Self::Output {
        self.default_output()
    }

    /// Visit a let binding
    fn visit_let(
        &mut self,
        _name: &str,
        _mutable: bool,
        _annotation: Option<&TypeAnnotation>,
        value: &Expr,
        _expr: &Expr,
    ) -> Self::Output {
        self.visit_expr(value)
    }

    /// Visit a unary expression
    fn visit_unary(&mut self, _op: UnaryOp, expression: &Expr, _expr: &Expr) -> Self::Output {
        self.visit_expr(expression)
    }

    /// Visit a binary expression
    fn visit_binary(
        &mut self,
        _op: BinaryOp,
        left: &Expr,
        right: &Expr,
        _expr: &Expr,
    ) -> Self::Output {
        self.visit_expr(left);
        self.visit_expr(right);
        self.default_output()
    }

    fn visit_let_pattern(
        &mut self,
        _pattern: &crate::syntax::Pattern,
        _mutable: bool,
        value: &Expr,
        _expr: &Expr,
    ) -> Self::Output {
        self.visit_expr(value);
        self.default_output()
    }

    /// Visit a function call
    fn visit_call(
        &mut self,
        callee: &Expr,
        arguments: &[crate::syntax::CallArgument],
        _expr: &Expr,
    ) -> Self::Output {
        self.visit_expr(callee);
        for arg in arguments {
            self.visit_expr(&arg.value);
        }
        self.default_output()
    }

    /// Visit a tuple literal
    fn visit_tuple(&mut self, elements: &[Expr], _expr: &Expr) -> Self::Output {
        for element in elements {
            self.visit_expr(element);
        }
        self.default_output()
    }

    /// Visit a struct construction expression
    fn visit_struct_init(
        &mut self,
        _name: &str,
        fields: &[(String, Expr)],
        _expr: &Expr,
    ) -> Self::Output {
        for (_, value) in fields {
            self.visit_expr(value);
        }
        self.default_output()
    }

    /// Visit a field access
    fn visit_field(&mut self, value: &Expr, _access: &FieldAccess, _expr: &Expr) -> Self::Output {
        self.visit_expr(value)
    }

    /// Visit an if expression
    fn visit_if(
        &mut self,
        condition: &Expr,
        then_branch: &Expr,
        else_branch: &Expr,
        _expr: &Expr,
    ) -> Self::Output {
        self.visit_expr(condition);
        self.visit_expr(then_branch);
        self.visit_expr(else_branch);
        self.default_output()
    }

    fn visit_match(
        &mut self,
        value: &Expr,
        arms: &[crate::syntax::MatchArm],
        _expr: &Expr,
    ) -> Self::Output {
        self.visit_expr(value);
        for arm in arms {
            self.visit_expr(&arm.value);
        }
        self.default_output()
    }

    fn visit_for(
        &mut self,
        _index: Option<&str>,
        _item: &str,
        iterable: &Expr,
        body: &Expr,
        limit: Option<&Expr>,
        _expr: &Expr,
    ) -> Self::Output {
        self.visit_expr(iterable);
        if let Some(limit) = limit {
            self.visit_expr(limit);
        }
        self.visit_expr(body);
        self.default_output()
    }

    /// Visit a while loop
    fn visit_while(&mut self, condition: &Expr, body: &Expr, _expr: &Expr) -> Self::Output {
        self.visit_expr(condition);
        self.visit_expr(body);
        self.default_output()
    }

    /// Visit a loop expression
    fn visit_loop(&mut self, body: &Expr, _expr: &Expr) -> Self::Output {
        self.visit_expr(body)
    }

    /// Visit a break expression
    fn visit_break(&mut self, value: Option<&Expr>, _expr: &Expr) -> Self::Output {
        if let Some(v) = value {
            self.visit_expr(v);
        }
        self.default_output()
    }

    /// Visit a Handler abort expression
    fn visit_abort(&mut self, value: &Expr, _expr: &Expr) -> Self::Output {
        self.visit_expr(value)
    }

    /// Visit a continue expression
    fn visit_continue(&mut self, _expr: &Expr) -> Self::Output {
        self.default_output()
    }

    /// Visit a Cown lease expression
    fn visit_when(
        &mut self,
        cowns: &[Expr],
        _bindings: Option<&[crate::syntax::Pattern]>,
        until: Option<&Expr>,
        body: &Expr,
        _expr: &Expr,
    ) -> Self::Output {
        for cown in cowns {
            self.visit_expr(cown);
        }
        if let Some(until) = until {
            self.visit_expr(until);
        }
        self.visit_expr(body);
        self.default_output()
    }

    /// Visit a block expression
    fn visit_block(&mut self, expressions: &[Expr], _expr: &Expr) -> Self::Output {
        for e in expressions {
            self.visit_expr(e);
        }
        self.default_output()
    }

    /// Default output value (for visitors that do not need a return value)
    fn default_output(&self) -> Self::Output;
}

/// Traverse all child nodes of an expression
///
/// Helper function that provides the default traversal behavior
pub fn walk_expr<V: ExprVisitor + ?Sized>(visitor: &mut V, expr: &Expr) -> V::Output {
    match &expr.kind {
        ExprKind::Integer(value) => visitor.visit_integer(*value, expr),
        ExprKind::Float(value) => visitor.visit_float(*value, expr),
        ExprKind::Duration(value) => visitor.visit_duration(*value, expr),
        ExprKind::String(value) => visitor.visit_string(value, expr),
        ExprKind::Bytes(value) => visitor.visit_string(value, expr),
        ExprKind::InterpolatedString(parts) => {
            for (_, expression) in parts {
                if let Some(expression) = expression {
                    visitor.visit_expr(expression);
                }
            }
            visitor.default_output()
        }
        ExprKind::Boolean(value) => visitor.visit_boolean(*value, expr),
        ExprKind::Name(name) => visitor.visit_name(name, expr),
        ExprKind::Let {
            name,
            mutable,
            annotation,
            value,
        } => visitor.visit_let(name, *mutable, annotation.as_ref(), value, expr),
        ExprKind::LetPattern {
            pattern,
            mutable,
            value,
            ..
        } => visitor.visit_let_pattern(pattern, *mutable, value, expr),
        ExprKind::Unary { op, expression } => visitor.visit_unary(*op, expression, expr),
        ExprKind::Unwrap { value, .. } => visitor.visit_expr(value),
        ExprKind::Cast { value, .. } => visitor.visit_expr(value),
        ExprKind::Binary { op, left, right } => visitor.visit_binary(*op, left, right, expr),
        ExprKind::Range {
            start, end, step, ..
        } => {
            visitor.visit_expr(start);
            visitor.visit_expr(end);
            if let Some(step) = step {
                visitor.visit_expr(step);
            }
            visitor.default_output()
        }
        ExprKind::Call {
            callee, arguments, ..
        } => visitor.visit_call(callee, arguments, expr),
        ExprKind::Closure { body, .. } => visitor.visit_expr(body),
        ExprKind::Tuple(elements) => visitor.visit_tuple(elements, expr),
        ExprKind::CollectionLiteral(literal) => match literal {
            super::CollectionLiteral::List(elements)
            | super::CollectionLiteral::MutList(elements)
            | super::CollectionLiteral::Set(elements)
            | super::CollectionLiteral::MutSet(elements) => {
                for element in elements {
                    visitor.visit_expr(element);
                }
                visitor.default_output()
            }
            super::CollectionLiteral::Map(entries) | super::CollectionLiteral::MutMap(entries) => {
                for entry in entries {
                    visitor.visit_expr(&entry.key);
                    visitor.visit_expr(&entry.value);
                }
                visitor.default_output()
            }
        },
        ExprKind::StructInit { name, fields } => visitor.visit_struct_init(name, fields, expr),
        ExprKind::AnonymousStruct { .. } => visitor.default_output(),
        ExprKind::AnonymousEnum { .. } => visitor.default_output(),
        ExprKind::Field { value, access } => visitor.visit_field(value, access, expr),
        ExprKind::Assign { target, value } => {
            visitor.visit_expr(target);
            visitor.visit_expr(value);
            visitor.default_output()
        }
        ExprKind::CompoundAssign { target, value, .. } => {
            visitor.visit_expr(target);
            visitor.visit_expr(value);
            visitor.default_output()
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => visitor.visit_if(condition, then_branch, else_branch, expr),
        ExprKind::Match { value, arms } => visitor.visit_match(value, arms, expr),
        ExprKind::For {
            index,
            item,
            iterable,
            body,
            limit,
        } => visitor.visit_for(
            index.as_deref(),
            item,
            iterable,
            body,
            limit.as_deref(),
            expr,
        ),
        ExprKind::While { condition, body } => visitor.visit_while(condition, body, expr),
        ExprKind::Loop { body } => visitor.visit_loop(body, expr),
        ExprKind::Break { value } => visitor.visit_break(value.as_deref(), expr),
        ExprKind::Abort { value } => visitor.visit_abort(value, expr),
        ExprKind::Continue => visitor.visit_continue(expr),
        ExprKind::When {
            cowns,
            bindings,
            until,
            body,
        } => visitor.visit_when(cowns, bindings.as_deref(), until.as_deref(), body, expr),
        ExprKind::Do { body, handlers } => {
            visitor.visit_expr(body);
            for handler in handlers {
                visitor.visit_expr(&handler.value);
            }
            visitor.default_output()
        }
        ExprKind::Parallel(arms) | ExprKind::Race(arms) => {
            for arm in arms {
                visitor.visit_expr(arm);
            }
            visitor.default_output()
        }
        ExprKind::Branch(body) | ExprKind::Region(body) => visitor.visit_expr(body),
        ExprKind::Block(expressions) => visitor.visit_block(expressions, expr),
    }
}

/// Visit all functions and class methods in a program
pub fn walk_program<V: ExprVisitor + ?Sized>(visitor: &mut V, program: &Program) {
    for constant in &program.constants {
        visitor.visit_expr(&constant.value);
    }
    for function in &program.functions {
        visitor.visit_expr(&function.body);
    }
    for class in &program.classes {
        for method in &class.methods {
            visitor.visit_expr(&method.body);
        }
    }
    for structure in &program.structs {
        for method in &structure.methods {
            visitor.visit_expr(&method.body);
        }
    }
}

/// Visit a single function
pub fn walk_function<V: ExprVisitor + ?Sized>(visitor: &mut V, function: &Function) {
    visitor.visit_expr(&function.body);
}
