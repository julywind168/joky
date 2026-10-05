//! Sequential task scopes (`scope { ... }`) and dynamic child-task
//! registration (`branch { ... }`). The scope body is planned inline; each
//! branch block is lowered into a private entry function whose parameters are
//! the captured free variables, in first-use order.
use super::*;
use crate::parser::ast::{BindingPattern, Param, Pattern};
use std::sync::atomic::{AtomicU32, Ordering};

static TASK_ENTRY_COUNTER: AtomicU32 = AtomicU32::new(0);

/// One open `scope { ... }` block. Abrupt exits leaving the scope are routed
/// through its join so retained borrows end and child tasks complete first.
pub(super) struct TaskScopeFrame {
    pub(super) scope: u32,
    pub(super) exits: Vec<TaskScopeExit>,
}

/// One abrupt exit that crosses this scope: the exit site jumps to `label`,
/// the join runs there, and control continues to `next` afterwards.
pub(super) struct TaskScopeExit {
    pub(super) label: usize,
    pub(super) next: usize,
    /// Local scope depth whose bindings end after the join.
    pub(super) depth: usize,
}

impl FunctionPlan {
    pub(super) fn task_scope_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Expr::TaskScope { body, span } = expr else {
            unreachable!()
        };
        if !suspends {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "scope must be inside an @suspends function",
            ));
        }
        let scope = self.task_scope_count;
        self.task_scope_count += 1;
        self.task_scopes.push(TaskScopeFrame {
            scope,
            exits: Vec::new(),
        });
        self.emit(Op::TaskScopeEnter);
        self.local_scopes.push(Vec::new());
        let flow = self.expr_expected(body, vars, functions, suspends, types, Some(&Type::Unit))?;
        if let Flow::Value(value) = &flow {
            if slot_type(value) != Type::Unit {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "A task scope body must end with Unit, got {}",
                        slot_type(value)
                    ),
                ));
            }
        }
        let frame = self.task_scopes.pop().unwrap();
        // The scope-body locals stay live until the join: child tasks may hold
        // captures resolved from them, and their storage must not end early.
        let end_label = if matches!(flow, Flow::Value(_)) {
            self.emit(Op::TaskJoin { scope });
            let end_label = self.label_count;
            self.label_count += 1;
            self.emit(Op::Jump(end_label));
            Some(end_label)
        } else {
            None
        };
        for exit in &frame.exits {
            self.emit(Op::Label(exit.label));
            self.emit(Op::TaskJoin { scope });
            self.end_locals_from(exit.depth);
            self.emit(Op::Jump(exit.next));
        }
        self.end_local_scope();
        if let Some(end_label) = end_label {
            self.emit(Op::Label(end_label));
        }
        Ok(match flow {
            Flow::Value(_) => Flow::Value(None),
            flow => flow,
        })
    }

    pub(super) fn branch_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Expr::Branch { body, span } = expr else {
            unreachable!()
        };
        if !suspends {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "branch must be inside an @suspends function",
            ));
        }
        let Some(frame) = self.task_scopes.last() else {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "branch requires an enclosing scope in the same function",
            ));
        };
        let scope = frame.scope;
        let captures = free_captures(body, vars);
        let entry = format!(
            "__joky_task_{}",
            TASK_ENTRY_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let task_def = FnDef {
            noescape: None,
            public: false,
            buffer_intrinsic: None,
            compiler_intrinsic: None,
            buffer_element: None,
            scope: self.definition_scope.clone(),
            name: entry.clone(),
            type_params: Vec::new(),
            params: captures
                .iter()
                .map(|(name, ty)| Param {
                    name: name.clone(),
                    ty: ty.clone(),
                    span: *span,
                })
                .collect(),
            ret_ty: Type::Unit,
            suspends: true,
            body: (**body).clone(),
            span: *span,
        };
        let info = FunctionInfo {
            noescape: false,
            noescape_params: Vec::new(),
            compiler_intrinsic: None,
            host_text: false,
            text_ffi: false,
            c_abi: false,
            borrowing_host: false,
            signature: crate::FunctionSignature::new(
                captures.iter().map(|(_, ty)| ty.clone()).collect(),
                Type::Unit,
            ),
            suspends: true,
            may_fail: false,
            uses_arena: false,
            needs_arena: false,
            frame_slots: 0,
            internal: true,
            instance_entry: false,
            span: *span,
        };
        let plan = FunctionPlan::new(
            &task_def,
            functions,
            types,
            &self.nominals,
            self.generics.clone(),
            self.instantiation_depth,
        )?;
        // Captured values snapshot at registration: Copy inputs read the
        // current slot contents, owning inputs move out of the parent slot.
        let mut capture_slots = Vec::new();
        for (name, _) in &captures {
            let value = vars.get(name).and_then(|binding| binding.slot.clone());
            extend_argument(&mut capture_slots, value);
        }
        self.nested.push(NestedTask {
            name: entry.clone(),
            info,
            plan,
        });
        self.emit(Op::TaskBranch {
            scope,
            entry,
            captures: capture_slots,
        });
        Ok(Flow::Value(None))
    }

    /// Route an abrupt exit through every task scope it crosses: scopes
    /// `[from..]` were opened after the exit's target loop (or the exit is a
    /// return, so `from` is 0). Returns the label the exit site jumps to.
    pub(super) fn route_task_scope_exit(
        &mut self,
        original: usize,
        depth: usize,
        from: usize,
    ) -> usize {
        let crossed = self.task_scopes.len().saturating_sub(from);
        let mut labels = Vec::with_capacity(crossed);
        for _ in 0..crossed {
            labels.push(self.label_count);
            self.label_count += 1;
        }
        let mut next = original;
        for (frame, label) in self.task_scopes[from..].iter_mut().rev().zip(labels) {
            frame.exits.push(TaskScopeExit { label, next, depth });
            next = label;
        }
        next
    }
}

/// Collect the branch body's free variables that resolve to bindings visible
/// at the branch site, in first-use order. Shadowing mirrors planning: blocks
/// and control-flow arms open layers, sequences bind in place.
fn free_captures(body: &Expr, vars: &HashMap<String, Binding>) -> Vec<(String, Type)> {
    struct Walk<'a> {
        vars: &'a HashMap<String, Binding>,
        order: Vec<(String, Type)>,
    }
    impl Walk<'_> {
        fn capture(&mut self, name: &str, scopes: &[std::collections::HashSet<String>]) {
            if scopes.iter().any(|scope| scope.contains(name)) {
                return;
            }
            if self.order.iter().any(|(seen, _)| seen == name) {
                return;
            }
            if let Some(binding) = self.vars.get(name) {
                let ty = binding
                    .slot
                    .as_ref()
                    .map_or(Type::Unit, |slot| slot.ty.clone());
                self.order.push((name.to_owned(), ty));
            }
        }
    }
    fn binding_names(pattern: &BindingPattern, out: &mut Vec<String>) {
        match pattern {
            BindingPattern::Name { name, .. } => out.push(name.clone()),
            BindingPattern::Discard { .. } => {}
            BindingPattern::Tuple { elements, .. } => {
                for element in elements {
                    binding_names(element, out);
                }
            }
        }
    }
    fn visit(expr: &Expr, scopes: &mut Vec<std::collections::HashSet<String>>, walk: &mut Walk) {
        match expr {
            Expr::Ident { name, .. } => walk.capture(name, scopes),
            Expr::Assign { name, value, .. } => {
                // Captured bindings are immutable inputs inside the branch;
                // assigning to one is rejected when the entry is planned.
                walk.capture(name, scopes);
                visit(value, scopes, walk);
            }
            Expr::TaskScope { body, .. } | Expr::Branch { body, .. } => {
                scopes.push(std::collections::HashSet::new());
                visit(body, scopes, walk);
                scopes.pop();
            }
            Expr::Block { exprs, .. } | Expr::Unsafe { exprs, .. } | Expr::Graph { exprs, .. } => {
                scopes.push(std::collections::HashSet::new());
                for expr in exprs {
                    visit(expr, scopes, walk);
                }
                scopes.pop();
            }
            Expr::Sequence { exprs, .. } => {
                for expr in exprs {
                    visit(expr, scopes, walk);
                }
            }
            Expr::Let { name, value, .. } | Expr::Var { name, value, .. } => {
                visit(value, scopes, walk);
                scopes
                    .last_mut()
                    .expect("outermost layer")
                    .insert(name.clone());
            }
            Expr::Destructure { pattern, value, .. } => {
                visit(value, scopes, walk);
                let mut names = Vec::new();
                binding_names(pattern, &mut names);
                scopes.last_mut().expect("outermost layer").extend(names);
            }
            Expr::While {
                condition, body, ..
            } => {
                visit(condition, scopes, walk);
                scopes.push(std::collections::HashSet::new());
                visit(body, scopes, walk);
                scopes.pop();
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                visit(condition, scopes, walk);
                let branches = std::iter::once(then_branch.as_ref())
                    .chain(else_branch.iter().map(Box::as_ref));
                for branch in branches {
                    scopes.push(std::collections::HashSet::new());
                    visit(branch, scopes, walk);
                    scopes.pop();
                }
            }
            Expr::Match { value, arms, .. } => {
                visit(value, scopes, walk);
                for arm in arms {
                    scopes.push(std::collections::HashSet::new());
                    let mut names = Vec::new();
                    match &arm.pattern {
                        Pattern::Wildcard { .. } => {}
                        Pattern::Variant { binding, .. } => {
                            if let Some(binding) = binding {
                                binding.names(&mut names);
                            }
                        }
                    }
                    scopes.last_mut().expect("outermost layer").extend(names);
                    visit(&arm.body, scopes, walk);
                    scopes.pop();
                }
            }
            Expr::Tuple { elements, .. }
            | Expr::Scope {
                branches: elements, ..
            } => {
                for element in elements {
                    visit(element, scopes, walk);
                }
            }
            Expr::Record { fields, .. } => {
                for field in fields {
                    visit(&field.value, scopes, walk);
                }
            }
            Expr::Field { base, .. } => visit(base, scopes, walk),
            Expr::FieldAssign { target, value, .. } => {
                visit(target, scopes, walk);
                visit(value, scopes, walk);
            }
            Expr::Invoke { callee, args, .. } => {
                visit(callee, scopes, walk);
                for arg in args {
                    visit(arg, scopes, walk);
                }
            }
            Expr::Call { args, .. } => {
                for arg in args {
                    visit(arg, scopes, walk);
                }
            }
            Expr::MethodCall(call) => {
                visit(&call.receiver, scopes, walk);
                for arg in &call.args {
                    visit(arg, scopes, walk);
                }
            }
            Expr::Return { value, .. } => {
                if let Some(value) = value {
                    visit(value, scopes, walk);
                }
            }
            Expr::BinOp { lhs, rhs, .. } | Expr::Logical { lhs, rhs, .. } => {
                visit(lhs, scopes, walk);
                visit(rhs, scopes, walk);
            }
            Expr::Unary { operand, .. } => visit(operand, scopes, walk),
            Expr::Literal { .. }
            | Expr::GenericName(_)
            | Expr::FunctionRef { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => {}
        }
    }
    let mut scopes = vec![std::collections::HashSet::new()];
    let mut walk = Walk {
        vars,
        order: Vec::new(),
    };
    visit(body, &mut scopes, &mut walk);
    walk.order
}
