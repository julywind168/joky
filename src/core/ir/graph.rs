//! Lexically scoped construction capabilities. They never become source values.
use super::*;

pub(crate) enum GraphOp {
    Link {
        reference: Slot,
        source: Slot,
        path: Vec<String>,
        value: Slot,
        tag: i32,
        offset: usize,
    },
    Seal {
        dst: Slot,
        root: Slot,
        members: Vec<Slot>,
    },
}

fn statements<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Sequence { exprs, .. } = expr {
        for expr in exprs {
            statements(expr, out);
        }
    } else {
        out.push(expr);
    }
}

fn has_tail(expr: &Expr) -> bool {
    match expr {
        Expr::Sequence {
            exprs, trailing, ..
        } => !trailing && exprs.last().is_some_and(has_tail),
        _ => true,
    }
}

impl FunctionPlan {
    pub(super) fn graph_hint(
        &self,
        exprs: &[Expr],
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        default: bool,
    ) -> Option<Type> {
        let mut body = Vec::new();
        for expr in exprs {
            statements(expr, &mut body);
        }
        let (root, body) = body.split_last()?;
        let mut local = vars.clone();
        for expr in body {
            if let Expr::Let { name, value, .. } = expr {
                if let Expr::Call { callee, args, .. } = value.as_ref() {
                    if callee == "graph.alloc" && args.len() == 1 {
                        let ty = self.hint(&args[0], &local, functions, types, default)?;
                        local.insert(
                            name.clone(),
                            Binding {
                                place: None,
                                slot: Some(Slot {
                                    index: 0,
                                    ty: Type::reference(ty, true),
                                }),
                                mutable: false,
                            },
                        );
                        continue;
                    }
                }
            }
            self.hint_bindings(expr, &mut local, functions, types);
        }
        let root = self.hint(root, &local, functions, types, default)?;
        Some(Type::reference(
            root.reference_type()?.target.clone(),
            false,
        ))
    }

    pub(super) fn check_graph_expression(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
    ) -> Result<(), Error> {
        let path = match expr {
            Expr::Call { callee, .. } => Some(callee.clone()),
            Expr::Assign { name, .. } => Some(name.clone()),
            Expr::FieldAssign { target, .. } => expression_path(target),
            Expr::MethodCall(call) => expression_path(&call.receiver),
            _ => expression_path(expr),
        };
        if let Some(name) = path.as_deref().and_then(|path| path.split('.').next()) {
            if vars
                .get(name)
                .and_then(|binding| binding.slot.as_ref())
                .is_some_and(|slot| self.graph_handles.contains(&slot.index))
            {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::InvalidAssignment,
                    "Construction handles can only be graph.link endpoints or the graph root",
                ));
            }
        }
        if let Some(depth) = self.graph_loop_depth {
            if matches!(expr, Expr::Return { .. })
                || matches!(expr, Expr::Break { .. } | Expr::Continue { .. })
                    && self.loops.len() <= depth
            {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::InvalidControlFlow,
                    "Control flow cannot leave a graph block before sealing",
                ));
            }
        }
        if let Expr::Call { callee, span, .. } = expr {
            if matches!(callee.as_str(), "graph.alloc" | "graph.link")
                && (self.graph_loop_depth.is_some()
                    || !vars.contains_key("graph") && !self.scope.modules.contains_key("graph"))
            {
                return Err(type_error(span, ErrorCode::InvalidCall,
                    "graph.alloc requires a direct let binding and graph.link a direct statement inside graph"));
            }
        }
        Ok(())
    }

    pub(super) fn graph_expr(
        &mut self,
        exprs: &[Expr],
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        if self.graph_loop_depth.is_some() {
            return Err(type_error(
                &self.current_span,
                ErrorCode::InvalidCall,
                "graph blocks must be synchronous and cannot be nested",
            ));
        }
        self.graph_loop_depth = Some(self.loops.len());
        self.local_scopes.push(Vec::new());
        let result = self.graph_body(exprs, &mut vars.clone(), functions, types);
        self.end_local_scope();
        self.graph_loop_depth = None;
        result
    }

    fn graph_body(
        &mut self,
        exprs: &[Expr],
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let span = self.current_span;
        let mut body = Vec::new();
        for expr in exprs {
            statements(expr, &mut body);
        }
        let Some((root, body)) = body.split_last() else {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                "A graph block must end with a construction handle",
            ));
        };
        if !exprs.last().is_some_and(has_tail) || !matches!(root, Expr::Ident { .. }) {
            return Err(type_error(
                &root.span(),
                ErrorCode::TypeMismatch,
                "A graph block must end with a bare construction handle without a semicolon",
            ));
        }
        let mut members = Vec::new();
        for expr in body {
            self.current_span = expr.span();
            if let Expr::Let {
                name,
                annotation,
                value,
                span,
            } = expr
            {
                if let Expr::Call {
                    callee,
                    type_args,
                    args,
                    span: call_span,
                } = value.as_ref()
                {
                    if callee == "graph.alloc" {
                        if annotation.is_some() || !type_args.is_empty() || args.len() != 1 {
                            return Err(type_error(span, ErrorCode::InvalidCall,
                                "graph.alloc requires one initializer and an unannotated let binding"));
                        }
                        let Flow::Value(value) =
                            self.expr(&args[0], vars, functions, false, types)?
                        else {
                            return Err(unreachable_error(root));
                        };
                        let target = slot_type(&value);
                        if !matches!(target, Type::Record(_)) || !target.is_copy_value() {
                            return Err(type_error(
                                call_span,
                                ErrorCode::TypeMismatch,
                                "graph.alloc requires a fully initialized Copy record",
                            ));
                        }
                        let dst = self.slot(Type::reference(target.clone(), true))?;
                        self.current_span = *call_span;
                        self.emit(Op::Alloc {
                            dst: dst.clone(),
                            value,
                            target,
                        });
                        self.graph_handles.insert(dst.index);
                        members.push(dst.clone());
                        vars.insert(
                            name.clone(),
                            Binding {
                                place: None,
                                slot: Some(dst),
                                mutable: false,
                            },
                        );
                        continue;
                    }
                }
            }
            if let Expr::Call {
                callee,
                args,
                type_args,
                span,
            } = expr
            {
                if callee == "graph.link" {
                    if !type_args.is_empty() || args.len() != 2 {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidCall,
                            "graph.link expects a destination field and a construction handle",
                        ));
                    }
                    self.graph_link(&args[0], &args[1], vars, &members)?;
                    continue;
                }
            }
            if matches!(
                self.expr(expr, vars, functions, false, types)?,
                Flow::Diverged
            ) {
                return Err(unreachable_error(root));
            }
        }
        let root = self.graph_handle(root, vars, &members)?;
        let dst = self.slot(Type::reference(
            root.ty.reference_type().unwrap().target.clone(),
            false,
        ))?;
        self.current_span = span;
        self.emit(Op::Graph(Box::new(GraphOp::Seal {
            dst: dst.clone(),
            root,
            members,
        })));
        Ok(Flow::Value(Some(dst)))
    }

    fn graph_handle(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        members: &[Slot],
    ) -> Result<Slot, Error> {
        if let Expr::Ident { name, .. } = expr {
            if let Some(slot) = vars.get(name).and_then(|binding| binding.slot.as_ref()) {
                if members.iter().any(|member| member.index == slot.index) {
                    return Ok(slot.clone());
                }
            }
        }
        Err(type_error(
            &expr.span(),
            ErrorCode::TypeMismatch,
            "Expected a construction handle allocated in this graph block",
        ))
    }

    fn graph_link(
        &mut self,
        destination: &Expr,
        source: &Expr,
        vars: &HashMap<String, Binding>,
        members: &[Slot],
    ) -> Result<(), Error> {
        let source = self.graph_handle(source, vars, members)?;
        let mut base = destination;
        let mut path = Vec::new();
        while let Expr::Field {
            base: parent,
            name,
            type_args,
            ..
        } = base
        {
            if !type_args.is_empty() {
                return Err(type_error(
                    &base.span(),
                    ErrorCode::InvalidCall,
                    "Graph link fields do not accept type arguments",
                ));
            }
            path.push(name.clone());
            base = parent;
        }
        path.reverse();
        let reference = self.graph_handle(base, vars, members)?;
        let mut target = reference.ty.reference_type().unwrap().target.clone();
        for name in &path {
            target = self.field_member(&target, name, &destination.span())?.1;
        }
        let invalid = || {
            type_error(
                &destination.span(),
                ErrorCode::TypeMismatch,
                "graph.link requires a matching standard Option<&T> destination field",
            )
        };
        let Type::Enum(option) = &target else {
            return Err(invalid());
        };
        // The resolver owns canonical module names. Check the declaration in
        // this compilation, including ASTs resolved by another TypeRegistry.
        let standard = self.nominals.get("joky/option.Option");
        if path.is_empty() || !matches!(standard, Some(Type::Enum(ty)) if ty.id == option.id) {
            return Err(invalid());
        }
        let (tag, variant) = option
            .variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == "Some")
            .ok_or_else(invalid)?;
        let Some(payload) = variant.payload.as_ref().and_then(Type::reference_type) else {
            return Err(invalid());
        };
        if payload.mutable || payload.target != source.ty.reference_type().unwrap().target {
            return Err(invalid());
        }
        let tag = tag as i32;
        let offset = variant.offset;
        let value = self.slot(target)?;
        self.emit(Op::Graph(Box::new(GraphOp::Link {
            reference,
            source,
            path,
            value,
            tag,
            offset,
        })));
        Ok(())
    }

    /// Erase capabilities only after shared semantic analysis has accepted them.
    /// The backends then use the existing arena allocation/store/copy operations.
    pub(super) fn lower_graph_ops(&mut self) {
        if !self.ops.iter().any(|op| matches!(op, Op::Graph(_))) {
            return;
        }
        let ops = std::mem::take(&mut self.ops);
        let spans = std::mem::take(&mut self.op_spans);
        for (op, span) in ops.into_iter().zip(spans) {
            self.current_span = span;
            match op {
                Op::Graph(op) => match *op {
                    GraphOp::Link {
                        reference,
                        source,
                        path,
                        value,
                        tag,
                        offset,
                    } => {
                        self.emit(Op::Literal(
                            Slot {
                                index: value.index,
                                ty: Type::Int32,
                            },
                            Constant::Int32(tag),
                        ));
                        let payload = Slot {
                            index: value.index + offset,
                            ty: Type::reference(
                                source.ty.reference_type().unwrap().target.clone(),
                                false,
                            ),
                        };
                        self.emit(Op::Reference {
                            dst: payload,
                            src: source,
                            moving: false,
                        });
                        self.emit(Op::WriteMemory {
                            reference,
                            path,
                            target: value.ty.clone(),
                            value: Some(value),
                        });
                    }
                    GraphOp::Seal { dst, root, .. } => {
                        self.emit(Op::Reference {
                            dst,
                            src: root,
                            moving: false,
                        });
                    }
                },
                op => self.emit(op),
            }
        }
    }
}
