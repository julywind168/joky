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

impl FunctionPlan {
    fn when_parameters(
        &self,
        block: &When,
        vars: &HashMap<String, Binding>,
    ) -> Result<Vec<Param>, Error> {
        block
            .accesses
            .iter()
            .map(|(name, mutable)| {
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
                    name: name.clone(),
                    ty: Type::reference(target, *mutable),
                    span: block.span,
                })
            })
            .collect()
    }

    fn when_result(&self, result: &Type, types: &TypeRegistry) -> Result<Type, Error> {
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

    pub(super) fn when_expr(
        &mut self,
        block: &When,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
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
                    name: p.name.clone(),
                    ty: Some(p.ty.clone()),
                    span: p.span,
                })
                .collect(),
            ret: None,
            body: block.body.clone(),
            span: block.span,
        };
        let mut names = closures::free_names(&anonymous, vars)?;
        names.sort();
        let input = packet(
            "$when.captures".into(),
            names
                .iter()
                .map(|n| (n.clone(), slot_type(&vars[n].slot)))
                .collect(),
        );
        if !self.domain_value(&input) {
            return Err(type_error(
                &block.span,
                ErrorCode::InvalidCall,
                "when captures require a bounded value packet (64 contract members, 64 lanes, 8 aggregate levels)",
            ));
        }
        let ret = self
            .bound_hint(&block.body, &local, functions, types)
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
        let entry = {
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
        let owned_captures = input.is_owner();
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
            let value = Box::new(if owned_captures {
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
            });
            body.push(if vars[name].mutable {
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
            });
        }
        body.push(block.body.clone());
        params.push(Param {
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
                json_encode: false,
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
        let result_ty = self.when_result(&ret, types)?;
        let name = {
            let mut c = self.generics.borrow_mut();
            let adapter = c
                .templates
                .iter()
                .find(|(_, f)| {
                    matches!(
                        f.compiler_intrinsic,
                        Some(CompilerIntrinsic::DomainWhenFinish)
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
            json_encode: false,
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
