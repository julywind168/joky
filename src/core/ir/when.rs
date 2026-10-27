//! Inline domain operations share the checked value bridge and FIFO admission.
use super::*;
use crate::core::types::{RecordField, RecordType};
use crate::parser::ast::{AnonymousFunction, ClosureParameter, Param, When};

fn packet(name: String, fields: Vec<(String, Type)>) -> Type {
    let mut scalars = Vec::new();
    let mut depth = 1;
    let mut owners = Vec::new();
    let fields = fields
        .into_iter()
        .map(|(name, ty)| {
            let offset = scalars.len();
            if ty.is_owner() {
                owners.extend_from_slice(ty.buffer_elements());
            }
            scalars.extend_from_slice(ty.scalar_types());
            depth = depth.max(ty.layout_depth() + 1);
            RecordField {
                name,
                ty,
                offset,
                private: false,
            }
        })
        .collect();
    Type::Record(Arc::new(RecordType {
        array: false,
        list: false,
        map: false,
        set: false,
        id: crate::core::types::fresh_type_id(),
        name,
        fields,
        scalars,
        depth,
        closure: None,
        owners,
        repr_c: false,
        indirect: false,
        parameters: vec![],
        arguments: vec![],
        template: None,
    }))
}

struct PreparedWhen {
    entry: String,
    input: Type,
    ret: Type,
    names: Vec<String>,
}

impl FunctionPlan {
    fn when_path_packet(
        &mut self,
        block: &When,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<(Type, Slot), Error> {
        fn projection(expr: &Expr) -> Option<(String, Vec<String>)> {
            match expr {
                Expr::Ident { name, .. } => Some((name.clone(), vec![])),
                Expr::Field {
                    base,
                    name,
                    type_args,
                    ..
                } if type_args.is_empty() => {
                    let (root, mut fields) = projection(base)?;
                    fields.push(name.clone());
                    Some((root, fields))
                }
                _ => None,
            }
        }
        let fail = || {
            type_error(&block.span, ErrorCode::InvalidCall,
            "when paths require DirectoryHandle projections and UInt64/String keys (Option only on optional paths); dependent keys must project an earlier state")
        };
        let literal = |value| Expr::Literal {
            literal: Literal::UInt64(value),
            span: block.span,
        };
        let saved_scope = self.scope.clone();
        let params = self.when_parameters(block, vars)?;
        let mut elements = vec![literal(u64::from(block.alias.is_some()))];
        let mut temporary = Vec::new();
        let mut keys = Vec::new();
        for (i, (name, _)) in block.accesses.iter().enumerate() {
            let mut descriptor = vec![literal(0); 6];
            if let Some(path) = block.paths.get(i).and_then(Option::as_ref) {
                let (root, _) = projection(&path.directory).ok_or_else(fail)?;
                if block.accesses[..i].iter().any(|(n, _)| n == &root) {
                    return Err(fail());
                }
                let Flow::Value(Some(directory)) =
                    self.expr(&path.directory, vars, functions, suspends, types)?
                else {
                    return Err(fail());
                };
                if !matches!(&directory.ty, Type::Record(r) if r.name == "joky/domain.DirectoryHandle")
                {
                    return Err(fail());
                }
                let temp = format!("$when.directory.{i}");
                vars.insert(
                    temp.clone(),
                    payload_binding(&Type::UInt64, directory.index),
                );
                temporary.push(temp.clone());
                descriptor[0] = literal(1);
                descriptor[3] = Expr::Ident {
                    name: temp,
                    span: block.span,
                };
                descriptor[5] = literal(u64::from(path.optional));
                if let Some((root, fields)) = projection(&path.key) {
                    if let Some(dependency) =
                        block.accesses[..i].iter().position(|(n, _)| n == &root)
                    {
                        let ty = &params[dependency].ty.reference_type().unwrap().target;
                        let mut target = ty.clone();
                        for field in &fields {
                            if let Type::Record(record) = &target {
                                self.check_field(record, field, &block.span)?;
                            }
                            target = target.inline_field(field).ok_or_else(fail)?.1.clone();
                        }
                        let (offset, ty) = ty.storage_field_path(&fields).map_err(|_| fail())?;
                        if block
                            .paths
                            .get(dependency)
                            .and_then(Option::as_ref)
                            .is_some_and(|p| p.create.is_some())
                        {
                            return Err(type_error(
                                &block.span,
                                ErrorCode::InvalidCall,
                                "A path cannot depend on a binding with a creation fallback",
                            ));
                        }
                        let optional = crate::core::state_domain::optional_key(&ty);
                        if optional.is_some() && (!path.optional || path.create.is_some()) {
                            return Err(type_error(
                                &block.span,
                                ErrorCode::InvalidCall,
                                "Option keys require an optional path without get_or_stage",
                            ));
                        }
                        let key = optional.unwrap_or(&ty);
                        let kind = match key {
                            Type::String => 2,
                            Type::UInt64 => 1,
                            _ => return Err(fail()),
                        };
                        descriptor[0] = literal(kind | if optional.is_some() { 8 } else { 0 });
                        descriptor[1] = literal(dependency as u64 + 1);
                        descriptor[2] = literal(offset as u64);
                    } else {
                        descriptor[4] = path.key.clone();
                    }
                } else if matches!(path.key, Expr::Literal { .. }) {
                    descriptor[4] = path.key.clone();
                } else {
                    return Err(fail());
                }
                if matches!(
                    &descriptor[1],
                    Expr::Literal {
                        literal: Literal::UInt64(0),
                        ..
                    }
                ) {
                    let key_ty = self
                        .bound_hint(&path.key, vars, functions, types)
                        .ok_or_else(fail)?;
                    let borrowed = key_ty.reference_type().is_some();
                    let key_ty = key_ty.reference_type().map_or(&key_ty, |r| &r.target);
                    if let Some(key) = crate::core::state_domain::optional_key(key_ty) {
                        if !path.optional || path.create.is_some() {
                            return Err(type_error(
                                &block.span,
                                ErrorCode::InvalidCall,
                                "Option keys require an optional path without get_or_stage",
                            ));
                        }
                        let Type::Enum(enumeration) = key_ty else {
                            unreachable!()
                        };
                        // Compiler-authored patterns must not resolve user constructors
                        // named Some/None, or depend on how Option was imported.
                        for variant in ["Some", "None"] {
                            Arc::make_mut(&mut self.scope).constructors.insert(
                                format!("$when.option.{variant}"),
                                vec![crate::core::imports::ConstructorBinding {
                                    enumeration: crate::core::imports::TypeBinding {
                                        name: enumeration.name.clone(),
                                        declaration: Some(enumeration.id),
                                        resolved: None,
                                    },
                                    variant: variant.into(),
                                }],
                            );
                        }
                        let span = block.span;
                        let binding = format!("$when.key.{i}");
                        let select = |some: Expr, none: Expr| Expr::Match {
                            conditional: false,
                            iteration: false,
                            value: Box::new(if key_ty.is_owner() && !borrowed {
                                Expr::Unary {
                                    op: UnaryOp::Borrow,
                                    operand: Box::new(path.key.clone()),
                                    span,
                                }
                            } else {
                                path.key.clone()
                            }),
                            arms: vec![
                                crate::parser::ast::MatchArm {
                                    pattern: crate::parser::ast::Pattern::Variant {
                                        name: "$when.option.Some".into(),
                                        binding: Some(Box::new(
                                            crate::parser::ast::PayloadPattern::Name {
                                                name: binding.clone(),
                                                span,
                                            },
                                        )),
                                        span,
                                    },
                                    body: some,
                                    span,
                                },
                                crate::parser::ast::MatchArm {
                                    pattern: crate::parser::ast::Pattern::Variant {
                                        name: "$when.option.None".into(),
                                        binding: None,
                                        span,
                                    },
                                    body: none,
                                    span,
                                },
                            ],
                            span,
                        };
                        let value = Expr::Ident {
                            name: binding.clone(),
                            span,
                        };
                        let kind = if *key == Type::String { 2 } else { 1 };
                        descriptor[0] = select(literal(kind | 8), literal(kind | 8 | 16));
                        if *key == Type::String {
                            descriptor[4] = literal(keys.len() as u64);
                            keys.push(select(
                                Expr::MethodCall(Box::new(crate::parser::ast::MethodCall {
                                    receiver: value,
                                    name: "clone".into(),
                                    type_args: vec![],
                                    args: vec![],
                                    span,
                                })),
                                Expr::Literal {
                                    literal: Literal::String(String::new()),
                                    span,
                                },
                            ));
                        } else {
                            descriptor[4] = select(value, literal(0));
                        }
                    } else if *key_ty == Type::String {
                        descriptor[0] = literal(2);
                        descriptor[4] = literal(keys.len() as u64);
                        keys.push(Expr::MethodCall(Box::new(crate::parser::ast::MethodCall {
                            receiver: path.key.clone(),
                            name: "clone".into(),
                            type_args: vec![],
                            args: vec![],
                            span: block.span,
                        })));
                    } else if *key_ty != Type::UInt64
                        && !matches!(
                            path.key,
                            Expr::Literal {
                                literal: Literal::Integer { .. },
                                ..
                            }
                        )
                    {
                        return Err(fail());
                    }
                }
                if path.create.is_some() {
                    let Expr::Literal {
                        literal: Literal::UInt64(kind),
                        ..
                    } = &mut descriptor[0]
                    else {
                        unreachable!()
                    };
                    *kind |= 4;
                }
            } else {
                let value = vars
                    .get(name)
                    .and_then(|v| v.slot.as_ref())
                    .ok_or_else(fail)?;
                let token = self.slot(Type::UInt64)?;
                self.emit(Op::DomainBits {
                    dst: token.clone(),
                    src: Slot {
                        index: value.index,
                        ty: crate::runtime::domain::handle_type(),
                    },
                    decode: false,
                });
                let temp = format!("$when.handle.{i}");
                vars.insert(temp.clone(), payload_binding(&Type::UInt64, token.index));
                temporary.push(temp.clone());
                descriptor[4] = Expr::Ident {
                    name: temp,
                    span: block.span,
                };
            }
            elements.extend(descriptor);
        }
        let arrays = [
            self.dynamic_array_type(Type::UInt64, types, block.span)?,
            self.dynamic_array_type(Type::String, types, block.span)?,
        ];
        let ty = packet(
            "$when.paths".into(),
            vec![
                ("steps".into(), arrays[0].clone()),
                ("keys".into(), arrays[1].clone()),
            ],
        );
        let slot = self.slot(ty.clone())?;
        for ((name, elements), array) in [("steps", elements), ("keys", keys)]
            .into_iter()
            .zip(arrays)
        {
            let Flow::Value(Some(value)) = self.expr_expected(
                &Expr::Array {
                    elements,
                    span: block.span,
                },
                vars,
                functions,
                suspends,
                types,
                Some(&array),
            )?
            else {
                return Err(fail());
            };
            self.emit(Op::Copy(slot.field_path(&[name.into()]), value));
        }
        self.register_owner(slot.clone(), None);
        for name in temporary {
            vars.remove(&name);
        }
        self.scope = saved_scope;
        Ok((ty, slot))
    }

    fn when_parameters(
        &self,
        block: &When,
        vars: &HashMap<String, Binding>,
    ) -> Result<Vec<Param>, Error> {
        block
            .accesses
            .iter()
            .enumerate()
            .map(|(index, (name, mutable))| {
                if let Some(Some(path)) = block.paths.get(index) {
                    return Ok(Param {
                        mutable: false,
                        name: name.clone(),
                        ty: Type::reference(path.ty.clone(), *mutable),
                        span: block.span,
                    });
                }
                let ty = vars.get(name).map(|b| slot_type(&b.slot));
                let target = match ty {
                    Some(Type::Record(r))
                        if r.name == "joky/domain.WriteHandle"
                            || (!mutable && r.name == "joky/domain.ReadHandle") =>
                    {
                        r.arguments.first().and_then(|a| a.as_type()).cloned()
                    }
                    _ => None,
                }
                .ok_or_else(|| {
                    type_error(
                        &block.span,
                        ErrorCode::InvalidCall,
                        format!(
                            "when binding `{name}` requires a {}Handle",
                            if *mutable {
                                "Write"
                            } else {
                                "ReadHandle or Write"
                            }
                        ),
                    )
                })?;
                Ok(Param {
                    mutable: false,
                    name: name.clone(),
                    ty: Type::reference(target, *mutable),
                    span: block.span,
                })
            })
            .collect()
    }

    fn when_result(&self, result: &Type, types: &TypeRegistry) -> Result<Type, Error> {
        if let Type::Enum(e) = result {
            if types.standard_success_variant(e.id) == Some("Ok") {
                let context = self.generics.borrow();
                let helper = context
                    .templates
                    .get("joky/domain.__when_outcome")
                    .ok_or_else(|| {
                        type_error(
                            &self.current_span,
                            ErrorCode::InvalidCall,
                            "when requires joky/domain",
                        )
                    })?;
                let substitutions = helper
                    .type_params
                    .iter()
                    .zip(&e.arguments)
                    .map(|(p, a)| (p.id, a.as_type().unwrap().clone()))
                    .collect();
                return crate::core::types::substitute(&helper.ret_ty, &substitutions, types);
            }
        }
        self.when_access_result(result, types)
    }

    fn when_access_result(&self, result: &Type, types: &TypeRegistry) -> Result<Type, Error> {
        let context = self.generics.borrow();
        let adapter = context
            .templates
            .values()
            .find(|f| {
                matches!(
                    f.compiler_intrinsic,
                    Some(CompilerIntrinsic::DomainWhenFinish)
                )
            })
            .ok_or_else(|| {
                type_error(
                    &self.current_span,
                    ErrorCode::InvalidCall,
                    "when requires joky/domain",
                )
            })?;
        let substitutions = [(adapter.type_params[2].id, result.clone())]
            .into_iter()
            .collect();
        crate::core::types::substitute(&adapter.ret_ty, &substitutions, types)
    }

    pub(super) fn when_hint(
        &self,
        block: &When,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Type> {
        let mut local = vars.clone();
        for p in self.when_parameters(block, vars).ok()? {
            local.insert(p.name, payload_binding(&p.ty, 0));
        }
        let ret = self.bound_hint(&block.body, &local, functions, types)?;
        self.when_result(&ret, types).ok()
    }

    fn prepare_when(
        &mut self,
        block: &When,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        context: (Option<&Type>, Option<(&PreparedWhen, &str)>),
    ) -> Result<PreparedWhen, Error> {
        let (expected, shared) = context;
        if !suspends
            || self.generics.borrow().checking
            || !self
                .generics
                .borrow()
                .definitions
                .contains(&self.when_function)
        {
            return Err(type_error(
                &block.span,
                ErrorCode::InvalidCall,
                "when requires a named, nongeneric @suspends function",
            ));
        }
        let mut pending = vec![&block.body];
        while let Some(expr) = pending.pop() {
            if matches!(
                expr,
                Expr::Return { .. } | Expr::When(_) | Expr::AnonymousFunction(_)
            ) {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::InvalidCall,
                    "when cannot contain return, nested when or closures",
                ));
            }
            pending.extend(closures::children(expr));
        }
        let mut params = self.when_parameters(block, vars)?;
        let mut local = vars.clone();
        for p in &params {
            local.insert(p.name.clone(), payload_binding(&p.ty, 0));
        }
        let anonymous = AnonymousFunction {
            moving: false,
            params: params
                .iter()
                .map(|p| ClosureParameter {
                    mutable: false,
                    name: p.name.clone(),
                    ty: Some(p.ty.clone()),
                    span: p.span,
                })
                .collect(),
            ret: None,
            body: Expr::Block {
                exprs: std::iter::once(block.body.clone())
                    .chain(block.missing.clone())
                    .chain(block.alias.clone())
                    .chain(
                        block
                            .paths
                            .iter()
                            .flatten()
                            .filter_map(|p| p.create.clone()),
                    )
                    .collect(),
                span: block.span,
            },
            span: block.span,
        };
        let mut names = closures::free_names(&anonymous, vars)?;
        names.sort();
        let mut input = packet(
            "$when.captures".into(),
            names
                .iter()
                .map(|n| (n.clone(), slot_type(&vars[n].slot)))
                .collect(),
        );
        if let Some((shared, _)) = shared {
            names = shared.names.clone();
            input = shared.input.clone();
        }
        let dynamic = shared.is_some() || block.paths.iter().any(Option::is_some);
        if dynamic {
            for name in &names {
                let ty = slot_type(&vars[name].slot);
                if ty.is_owner() {
                    local.insert(
                        name.clone(),
                        payload_binding(&Type::reference(ty, false), 0),
                    );
                }
            }
        }
        if !self.domain_value(&input) {
            return Err(type_error(
                &block.span,
                ErrorCode::InvalidCall,
                "when captures require a bounded value packet (64 contract members, 64 lanes, 8 aggregate levels)",
            ));
        }
        let expected_body = expected
            .and_then(|ty| match ty {
                Type::Enum(e) if e.name == "joky/result.Result" => {
                    e.arguments.first().and_then(|a| a.as_type()).cloned()
                }
                Type::Enum(e) if types.standard_success_variant(e.id) == Some("Committed") => {
                    let context = self.generics.borrow();
                    let helper = context.templates.get("joky/domain.__when_outcome")?;
                    let Type::Enum(access) = &helper.params[0].ty else {
                        return None;
                    };
                    let body = access.arguments.first()?.as_type()?;
                    let substitutions = helper
                        .type_params
                        .iter()
                        .zip(&e.arguments)
                        .map(|(p, a)| Some((p.id, a.as_type()?.clone())))
                        .collect::<Option<_>>()?;
                    crate::core::types::substitute(body, &substitutions, types).ok()
                }
                _ => None,
            })
            .filter(|ty| self.when_result(ty, types).ok().as_ref() == expected);
        let ret = shared
            .map(|(s, _)| s.ret.clone())
            .or(expected_body)
            .or_else(|| self.bound_hint(&block.body, &local, functions, types))
            .ok_or_else(|| {
                type_error(
                    &block.span,
                    ErrorCode::TypeMismatch,
                    "Cannot infer when result type",
                )
            })?;
        if !self.domain_value(&ret) {
            return Err(type_error(
                &block.span,
                ErrorCode::InvalidCall,
                "when result requires a bounded value (64 contract members, 64 lanes, 8 aggregate levels)",
            ));
        }
        let entry = if let Some((shared, suffix)) = shared {
            format!("{}/{suffix}", shared.entry)
        } else {
            let mut c = self.generics.borrow_mut();
            if let Some((_, _, entry)) = c
                .when_sites
                .iter()
                .find(|(parent, site, _)| parent == &self.when_function && *site == block.span)
            {
                entry.clone()
            } else {
                c.check_generation(1, self.instantiation_depth + 1, block.span)?;
                let module = block
                    .method
                    .as_ref()
                    .and_then(|method| c.when_modules.get(method))
                    .or_else(|| {
                        self.definition_scope
                            .as_ref()
                            .map(|scope| &scope.module_name)
                    })
                    .unwrap_or(&c.module_name);
                let entry = if let Some(method) = &block.method {
                    format!(
                        "$when/method/{}:{module}/{}:{method}",
                        module.len(),
                        method.len()
                    )
                } else if let Some(label) = &block.label {
                    format!(
                        "$when/named/{}:{module}/{}:{}/{}:{}",
                        module.len(),
                        self.when_function.len(),
                        self.when_function,
                        label.len(),
                        label
                            .as_bytes()
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    )
                } else {
                    format!("$when/anon/{}", c.when_sites.len())
                };
                if block.method.is_none() && c.when_sites.iter().any(|(_, _, key)| key == &entry) {
                    return Err(type_error(
                        &block.span,
                        ErrorCode::InvalidCall,
                        "Duplicate when label in this function",
                    ));
                }
                c.when_sites
                    .push((self.when_function.clone(), block.span, entry.clone()));
                entry
            }
        };
        let mut body = Vec::new();
        let owned_captures = input.is_owner() && !dynamic;
        if owned_captures {
            use crate::parser::ast::{BindingPattern, RecordFieldPattern, RecordPattern};
            body.push(Expr::Destructure {
                pattern: BindingPattern::Record {
                    inner: Box::new(RecordPattern {
                        name: "$when_captures".into(),
                        type_args: vec![],
                        rest: false,
                        fields: names
                            .iter()
                            .map(|name| RecordFieldPattern {
                                name: name.clone(),
                                pattern: BindingPattern::Name {
                                    name: format!("$capture.{name}"),
                                    span: block.span,
                                },
                                span: block.span,
                            })
                            .collect(),
                    }),
                    span: block.span,
                },
                mutable: false,
                annotation: None,
                value: Box::new(Expr::Ident {
                    name: "$captures".into(),
                    span: block.span,
                }),
                span: block.span,
            });
        }
        for name in &names {
            let value = if owned_captures {
                Expr::Ident {
                    name: format!("$capture.{name}"),
                    span: block.span,
                }
            } else {
                Expr::Field {
                    base: Box::new(Expr::Ident {
                        name: "$captures".into(),
                        span: block.span,
                    }),
                    name: name.clone(),
                    type_args: vec![],
                    span: block.span,
                }
            };
            let value = Box::new(if dynamic && slot_type(&vars[name].slot).is_owner() {
                Expr::Unary {
                    op: UnaryOp::Borrow,
                    operand: Box::new(value),
                    span: block.span,
                }
            } else {
                value
            });
            body.push(
                if vars[name].mutable && (!dynamic || !slot_type(&vars[name].slot).is_owner()) {
                    Expr::Var {
                        name: name.clone(),
                        annotation: None,
                        value,
                        span: block.span,
                    }
                } else {
                    Expr::Let {
                        name: name.clone(),
                        annotation: None,
                        value,
                        span: block.span,
                    }
                },
            );
        }
        body.push(block.body.clone());
        params.push(Param {
            mutable: false,
            name: "$captures".into(),
            ty: input.clone(),
            span: block.span,
        });
        if !self
            .generics
            .borrow()
            .generated_functions
            .contains_key(&entry)
        {
            let mut info = FunctionInfo::indirect(
                crate::FunctionSignature::new(
                    params.iter().map(|p| p.ty.clone()).collect(),
                    ret.clone(),
                ),
                false,
                block.span,
            );
            info.noescape = true;
            info.infer_noescape = true;
            info.returns_interior = false;
            let mut definition = FnDef {
                every: None,
                derived_body: None,
                noescape: Some(Box::new(crate::parser::ast::NoEscape {
                    all: true,
                    params: vec![],
                })),
                public: false,
                buffer_intrinsic: None,
                compiler_intrinsic: None,
                buffer_element: None,
                scope: self.definition_scope.clone(),
                name: entry.clone(),
                type_params: vec![],
                params,
                ret_ty: ret.clone(),
                suspends: false,
                body: Expr::Block {
                    exprs: body,
                    span: block.span,
                },
                span: block.span,
            };
            self.generics
                .borrow_mut()
                .generated_functions
                .insert(entry.clone(), info.clone());
            let mut nominals = (*self.nominals).clone();
            nominals.insert("$when_captures".into(), input.clone());
            if let Some(scope) = definition.scope.as_mut() {
                Arc::make_mut(scope).nominals = Arc::new(nominals.clone());
            }
            let plan = FunctionPlan::new(
                &definition,
                functions,
                types,
                &Arc::new(nominals),
                self.generics.clone(),
                self.instantiation_depth + 1,
            )?;
            self.generics
                .borrow_mut()
                .generated_bodies
                .push(NestedTask {
                    name: entry.clone(),
                    info,
                    plan,
                });
        }
        self.domain_calls.push((entry.clone(), block.span));
        Ok(PreparedWhen {
            entry,
            input,
            ret,
            names,
        })
    }

    pub(super) fn when_expr(
        &mut self,
        block: &When,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let prepared =
            self.prepare_when(block, vars, functions, suspends, types, (expected, None))?;
        let dynamic = block.paths.iter().any(Option::is_some);
        if dynamic {
            for (suffix, branch) in [("missing", &block.missing), ("alias", &block.alias)] {
                if let Some(body) = branch {
                    let mut alternate = block.clone();
                    alternate.accesses.pop();
                    alternate.paths.pop();
                    alternate.body = body.clone();
                    alternate.missing = None;
                    alternate.alias = None;
                    // Preserve borrowed capture rules even when the only path was last.
                    self.prepare_when(
                        &alternate,
                        vars,
                        functions,
                        suspends,
                        types,
                        (expected, Some((&prepared, suffix))),
                    )?;
                }
            }
        }
        for (index, path) in block.paths.iter().enumerate() {
            if let Some(path) = path.as_ref().filter(|p| p.create.is_some()) {
                let mut constructor = block.clone();
                constructor.accesses.clear();
                constructor.paths.clear();
                constructor.missing = None;
                constructor.alias = None;
                constructor.body = path.create.clone().unwrap();
                let contract = PreparedWhen {
                    entry: prepared.entry.clone(),
                    input: prepared.input.clone(),
                    ret: path.ty.clone(),
                    names: prepared.names.clone(),
                };
                self.prepare_when(
                    &constructor,
                    vars,
                    functions,
                    suspends,
                    types,
                    (None, Some((&contract, &format!("create/{index}")))),
                )?;
            }
        }
        let PreparedWhen {
            entry,
            input,
            ret,
            names,
        } = prepared;
        let (handles, handle_slot) = if dynamic {
            self.when_path_packet(block, vars, functions, suspends, types)?
        } else {
            let handles = packet(
                "$when.handles".into(),
                block
                    .accesses
                    .iter()
                    .map(|(n, _)| (n.clone(), Type::UInt64))
                    .collect(),
            );
            let handle_slot = self.slot(handles.clone())?;
            for (i, (name, _)) in block.accesses.iter().enumerate() {
                let Flow::Value(Some(value)) = self.expr(
                    &Expr::Ident {
                        name: name.clone(),
                        span: block.span,
                    },
                    vars,
                    functions,
                    suspends,
                    types,
                )?
                else {
                    unreachable!()
                };
                let token = Slot {
                    index: value.index,
                    ty: crate::runtime::domain::handle_type(),
                };
                self.emit(Op::DomainBits {
                    dst: Slot {
                        index: handle_slot.index + i,
                        ty: Type::UInt64,
                    },
                    src: token,
                    decode: false,
                });
            }
            (handles, handle_slot)
        };
        let input_slot = self.slot(input.clone())?;
        for name in &names {
            let Flow::Value(value) = self.expr(
                &Expr::Ident {
                    name: name.clone(),
                    span: block.span,
                },
                vars,
                functions,
                suspends,
                types,
            )?
            else {
                unreachable!()
            };
            if let Some(value) = value {
                self.emit(Op::Copy(
                    input_slot.field_path(std::slice::from_ref(name)),
                    value,
                ));
            }
        }
        if input_slot.ty.is_owner() {
            self.register_owner(input_slot.clone(), None);
        }
        let module = self.slot(crate::runtime::domain::module_type())?;
        self.emit(Op::DomainModule(module.clone()));
        let Flow::Value(text) = self.expr(
            &Expr::Literal {
                literal: Literal::String(entry),
                span: block.span,
            },
            vars,
            functions,
            suspends,
            types,
        )?
        else {
            unreachable!()
        };
        let result_ty = self.when_access_result(&ret, types)?;
        let outcome_args = match &ret {
            Type::Enum(e) if types.standard_success_variant(e.id) == Some("Ok") => {
                Some(e.arguments.clone())
            }
            _ => None,
        };
        let name = {
            let mut c = self.generics.borrow_mut();
            let adapter = c
                .templates
                .iter()
                .find(|(_, f)| {
                    matches!(
                        (dynamic, &f.compiler_intrinsic),
                        (true, Some(CompilerIntrinsic::DomainPathsFinish))
                            | (false, Some(CompilerIntrinsic::DomainWhenFinish))
                    )
                })
                .map(|(n, _)| n.clone())
                .unwrap();
            c.instantiate(
                &adapter,
                vec![handles.clone().into(), input.clone().into(), ret.into()],
                crate::FunctionSignature::new(
                    vec![module.ty.clone(), Type::String, handles, input],
                    result_ty.clone(),
                ),
                self.instantiation_depth + 1,
                block.span,
            )?
        };
        let mut args = vec![Some(module), text];
        extend_argument(&mut args, Some(handle_slot));
        extend_argument(&mut args, Some(input_slot));
        let result = self.slot(result_ty)?;
        let state = self.state_count;
        self.state_count += 1;
        self.emit(Op::Call {
            indirect: None,
            name,
            args,
            result: Some(result.clone()),
            state: Some(state),
        });
        if result.ty.is_owner() {
            self.register_owner(result.clone(), None);
        }
        if let Some(type_args) = outcome_args {
            let mut local = vars.clone();
            local.insert(
                "$when.result".into(),
                Binding {
                    slot: Some(result),
                    mutable: false,
                    place: None,
                },
            );
            let helper = "joky/domain.__when_outcome";
            let scope = self.scope.clone();
            Arc::make_mut(&mut self.scope)
                .functions
                .insert(helper.into(), helper.into());
            let flow = self.expr(
                &Expr::Call {
                    callee: helper.into(),
                    type_args,
                    args: vec![Expr::Ident {
                        name: "$when.result".into(),
                        span: block.span,
                    }],
                    span: block.span,
                },
                &mut local,
                functions,
                suspends,
                types,
            );
            self.scope = scope;
            return flow;
        }
        Ok(Flow::Value(Some(result)))
    }
}

pub(super) fn validate_states(
    module: &AstModule,
    functions: &HashMap<String, FunctionInfo>,
    types: &TypeRegistry,
    nominals: &Arc<HashMap<String, Type>>,
    generics: std::rc::Rc<std::cell::RefCell<generics::GenericContext>>,
) -> Result<(), Error> {
    for record in &module.records {
        if !record.attributes.iter().any(|a| a.path == "state") {
            continue;
        }
        let f = FnDef {
            every: None,
            derived_body: None,
            noescape: None,
            public: false,
            buffer_intrinsic: None,
            compiler_intrinsic: None,
            buffer_element: None,
            scope: None,
            name: "$state".into(),
            type_params: vec![],
            params: vec![],
            ret_ty: Type::Unit,
            suspends: false,
            body: Expr::Block {
                exprs: vec![],
                span: record.span,
            },
            span: record.span,
        };
        let plan = FunctionPlan::new(&f, functions, types, nominals, generics.clone(), 0)?;
        if !plan.domain_storage(&nominals[&record.name]) {
            return Err(type_error(&record.span, ErrorCode::InvalidCall, "#state requires a hook-free owning state record without references, resources or function values"));
        }
    }
    Ok(())
}

pub(super) fn validate_blocks(f: &FnDef) -> Result<(), Error> {
    let mut labels = std::collections::HashSet::new();
    let mut pending = vec![(&f.body, false)];
    while let Some((expr, nested)) = pending.pop() {
        if let Expr::When(block) = expr {
            if nested || !f.suspends || !f.type_params.is_empty() {
                return Err(type_error(
                    &block.span,
                    ErrorCode::InvalidCall,
                    "when requires a named, nongeneric @suspends function and cannot be nested",
                ));
            }
            if let Some(label) = &block.label {
                if !labels.insert(label) {
                    return Err(type_error(
                        &block.span,
                        ErrorCode::InvalidCall,
                        "Duplicate when label in this function",
                    ));
                }
            }
        }
        let nested = nested || matches!(expr, Expr::When(_) | Expr::AnonymousFunction(_));
        pending.extend(
            closures::children(expr)
                .into_iter()
                .map(|child| (child, nested)),
        );
    }
    Ok(())
}
