//! Sequential task scopes (`scope { ... }`) and dynamic child-task
//! registration (`branch { ... }`). The scope body is planned inline; each
//! branch block is lowered into a private entry function whose parameters are
//! the captured free variables, in first-use order.
use super::*;
use crate::parser::ast::{BindingPattern, Param};
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
        expected: Option<&Type>,
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
        // This block's lexical lifetime includes its join. Planning it as an
        // ordinary nested block would end its locals before children finish.
        let flow = if matches!(body.as_ref(), Expr::Block { .. }) {
            self.sequence(
                body,
                expected,
                &mut vars.clone(),
                functions,
                suspends,
                types,
            )?
        } else {
            self.expr_expected(body, vars, functions, suspends, types, expected)?
        };
        let flow = match flow {
            Flow::Value(value) => {
                let ty = slot_type(&value);
                if ty.contains_references()
                    || ty.contains_closures()
                    || ty.contains_functions()
                    || (ty.is_owner() && !ty.is_suspend_safe_owner())
                {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        format!(
                            "{} results must be reference-free values, got {ty}",
                            self.branch_context
                                .filter(|_| *span == self.span)
                                .map_or("scope", |(kind, _)| kind)
                        ),
                    ));
                }
                // Snapshot before joining: a child may mutate a captured local.
                Flow::Value(self.snapshot(&value)?)
            }
            flow => flow,
        };
        self.escape_owner(&flow);
        let frame = self.task_scopes.pop().unwrap();
        // The scope-body locals stay live until the join: child tasks may hold
        // captures resolved from them, and their storage must not end early.
        let end_label = if matches!(flow, Flow::Value(_)) {
            let state = self.state_count;
            self.state_count += 1;
            self.emit(Op::TaskJoin { scope, state });
            let end_label = self.label_count;
            self.label_count += 1;
            self.emit(Op::Jump(end_label));
            Some(end_label)
        } else {
            None
        };
        for exit in &frame.exits {
            self.emit(Op::Label(exit.label));
            let state = self.state_count;
            self.state_count += 1;
            self.emit(Op::TaskJoin { scope, state });
            self.end_locals_from(exit.depth);
            self.emit(Op::Jump(exit.next));
        }
        if let Some(end_label) = end_label {
            self.emit(Op::Label(end_label));
        }
        self.end_local_scope();
        Ok(flow)
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
        let (entry, capture_slots, _) = self.nested_task(
            body,
            ("scope branch", *span),
            Some(Type::Unit),
            vars,
            functions,
            types,
        )?;
        self.emit(Op::TaskBranch {
            scope,
            entry,
            captures: capture_slots,
        });
        Ok(Flow::Value(None))
    }

    pub(super) fn nested_task(
        &mut self,
        body: &Expr,
        (kind, span): (&'static str, Span),
        ret: Option<Type>,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<(String, Vec<Option<Slot>>, Type), Error> {
        let infer_return = ret.is_none();
        let ret = ret.unwrap_or(Type::Unit);
        let captures = free_captures(body, vars);
        let entry = format!(
            "__joky_task_{}",
            TASK_ENTRY_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let task_def = FnDef {
            every: None,
            kind: crate::parser::ast::FunctionKind::Ordinary,
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
                    mutable: false,
                    name: name.clone(),
                    ty: ty.clone(),
                    span,
                })
                .collect(),
            ret_ty: ret.clone(),
            suspends: true,
            // A child owns its descendants even without an explicit scope in
            // its body. Returns leave this scope through the usual join path.
            body: Expr::TaskScope {
                body: Box::new(body.clone()),
                span,
            },
            span,
        };
        let mut info = FunctionInfo {
            every: None,
            closure_entry: false,
            noescape: false,
            noescape_params: Vec::new(),
            infer_noescape: true,
            inferred_noescape: Vec::new(),
            inferred_retained: None,
            inferred_arena_retained: Vec::new(),
            call_region: false,
            returns_interior: true,
            compiler_intrinsic: None,
            builtin: None,
            extern_abi: crate::core::builtins::ExternAbi::Scalar,
            signature: crate::FunctionSignature::new(
                captures.iter().map(|(_, ty)| ty.clone()).collect(),
                ret,
            ),
            suspends: true,
            uses_concurrency: false,
            may_fail: false,
            uses_arena: false,
            needs_arena: false,
            frame_slots: 0,
            internal: true,
            instance_entry: false,
            domain_entry: false,
            domain_request_entry: false,
            script_entry: false,
            parallel_entry: false,
            span,
        };
        self.generics
            .borrow_mut()
            .task_parents
            .insert(task_def.name.clone(), (self.lexical_function.clone(), kind));
        let plan = FunctionPlan::new_with_return_inference(
            &task_def,
            functions,
            types,
            &self.nominals,
            self.generics.clone(),
            self.instantiation_depth,
            infer_return,
        )?;
        let ret = plan.return_type.clone();
        info.signature.ret = ret.clone();
        // Captured values snapshot at registration: Copy inputs read the
        // current slot contents, owning inputs move out of the parent slot.
        let capture_args: Vec<_> = captures
            .iter()
            .map(|(name, _)| Expr::Ident {
                name: name.clone(),
                span,
            })
            .collect();
        let capture_slots = self
            .checked_arguments(
                &entry,
                &capture_args,
                &info.signature.params,
                None,
                span,
                |_| None,
                &mut vars.clone(),
                functions,
                true,
                types,
            )?
            .expect("capture identifiers cannot diverge");
        self.nested.push(NestedTask {
            name: entry.clone(),
            info,
            plan,
        });
        Ok((entry, capture_slots, ret))
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
            BindingPattern::Sequence(p) => {
                for item in p.prefix.iter().chain(&p.suffix) {
                    binding_names(item, out);
                }
                if let Some(Some(name)) = &p.rest {
                    out.push(name.clone());
                }
            }
            BindingPattern::Name { name, .. } => out.push(name.clone()),
            BindingPattern::Discard { .. } => {}
            BindingPattern::Tuple { elements, .. } => {
                for element in elements {
                    binding_names(element, out);
                }
            }
            BindingPattern::Record { inner, .. } => {
                for field in &inner.fields {
                    binding_names(&field.pattern, out);
                }
            }
        }
    }
    fn visit(expr: &Expr, scopes: &mut Vec<std::collections::HashSet<String>>, walk: &mut Walk) {
        match expr {
            Expr::When(w) => {
                scopes.push(std::collections::HashSet::new());
                for (index, (name, _)) in w.accesses.iter().enumerate() {
                    if let Some(Some(path)) = w.paths.get(index) {
                        visit(&path.directory, scopes, walk);
                        if let Some(create) = &path.create {
                            visit(create, scopes, walk);
                        }
                        visit(&path.key, scopes, walk);
                    } else {
                        walk.capture(name, scopes);
                    }
                    scopes.last_mut().unwrap().insert(name.clone());
                }
                visit(&w.body, scopes, walk);
                scopes
                    .last_mut()
                    .unwrap()
                    .remove(&w.accesses.last().unwrap().0);
                for body in w.missing.iter().chain(w.alias.iter()) {
                    visit(body, scopes, walk);
                }
                scopes.pop();
            }
            Expr::AnonymousFunction(f) => {
                scopes.push(f.params.iter().map(|p| p.name.clone()).collect());
                visit(&f.body, scopes, walk);
                scopes.pop();
            }
            Expr::Ident { name, .. } => walk.capture(name, scopes),
            Expr::Assign { name, value, .. } => {
                // Captured bindings are immutable inputs inside the branch;
                // assigning to one is rejected when the entry is planned.
                walk.capture(name, scopes);
                visit(value, scopes, walk);
            }
            Expr::Isolate { body, .. }
            | Expr::TaskScope { body, .. }
            | Expr::Branch { body, .. } => {
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
            Expr::LetElse(binding) => {
                for child in [&binding.value, &binding.fallback] {
                    scopes.push(std::collections::HashSet::new());
                    visit(child, scopes, walk);
                    scopes.pop();
                }
                scopes
                    .last_mut()
                    .expect("outermost layer")
                    .extend(binding.pattern.binding_names());
            }
            Expr::IndexedLoop(loop_) => {
                scopes.push(std::collections::HashSet::from([loop_.name.clone()]));
                visit(&loop_.body, scopes, walk);
                scopes.pop();
            }
            Expr::While {
                condition, body, ..
            } => {
                visit(condition, scopes, walk);
                scopes.push(std::collections::HashSet::new());
                visit(body, scopes, walk);
                scopes.pop();
            }
            Expr::BinaryChain { .. } => {
                for child in expr.children() {
                    visit(child, scopes, walk);
                }
            }
            Expr::IfChain { .. } => {
                for child in expr.children() {
                    scopes.push(std::collections::HashSet::new());
                    visit(child, scopes, walk);
                    scopes.pop();
                }
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
                    if let Some(pattern) = arm.pattern.payload() {
                        pattern.names(&mut names);
                    }
                    scopes.last_mut().expect("outermost layer").extend(names);
                    if let Some(guard) = &arm.guard {
                        scopes.push(std::collections::HashSet::new());
                        visit(guard, scopes, walk);
                        scopes.pop();
                    }
                    visit(&arm.body, scopes, walk);
                    scopes.pop();
                }
            }
            Expr::Array { elements, .. }
            | Expr::Tuple { elements, .. }
            | Expr::Concurrent {
                branches: elements, ..
            } => {
                for element in elements {
                    visit(element, scopes, walk);
                }
            }
            Expr::Record { fields, base, .. } => {
                for field in fields {
                    visit(&field.value, scopes, walk);
                }
                if let Some(base) = base {
                    visit(base, scopes, walk);
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
            Expr::Call { callee, args, .. } => {
                // Named calls are resolved after capture planning: the root
                // can be a local function value or a method receiver.
                walk.capture(callee.split('.').next().unwrap(), scopes);
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
            Expr::Index {
                base: lhs,
                index: rhs,
                ..
            }
            | Expr::BinOp { lhs, rhs, .. }
            | Expr::Logical { lhs, rhs, .. } => {
                visit(lhs, scopes, walk);
                visit(rhs, scopes, walk);
            }
            Expr::Unary { operand, .. } | Expr::Spread { value: operand, .. } => {
                visit(operand, scopes, walk)
            }
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
