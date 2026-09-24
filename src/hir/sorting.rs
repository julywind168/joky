//! Typed sort helpers reuse ordinary calls, loops, and collection ownership.
use super::*;
use std::collections::HashSet;

pub(crate) fn sort_function_name(ty: Type) -> String {
    format!("@sort/{ty:?}")
}

pub(super) fn materialize_sort_functions<'a>(
    functions: &mut Vec<CoreFunction>,
    types: &mut CheckedTypes,
    defaults: impl IntoIterator<Item = &'a CoreExpr>,
) {
    fn collect(expr: &CoreExpr, roots: &mut Vec<(Type, NodeId)>) {
        if let CoreExprKind::Call { callee, .. } = &expr.kind {
            if let CoreExprKind::Field {
                value,
                access: FieldAccess::Name(method),
            } = &callee.kind
            {
                if matches!(
                    (value.ty, method.as_str()),
                    (Type::List(_), "sorted") | (Type::MutList(_), "sort")
                ) {
                    roots.push((value.ty, expr.id));
                }
            }
        }
        super::analysis::visit_core_children(expr, &mut |child| collect(child, roots));
    }
    let mut roots = Vec::new();
    for f in functions.iter() {
        collect(&f.body, &mut roots);
    }
    for expr in defaults {
        collect(expr, &mut roots);
    }
    let mut seen = HashSet::new();
    let mut cursor = 0;
    while cursor < roots.len() {
        let (ty, node) = roots[cursor];
        cursor += 1;
        if !seen.insert(ty) {
            continue;
        }
        let (Type::List(id) | Type::MutList(id)) = ty else {
            unreachable!()
        };
        let immutable = matches!(ty, Type::List(_));
        if immutable {
            roots.push((Type::MutList(id), node));
        }
        let state = types.sort_state_type(id);
        let builder = SortBody {
            node,
            id,
            state,
            types,
        };
        functions.push(CoreFunction {
            closure_state: None,
            foreign: None,
            id: CoreFunctionId(functions.len()),
            module: CoreModuleId(0),
            name: sort_function_name(ty),
            visibility: Visibility::Private,
            receiver: None,
            receiver_mode: crate::syntax::ReceiverMode::Borrowed,
            parameters: vec![CoreParameter {
                name: "@sort/input".into(),
                ty,
            }],
            borrowed_parameters: usize::from(!immutable),
            parameter_ownership: vec![if immutable {
                None
            } else {
                Some(CoreParameterOwnership::Borrowed)
            }],
            return_type: if immutable { ty } else { Type::Unit },
            declared_effects: crate::sema::EffectGroupSet::new(),
            used_effects: EffectSet::new(),
            may_suspend: false,
            body: if immutable {
                builder.sorted()
            } else {
                builder.sort()
            },
        });
    }
}

struct SortBody<'a> {
    node: NodeId,
    id: usize,
    state: Type,
    types: &'a CheckedTypes,
}

impl SortBody<'_> {
    fn expr(&self, ty: Type, kind: CoreExprKind) -> CoreExpr {
        CoreExpr {
            id: self.node,
            ty,
            kind,
        }
    }
    fn name(&self, name: &str, ty: Type) -> CoreExpr {
        let Type::Class(state) = self.state else {
            unreachable!()
        };
        if self
            .types
            .class_fields(state)
            .iter()
            .any(|(field, _)| field == name)
        {
            return self.expr(
                ty,
                CoreExprKind::Field {
                    value: Box::new(self.name("state", self.state)),
                    access: FieldAccess::Name(name.into()),
                },
            );
        }
        self.expr(ty, CoreExprKind::Name(format!("@sort/{name}")))
    }
    fn index(&self, name: &str) -> CoreExpr {
        self.name(name, Type::U64)
    }
    fn integer(&self, value: u64) -> CoreExpr {
        self.expr(Type::U64, CoreExprKind::Integer(value))
    }
    fn unit(&self) -> CoreExpr {
        self.expr(Type::Unit, CoreExprKind::Unit)
    }
    fn list(&self, name: &str) -> CoreExpr {
        self.name(name, Type::MutList(self.id))
    }
    fn block(&self, items: Vec<CoreExpr>) -> CoreExpr {
        self.expr(
            items.last().map_or(Type::Unit, |e| e.ty),
            CoreExprKind::Block(items),
        )
    }
    fn bind(&self, name: &str, value: CoreExpr) -> CoreExpr {
        if matches!(self.name(name, value.ty).kind, CoreExprKind::Field { .. }) {
            return self.assign(name, value);
        }
        self.expr(
            Type::Unit,
            CoreExprKind::Let {
                name: format!("@sort/{name}"),
                mutable: false,
                annotation: Some(value.ty),
                value: Box::new(value),
            },
        )
    }
    fn assign(&self, name: &str, value: CoreExpr) -> CoreExpr {
        self.expr(
            Type::Unit,
            CoreExprKind::Assign {
                target: Box::new(self.name(name, value.ty)),
                value: Box::new(value),
            },
        )
    }
    fn binary(&self, op: BinaryOp, left: CoreExpr, right: CoreExpr) -> CoreExpr {
        let ty = if op.is_comparison() || matches!(op, BinaryOp::And | BinaryOp::Or) {
            Type::Bool
        } else {
            left.ty
        };
        self.expr(
            ty,
            CoreExprKind::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }
    fn increment(&self, name: &str) -> CoreExpr {
        self.assign(
            name,
            self.binary(BinaryOp::Add, self.index(name), self.integer(1)),
        )
    }
    fn choose(&self, condition: CoreExpr, yes: CoreExpr, no: CoreExpr) -> CoreExpr {
        self.expr(
            yes.ty,
            CoreExprKind::If {
                condition: Box::new(condition),
                then_branch: Box::new(yes),
                else_branch: Box::new(no),
            },
        )
    }
    fn repeat(&self, condition: CoreExpr, items: Vec<CoreExpr>) -> CoreExpr {
        self.expr(
            Type::Unit,
            CoreExprKind::While {
                condition: Box::new(condition),
                body: Box::new(self.block(items)),
            },
        )
    }
    fn call(&self, callee: CoreExprKind, ty: Type, arguments: Vec<CoreExpr>) -> CoreExpr {
        self.expr(
            ty,
            CoreExprKind::Call {
                callee: Box::new(self.expr(Type::Unit, callee)),
                type_arguments: vec![],
                effect_operation: None,
                arguments: arguments
                    .into_iter()
                    .map(|value| CoreCallArgument { label: None, value })
                    .collect(),
            },
        )
    }
    fn method(
        &self,
        receiver: CoreExpr,
        method: &str,
        ty: Type,
        arguments: Vec<CoreExpr>,
    ) -> CoreExpr {
        self.call(
            CoreExprKind::Field {
                value: Box::new(receiver),
                access: FieldAccess::Name(method.into()),
            },
            ty,
            arguments,
        )
    }
    fn empty(&self, mutable: bool) -> CoreExpr {
        self.expr(
            if mutable {
                Type::MutList(self.id)
            } else {
                Type::List(self.id)
            },
            CoreExprKind::CollectionLiteral(if mutable {
                CoreCollectionLiteral::MutList(vec![])
            } else {
                CoreCollectionLiteral::List(vec![])
            }),
        )
    }
    fn state(&self) -> CoreExpr {
        let Type::Class(id) = self.state else {
            unreachable!()
        };
        self.bind(
            "state",
            self.expr(
                self.state,
                CoreExprKind::StructInit {
                    name: self.types.class_name(id).into(),
                    fields: self
                        .types
                        .class_fields(id)
                        .iter()
                        .map(|(name, ty)| {
                            (
                                name.clone(),
                                if *ty == Type::U64 {
                                    self.integer(0)
                                } else {
                                    self.empty(false)
                                },
                            )
                        })
                        .collect(),
                },
            ),
        )
    }
    fn unwrap(&self, value: CoreExpr, ty: Type) -> CoreExpr {
        self.expr(
            ty,
            CoreExprKind::Unwrap {
                value: Box::new(value),
                propagate: false,
            },
        )
    }
    fn get(&self, list: &str, index: &str) -> CoreExpr {
        let element = self.types.list_type(self.id);
        let option = Type::Option(self.types.option_id(element).expect("sort element option"));
        self.unwrap(
            self.method(self.list(list), "get", option, vec![self.index(index)]),
            element,
        )
    }
    fn set(&self, list: &str, index: &str, value: CoreExpr) -> CoreExpr {
        self.method(
            self.list(list),
            "set",
            Type::Unit,
            vec![self.index(index), value],
        )
    }
    fn left_first(&self) -> CoreExpr {
        let left = self.get("input", "left");
        let right = self.get("input", "right");
        let order = if let Some(symbol) = self
            .types
            .interface
            .method_symbols
            .get(&(left.ty, crate::sema::ORD_METHOD.into()))
        {
            self.call(
                CoreExprKind::ExternalSymbol(symbol.clone()),
                self.types.ordering_type(),
                vec![left, right],
            )
        } else {
            self.method(
                left,
                crate::sema::ORD_METHOD,
                self.types.ordering_type(),
                vec![right],
            )
        };
        let greater = self.expr(
            self.types.ordering_type(),
            CoreExprKind::Field {
                value: Box::new(self.expr(
                    Type::Unit,
                    CoreExprKind::Name(crate::sema::ORDERING_NAME.into()),
                )),
                access: FieldAccess::Name("Greater".into()),
            },
        );
        self.expr(
            Type::Bool,
            super::lower_comparison(self.node, BinaryOp::NotEqual, order, greater, self.types),
        )
    }

    // Bottom-up stable mergesort. Bounds use subtraction before addition, and
    // width saturates at n, so even the final incomplete run cannot overflow.
    fn sort(&self) -> CoreExpr {
        let bounded_end = |start: &str| {
            let remaining = self.binary(BinaryOp::Subtract, self.index("n"), self.index(start));
            self.choose(
                self.binary(BinaryOp::Less, self.index("width"), remaining),
                self.binary(BinaryOp::Add, self.index(start), self.index("width")),
                self.index("n"),
            )
        };
        let take_left = self.binary(
            BinaryOp::And,
            self.binary(BinaryOp::Less, self.index("left"), self.index("mid")),
            self.binary(
                BinaryOp::Or,
                self.binary(
                    BinaryOp::GreaterEqual,
                    self.index("right"),
                    self.index("end"),
                ),
                self.left_first(),
            ),
        );
        self.block(vec![
            self.state(),
            self.bind(
                "n",
                self.method(self.list("input"), "length", Type::U64, vec![]),
            ),
            self.bind("scratch", self.empty(true)),
            self.bind("i", self.integer(0)),
            self.repeat(
                self.binary(BinaryOp::Less, self.index("i"), self.index("n")),
                vec![
                    self.method(
                        self.list("scratch"),
                        "push",
                        Type::Unit,
                        vec![self.get("input", "i")],
                    ),
                    self.increment("i"),
                ],
            ),
            self.bind("width", self.integer(1)),
            self.repeat(
                self.binary(BinaryOp::Less, self.index("width"), self.index("n")),
                vec![
                    self.bind("start", self.integer(0)),
                    self.repeat(
                        self.binary(BinaryOp::Less, self.index("start"), self.index("n")),
                        vec![
                            self.bind("mid", bounded_end("start")),
                            self.bind("end", bounded_end("mid")),
                            self.bind("left", self.index("start")),
                            self.bind("right", self.index("mid")),
                            self.bind("out", self.index("start")),
                            self.repeat(
                                self.binary(BinaryOp::Less, self.index("out"), self.index("end")),
                                vec![
                                    self.choose(
                                        take_left,
                                        self.block(vec![
                                            self.set("scratch", "out", self.get("input", "left")),
                                            self.increment("left"),
                                        ]),
                                        self.block(vec![
                                            self.set("scratch", "out", self.get("input", "right")),
                                            self.increment("right"),
                                        ]),
                                    ),
                                    self.increment("out"),
                                ],
                            ),
                            self.assign("start", self.index("end")),
                        ],
                    ),
                    self.assign("i", self.integer(0)),
                    self.repeat(
                        self.binary(BinaryOp::Less, self.index("i"), self.index("n")),
                        vec![
                            self.set("input", "i", self.get("scratch", "i")),
                            self.increment("i"),
                        ],
                    ),
                    self.assign(
                        "width",
                        self.choose(
                            self.binary(
                                BinaryOp::Greater,
                                self.index("width"),
                                self.binary(BinaryOp::Divide, self.index("n"), self.integer(2)),
                            ),
                            self.index("n"),
                            self.binary(BinaryOp::Multiply, self.index("width"), self.integer(2)),
                        ),
                    ),
                ],
            ),
            self.unit(),
        ])
    }

    fn sorted(&self) -> CoreExpr {
        let list = Type::List(self.id);
        let element = self.types.list_type(self.id);
        let head = Type::Option(self.types.option_id(element).expect("sort head option"));
        let tail = Type::Option(self.types.option_id(list).expect("sort tail option"));
        self.block(vec![
            self.state(),
            self.bind("values", self.empty(true)),
            self.bind("cursor", self.name("input", list)),
            self.repeat(
                self.expr(
                    Type::Bool,
                    CoreExprKind::Unary {
                        op: UnaryOp::Not,
                        expression: Box::new(self.method(
                            self.name("cursor", list),
                            "is_empty",
                            Type::Bool,
                            vec![],
                        )),
                    },
                ),
                vec![
                    self.method(
                        self.list("values"),
                        "push",
                        Type::Unit,
                        vec![self.unwrap(
                            self.method(self.name("cursor", list), "head", head, vec![]),
                            element,
                        )],
                    ),
                    self.assign(
                        "cursor",
                        self.unwrap(
                            self.method(self.name("cursor", list), "tail", tail, vec![]),
                            list,
                        ),
                    ),
                ],
            ),
            self.call(
                CoreExprKind::Name(sort_function_name(Type::MutList(self.id))),
                Type::Unit,
                vec![self.list("values")],
            ),
            self.bind("result", self.empty(false)),
            self.bind(
                "i",
                self.method(self.list("values"), "length", Type::U64, vec![]),
            ),
            self.repeat(
                self.binary(BinaryOp::Greater, self.index("i"), self.integer(0)),
                vec![
                    self.assign(
                        "i",
                        self.binary(BinaryOp::Subtract, self.index("i"), self.integer(1)),
                    ),
                    self.assign(
                        "result",
                        self.method(
                            self.name("result", list),
                            "push_front",
                            list,
                            vec![self.get("values", "i")],
                        ),
                    ),
                ],
            ),
            self.name("result", list),
        ])
    }
}
