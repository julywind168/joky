use super::*;
use std::collections::HashSet;

pub(crate) fn retain_function_name(ty: Type) -> String {
    format!("@retain/{ty:?}")
}

pub(super) fn materialize_retain_functions<'a>(
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
                if method == "retain"
                    && matches!(
                        value.ty,
                        Type::MutList(_) | Type::MutMap(_) | Type::MutSet(_)
                    )
                {
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
    for (ty, node) in roots {
        if !seen.insert(ty) {
            continue;
        }
        let keep = match ty {
            Type::MutList(id) => {
                let element = types.list_type(id);
                types.intern_option_type(element);
                types.intern_function_type(vec![element], Type::Bool)
            }
            Type::MutMap(id) => {
                let info = types.map_info(id);
                let entry = types.intern_tuple_type(vec![info.key, info.value]);
                let list = types.intern_list_type(entry);
                types.intern_option_type(entry);
                types.intern_option_type(list);
                types.intern_option_type(info.value);
                types.intern_function_type(vec![info.key, info.value], Type::Bool)
            }
            Type::MutSet(id) => {
                let element = types.map_info(id).key;
                let list = types.intern_list_type(element);
                types.intern_option_type(element);
                types.intern_option_type(list);
                types.intern_function_type(vec![element], Type::Bool)
            }
            _ => unreachable!(),
        };
        let builder = RetainBody {
            node,
            ty,
            keep,
            types,
        };
        functions.push(CoreFunction {
            closure_state: None,
            foreign: None,
            id: CoreFunctionId(functions.len()),
            module: CoreModuleId(0),
            name: retain_function_name(ty),
            visibility: Visibility::Private,
            receiver: None,
            receiver_mode: crate::syntax::ReceiverMode::Borrowed,
            parameters: vec![
                CoreParameter {
                    name: "@retain/input".into(),
                    ty,
                },
                CoreParameter {
                    name: "@retain/keep".into(),
                    ty: keep,
                },
            ],
            borrowed_parameters: 1,
            parameter_ownership: vec![
                Some(CoreParameterOwnership::Borrowed),
                Some(CoreParameterOwnership::Owned),
            ],
            return_type: Type::Unit,
            declared_effects: crate::sema::EffectGroupSet::new(),
            used_effects: EffectSet::new(),
            may_suspend: false,
            body: match ty {
                Type::MutList(_) => builder.mut_list(),
                Type::MutMap(_) => builder.mut_map(),
                _ => builder.mut_set(),
            },
        });
    }
}

struct RetainBody<'a> {
    node: NodeId,
    ty: Type,
    keep: Type,
    types: &'a CheckedTypes,
}

impl RetainBody<'_> {
    fn expr(&self, ty: Type, kind: CoreExprKind) -> CoreExpr {
        CoreExpr {
            id: self.node,
            ty,
            kind,
        }
    }
    fn name(&self, name: &str, ty: Type) -> CoreExpr {
        self.expr(ty, CoreExprKind::Name(format!("@retain/{name}")))
    }
    fn integer(&self, value: u64) -> CoreExpr {
        self.expr(Type::U64, CoreExprKind::Integer(value))
    }
    fn unit(&self) -> CoreExpr {
        self.expr(Type::Unit, CoreExprKind::Unit)
    }
    fn block(&self, items: Vec<CoreExpr>) -> CoreExpr {
        self.expr(
            items.last().map_or(Type::Unit, |e| e.ty),
            CoreExprKind::Block(items),
        )
    }
    fn bind(&self, name: &str, value: CoreExpr, mutable: bool) -> CoreExpr {
        self.expr(
            Type::Unit,
            CoreExprKind::Let {
                name: format!("@retain/{name}"),
                mutable,
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
    fn not(&self, value: CoreExpr) -> CoreExpr {
        self.expr(
            Type::Bool,
            CoreExprKind::Unary {
                op: UnaryOp::Not,
                expression: Box::new(value),
            },
        )
    }
    fn increment(&self, name: &str) -> CoreExpr {
        self.assign(
            name,
            self.binary(BinaryOp::Add, self.name(name, Type::U64), self.integer(1)),
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
    fn call(&self, callee: CoreExpr, ty: Type, arguments: Vec<CoreExpr>) -> CoreExpr {
        self.expr(
            ty,
            CoreExprKind::Call {
                callee: Box::new(callee),
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
            self.expr(
                Type::Unit,
                CoreExprKind::Field {
                    value: Box::new(receiver),
                    access: FieldAccess::Name(method.into()),
                },
            ),
            ty,
            arguments,
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
    fn field(&self, value: CoreExpr, index: usize, ty: Type) -> CoreExpr {
        self.expr(
            ty,
            CoreExprKind::Field {
                value: Box::new(value),
                access: FieldAccess::Index(index),
            },
        )
    }
    fn input(&self) -> CoreExpr {
        self.name("input", self.ty)
    }
    fn keep(&self, arguments: Vec<CoreExpr>) -> CoreExpr {
        self.call(self.name("keep", self.keep), Type::Bool, arguments)
    }
    fn mut_list(&self) -> CoreExpr {
        let Type::MutList(id) = self.ty else {
            unreachable!()
        };
        let element = self.types.list_type(id);
        let option = Type::Option(self.types.option_id(element).expect("retain option"));
        let get = |index: &str| {
            self.unwrap(
                self.method(
                    self.input(),
                    "get",
                    option,
                    vec![self.name(index, Type::U64)],
                ),
                element,
            )
        };
        self.block(vec![
            self.bind(
                "n",
                self.method(self.input(), "length", Type::U64, vec![]),
                false,
            ),
            self.bind("write", self.integer(0), true),
            self.bind("read", self.integer(0), true),
            self.repeat(
                self.binary(
                    BinaryOp::Less,
                    self.name("read", Type::U64),
                    self.name("n", Type::U64),
                ),
                vec![
                    self.bind("item", get("read"), false),
                    self.choose(
                        self.keep(vec![self.name("item", element)]),
                        self.block(vec![
                            self.method(
                                self.input(),
                                "set",
                                Type::Unit,
                                vec![self.name("write", Type::U64), self.name("item", element)],
                            ),
                            self.increment("write"),
                        ]),
                        self.unit(),
                    ),
                    self.increment("read"),
                ],
            ),
            self.repeat(
                self.binary(
                    BinaryOp::Greater,
                    self.method(self.input(), "length", Type::U64, vec![]),
                    self.name("write", Type::U64),
                ),
                vec![
                    self.method(self.input(), "pop", option, vec![]),
                    self.unit(),
                ],
            ),
            self.unit(),
        ])
    }
    fn mut_map(&self) -> CoreExpr {
        let Type::MutMap(id) = self.ty else {
            unreachable!()
        };
        let info = self.types.map_info(id);
        let entry = Type::Tuple(
            self.types
                .tuple_id(&[info.key, info.value])
                .expect("retain entry"),
        );
        let list = Type::List(self.types.list_id(entry).expect("retain list"));
        let head = Type::Option(self.types.option_id(entry).expect("retain head"));
        let tail = Type::Option(self.types.option_id(list).expect("retain tail"));
        let removed = Type::Option(self.types.option_id(info.value).expect("retain remove"));
        self.snapshot_filter(
            list,
            head,
            tail,
            removed,
            |entry| {
                self.keep(vec![
                    self.field(entry.clone(), 0, info.key),
                    self.field(entry.clone(), 1, info.value),
                ])
            },
            |entry| self.field(entry, 0, info.key),
        )
    }
    fn mut_set(&self) -> CoreExpr {
        let Type::MutSet(id) = self.ty else {
            unreachable!()
        };
        let element = self.types.map_info(id).key;
        let list = Type::List(self.types.list_id(element).expect("retain list"));
        let head = Type::Option(self.types.option_id(element).expect("retain head"));
        let tail = Type::Option(self.types.option_id(list).expect("retain tail"));
        self.snapshot_filter(
            list,
            head,
            tail,
            Type::Bool,
            |item| self.keep(vec![item.clone()]),
            |item| item,
        )
    }
    fn snapshot_filter(
        &self,
        list: Type,
        head: Type,
        tail: Type,
        remove_ty: Type,
        keep: impl Fn(CoreExpr) -> CoreExpr,
        key: impl Fn(CoreExpr) -> CoreExpr,
    ) -> CoreExpr {
        let item = match head {
            Type::Option(id) => self.types.option_type(id),
            _ => unreachable!(),
        };
        self.block(vec![
            self.bind(
                "cursor",
                self.method(self.input(), "to_list", list, vec![]),
                true,
            ),
            self.repeat(
                self.not(self.method(self.name("cursor", list), "is_empty", Type::Bool, vec![])),
                vec![
                    self.bind(
                        "item",
                        self.unwrap(
                            self.method(self.name("cursor", list), "head", head, vec![]),
                            item,
                        ),
                        false,
                    ),
                    self.choose(
                        self.not(keep(self.name("item", item))),
                        self.block(vec![
                            self.method(
                                self.input(),
                                "remove",
                                remove_ty,
                                vec![key(self.name("item", item))],
                            ),
                            self.unit(),
                        ]),
                        self.unit(),
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
            self.unit(),
        ])
    }
}
