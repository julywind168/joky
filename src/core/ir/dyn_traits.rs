//! Borrowed dynamic interfaces, concrete entries and checked indirect calls.
use super::*;

pub(super) fn check_signature(
    params: &[crate::parser::ast::Param],
    ret: &Type,
    suspends: bool,
    host: bool,
    span: Span,
) -> Result<(), Error> {
    let has_dyn = params.iter().any(|p| p.ty.contains_dyn()) || ret.contains_dyn();
    if host && has_dyn {
        return Err(Error::new_with_code(
            ErrorPhase::Abi,
            ErrorCode::AbiError,
            "Dyn views cannot cross a host ABI",
        )
        .with_span(span));
    }
    if ret.contains_dyn() {
        return Err(type_error(
            &span,
            ErrorCode::TypeMismatch,
            "Dyn views cannot be returned",
        ));
    }
    if suspends && has_dyn {
        return Err(type_error(
            &span,
            ErrorCode::TypeMismatch,
            "Dyn parameters require a synchronous function",
        ));
    }
    Ok(())
}

impl ModulePlan {
    /// Check the concrete entry signatures before any backend sees a table.
    pub(super) fn validate_dyn_tables(&self) -> Result<(), Error> {
        for body in self.bodies.values() {
            let tables: HashMap<_, _> = body
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::DynTable { dst, table } => Some((dst.index, table)),
                    _ => None,
                })
                .collect();
            for erasure in &body.dyn_erasures {
                let valid = erasure
                    .source
                    .ty
                    .reference_type()
                    .zip(erasure.target.ty.dyn_interface())
                    .is_some_and(|(source, interface)| {
                        tables
                            .get(&(erasure.target.index + 1))
                            .is_some_and(|table| {
                                table.self_type == source.target && table.interface == *interface
                            })
                    });
                if !valid {
                    return Err(type_error(
                        &erasure.span,
                        ErrorCode::InvalidState,
                        "Invalid dyn erasure descriptor",
                    ));
                }
            }
            for (op, span) in body.ops.iter().zip(&body.op_spans) {
                if let Op::DynTable { table, .. } = op {
                    let DynTable {
                        self_type,
                        methods,
                        interface,
                        ..
                    } = table.as_ref();
                    if methods.len() != interface.methods.len() {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidState,
                            "Invalid dyn table slot count",
                        ));
                    }
                    for (entry, slot) in methods.iter().zip(&interface.methods) {
                        let mut signature = slot.signature.clone();
                        signature.params[0] = Type::reference(self_type.clone(), slot.mutable);
                        let valid = self.functions.get(entry).is_some_and(|f| {
                            !f.suspends && f.needs_arena && f.signature == signature
                        });
                        if !valid {
                            return Err(type_error(
                                span,
                                ErrorCode::InvalidState,
                                "Invalid concrete dyn method signature",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl FunctionPlan {
    pub(super) fn dyn_hint(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Type> {
        let (receiver, name) = match expr {
            Expr::MethodCall(call) => {
                let ty = self.bound_hint(&call.receiver, vars, functions, types);
                if ty.as_ref().is_some_and(|t| t.dyn_interface().is_some()) {
                    (ty?, call.name.as_str())
                } else {
                    let path = expression_path(&call.receiver)?;
                    let binding = self
                        .scope
                        .traits
                        .get(&path)
                        .filter(|_| !vars.contains_key(path.split('.').next().unwrap()))?;
                    let ty = self.bound_hint(call.args.first()?, vars, functions, types)?;
                    if ty.dyn_interface()?.trait_id != binding.id {
                        return None;
                    }
                    (ty, call.name.as_str())
                }
            }
            Expr::Call {
                callee, args, span, ..
            } => {
                let (path, name) = callee.rsplit_once('.')?;
                let ty = if let Some(binding) = self
                    .scope
                    .traits
                    .get(path)
                    .filter(|_| !vars.contains_key(path.split('.').next().unwrap()))
                {
                    let ty = self.bound_hint(args.first()?, vars, functions, types)?;
                    if ty.dyn_interface()?.trait_id != binding.id {
                        return None;
                    }
                    ty
                } else {
                    self.bound_hint(
                        &self.local_callee(path, vars, *span)?,
                        vars,
                        functions,
                        types,
                    )?
                };
                (ty, name)
            }
            _ => return None,
        };
        receiver
            .dyn_interface()?
            .methods
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.signature.ret.clone())
    }

    /// Select final static instances using the same trait/impl solver as static calls.
    pub(super) fn dyn_entries(
        &self,
        target: &Type,
        interface: &Arc<crate::parser::ast::DynInterface>,
        span: Span,
    ) -> Result<Arc<DynTable>, Error> {
        let context = self.generics.borrow();
        if !context.checking {
            if let Some(table) = context
                .dyn_tables
                .iter()
                .find(|t| t.self_type == *target && t.interface == *interface)
            {
                return Ok(table.clone());
            }
        }
        drop(context);
        let mut entries = Vec::new();
        for slot in &interface.methods {
            let context = self.generics.borrow();
            let method = context
                .members
                .trait_method(
                    target,
                    &slot.name,
                    Some(interface.trait_id),
                    &self.scope,
                    span,
                )?
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::TypeMismatch,
                        "Dyn method has no applicable implementation",
                    )
                })?;
            let function = method.function.clone();
            let arguments = if method.impl_parameters == 0 {
                Vec::new()
            } else {
                match (&method.target, target) {
                    (Type::Parameter(_), _) => vec![target.clone().into()],
                    (_, Type::Record(r)) => r.arguments.clone(),
                    (_, Type::Enum(e)) => e.arguments.clone(),
                    _ => {
                        return Err(type_error(
                            &span,
                            ErrorCode::TypeMismatch,
                            "Unsupported dyn implementation target",
                        ))
                    }
                }
            };
            drop(context);
            let entry = if arguments.is_empty() {
                function
            } else {
                let mut signature = slot.signature.clone();
                signature.params[0] = Type::reference(target.clone(), slot.mutable);
                self.generics.borrow_mut().instantiate(
                    &function,
                    arguments,
                    signature,
                    self.instantiation_depth + 1,
                    span,
                )?
            };
            entries.push(entry);
        }
        let mut context = self.generics.borrow_mut();
        let table = Arc::new(DynTable {
            id: context.dyn_tables.len(),
            self_type: target.clone(),
            interface: interface.clone(),
            methods: entries,
        });
        if !context.checking {
            context.dyn_tables.push(table.clone());
        }
        Ok(table)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn dyn_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Option<Flow>, Error> {
        if let Expr::Field { base, .. } = expr {
            if self
                .bound_hint(base, vars, functions, types)
                .is_some_and(|t| t.dyn_interface().is_some())
            {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::InvalidCall,
                    "Bound dyn methods cannot be used as function values",
                ));
            }
        }
        let (receiver, name, args, requested, type_args) = match expr {
            Expr::MethodCall(call)
                if self
                    .bound_hint(&call.receiver, vars, functions, types)
                    .is_some_and(|t| t.dyn_interface().is_some()) =>
            {
                (
                    call.receiver.clone(),
                    call.name.clone(),
                    call.args.clone(),
                    None,
                    call.type_args.clone(),
                )
            }
            Expr::Call {
                callee,
                args,
                type_args,
                span,
            } => {
                let Some((path, name)) = callee.rsplit_once('.') else {
                    return Ok(None);
                };
                if let Some(binding) = self
                    .scope
                    .traits
                    .get(path)
                    .filter(|_| !vars.contains_key(path.split('.').next().unwrap()))
                {
                    let Some(receiver) = args.first().filter(|r| {
                        self.bound_hint(r, vars, functions, types)
                            .is_some_and(|t| t.dyn_interface().is_some())
                    }) else {
                        return Ok(None);
                    };
                    (
                        receiver.clone(),
                        name.to_owned(),
                        args[1..].to_vec(),
                        Some(binding.id),
                        type_args.clone(),
                    )
                } else if let Some(receiver) = self.local_callee(path, vars, *span).filter(|r| {
                    self.bound_hint(r, vars, functions, types)
                        .is_some_and(|t| t.dyn_interface().is_some())
                }) {
                    (
                        receiver,
                        name.to_owned(),
                        args.clone(),
                        None,
                        type_args.clone(),
                    )
                } else {
                    return Ok(None);
                }
            }
            Expr::MethodCall(call) => {
                let Some(path) = expression_path(&call.receiver) else {
                    return Ok(None);
                };
                let Some(binding) = self
                    .scope
                    .traits
                    .get(&path)
                    .filter(|_| !vars.contains_key(path.split('.').next().unwrap()))
                else {
                    return Ok(None);
                };
                let Some(receiver) = call.args.first().filter(|r| {
                    self.bound_hint(r, vars, functions, types)
                        .is_some_and(|t| t.dyn_interface().is_some())
                }) else {
                    return Ok(None);
                };
                (
                    receiver.clone(),
                    call.name.clone(),
                    call.args[1..].to_vec(),
                    Some(binding.id),
                    call.type_args.clone(),
                )
            }
            _ => return Ok(None),
        };
        let span = expr.span();
        if !type_args.is_empty() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Dyn methods do not accept type arguments",
            ));
        }
        let ty = self.bound_hint(&receiver, vars, functions, types).unwrap();
        let interface = ty.dyn_interface().unwrap();
        if requested.is_some_and(|id| id != interface.trait_id) {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                "Dyn view belongs to a different trait",
            ));
        }
        let method = interface
            .methods
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::UnknownName,
                    format!("Dyn interface has no method '{name}'"),
                )
            })?;
        let slot = interface.methods[method].clone();
        if slot.mutable && !ty.reference_type().unwrap().mutable {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "An exclusive dyn method requires an exclusive view",
            ));
        }
        let Flow::Value(Some(view)) = self.expr(&receiver, vars, functions, suspends, types)?
        else {
            return Ok(Some(Flow::Diverged));
        };
        // Borrow with the method's permissions. A shared method called through
        // an exclusive view must allow another shared receiver in its arguments.
        let receiver_ty =
            Type::reference(ty.reference_type().unwrap().target.clone(), slot.mutable);
        let borrowed = self.slot(receiver_ty.clone())?;
        self.emit(Op::Reference {
            dst: borrowed.clone(),
            src: view,
            moving: false,
        });
        let view = borrowed;
        let target = self.slot(Type::function(
            slot.signature.params.clone(),
            slot.signature.ret.clone(),
        ))?;
        self.emit(Op::DynMethod {
            dst: target.clone(),
            table: Slot {
                index: view.index + 1,
                ty: Type::Ptr,
            },
            method,
        });
        // An explicit checked reborrow retains every origin and ancestor while
        // adapting only the data lane to the shared method ABI.
        let data = self.slot(receiver_ty.scalar_types()[0].clone())?;
        self.emit(Op::Reference {
            dst: data.clone(),
            src: view,
            moving: false,
        });
        let Some(mut values) = self.checked_arguments(
            &name,
            &args,
            &slot.signature.params[1..],
            None,
            span,
            |i| args.get(i + 1),
            vars,
            functions,
            suspends,
            types,
        )?
        else {
            return Ok(Some(Flow::Diverged));
        };
        values.insert(0, Some(data));
        let result = if slot.signature.ret == Type::Unit {
            None
        } else {
            Some(self.slot(slot.signature.ret.clone())?)
        };
        let index = {
            let mut context = self.generics.borrow_mut();
            if let Some(i) = context.dyn_contracts.iter().position(|s| {
                s.signature == slot.signature && s.noescape_params == slot.noescape_params
            }) {
                i
            } else {
                let i = context.dyn_contracts.len();
                context.dyn_contracts.push(slot);
                i
            }
        };
        self.emit(Op::Call {
            indirect: Some(target),
            name: format!("__joky_indirect_dyn_{index}"),
            args: values,
            result: result.clone(),
            state: None,
        });
        Ok(Some(Flow::Value(result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> Result<ModulePlan, Error> {
        let types = TypeRegistry::default();
        let prepared = crate::core::prepared::PreparedModule::new(
            &crate::parser::parse(source).unwrap(),
            &types,
            &Default::default(),
        )?;
        ModulePlan::check_with_import_layouts_and_types(
            prepared.ast(),
            &HashMap::new(),
            prepared.types(),
        )
    }

    const BASE: &str = "trait Read { type Item; fn value(&self) @noescape -> Self.Item; fn touch(&self) @noescape {} } struct Counter { value: Int32 } impl Read for Counter { type Item = Int32; fn value(&self) -> Int32 { self.value } }";

    #[test]
    fn borrowed_dyn_v2_preserves_erasure_and_table_descriptors() {
        for borrow in ["&", "&mut "] {
            let source = format!("{BASE} fn use_view(value: {borrow}dyn Read<Item = Int32>) @noescape {{}} fn answer() -> Int32 {{ var counter = Counter {{ value: 42 }}; let view: {borrow}dyn Read<Item = Int32> = {borrow}counter; use_view(view); 42 }}");
            let plan = checked(&source).unwrap_or_else(|e| panic!("{e}\n{source}"));
            let erased = &plan.bodies["answer"].dyn_erasures;
            assert_eq!(erased.len(), 1);
            assert!(erased[0]
                .source
                .ty
                .reference_type()
                .unwrap()
                .target
                .nominal()
                .is_some());
            let interface = erased[0].target.ty.dyn_interface().unwrap();
            assert_eq!(interface.bindings, vec![("Item".into(), Type::Int32)]);
            assert_eq!(
                interface
                    .methods
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>(),
                ["value", "touch"]
            );
            assert_eq!(interface.methods[0].signature.ret, Type::Int32);
            assert_eq!(interface.methods[0].noescape_params, [0]);
            assert!(plan.bodies["answer"]
                .ops
                .iter()
                .any(|op| matches!(op, Op::DynTable { .. })));
        }
    }

    #[test]
    fn borrowed_dyn_v1_rejects_invalid_conversions_before_execution() {
        for (value, binding, message) in [
            ("&counter", "&mut dyn Read<Item = Int32>", "shared borrow"),
            ("counter", "&dyn Read<Item = Int32>", "explicit borrow"),
            (
                "&counter",
                "&dyn Read<Item = Bool>",
                "equality 'Item' mismatch",
            ),
            ("&missing", "&dyn Read<Item = Int32>", "no applicable impl"),
        ] {
            let source = format!("{BASE} fn answer() {{ var counter = Counter {{ value: 42 }}; let missing = 1; let view: {binding} = {value} }}");
            let error = match checked(&source) {
                Ok(_) => panic!("accepted {source}"),
                Err(e) => e,
            };
            assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{error}");
            assert!(error.message().contains(message), "{message}: {error}");
        }
    }
    #[test]
    fn borrowed_dyn_v1_canonicalizes_aliases_bindings_and_generic_substitution() {
        let source = "trait Read { type Left; type Right; fn second(&self) @noescape -> Self.Right; fn first(&self) @noescape -> Self.Left; } type View<T> = &dyn Read<Right = Bool, Left = T>; fn a(v: View<Int32>) {} fn b(v: &dyn Read<Left = Int32, Right = Bool>) {} fn generic<T>(v: View<T>) {} fn forward(v: View<Int32>) { generic(v) }";
        let plan = checked(source).unwrap_or_else(|e| panic!("{e}"));
        let a = &plan.functions["a"].signature.params[0];
        let b = &plan.functions["b"].signature.params[0];
        assert_eq!(a, b);
        assert_eq!(a, &a.scalar_types()[0]);
        let interface = a.dyn_interface().unwrap();
        assert_eq!(
            interface.bindings,
            [("Left".into(), Type::Int32), ("Right".into(), Type::Bool)]
        );
        assert_eq!(
            interface
                .methods
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert_eq!(interface.methods[0].signature.ret, Type::Bool);
        assert!(a.storage_layout().is_err());
        assert!(!crate::core::instance::persistent(a));
        let error = crate::modules::validate_host_types(
            &mut TypeRegistry::default(),
            &crate::FunctionSignature::new(vec![a.clone()], Type::Unit),
        )
        .unwrap_err();
        assert_eq!(error.phase(), ErrorPhase::Abi);
    }

    #[test]
    fn borrowed_dyn_v1_preserves_field_and_branch_origins() {
        let prefix = format!("{BASE} struct Pair {{ left: Counter, right: Counter }} fn use_view(v: &dyn Read<Item = Int32>) {{}} fn use_pair(a: &mut dyn Read<Item = Int32>, b: &dyn Read<Item = Int32>) {{}}");
        for body in [
            "fn f() { var p = Pair { left: Counter { value: 1 }, right: Counter { value: 2 } }; let v: &dyn Read<Item = Int32> = &p.left; p.right.value = 3; use_view(v) }",
            "fn f() { var c = Counter { value: 1 }; let v: &mut dyn Read<Item = Int32> = &mut c; use_view(v); use_view(v) }",
            "fn f() { var c = Counter { value: 1 }; use_view(&c); c.value = 2 }",
        ] {
            checked(&format!("{prefix} {body}")).unwrap_or_else(|e| panic!("{e}\n{body}"));
        }
        for body in [
            "fn f() { var p = Pair { left: Counter { value: 1 }, right: Counter { value: 2 } }; let v: &dyn Read<Item = Int32> = &p.left; p.left.value = 3; use_view(v) }",
            "fn f(c: Bool) { var a = Counter { value: 1 }; var b = Counter { value: 2 }; let v: &dyn Read<Item = Int32> = if c { &a } else { &b }; a.value = 3; use_view(v) }",
            "fn f(c: Bool) { var a = Counter { value: 1 }; var b = Counter { value: 2 }; let v: &dyn Read<Item = Int32> = if c { &a } else { &b }; b.value = 3; use_view(v) }",
            "fn f() { var c = Counter { value: 1 }; use_pair(&mut c, &c) }",
        ] {
            let error = checked(&format!("{prefix} {body}")).err().expect(body);
            assert!(error.message().contains("borrow"), "{error}\n{body}");
        }
    }
    #[test]
    fn borrowed_dyn_v1_imports_keep_declaration_identity_and_private_signature_types() {
        let mut modules = crate::ModuleSources::new();
        modules.insert("base", "type Number = Int32; pub trait Read { type Item; fn value(&self, n: Number) @noescape -> Self.Item; } pub type View = &dyn Read<Item = Number>; pub fn use_view(v: View) {} ").unwrap();
        modules
            .insert(
                "left",
                "import base as b; pub fn use_view(v: b.View) { b.use_view(v) }",
            )
            .unwrap();
        modules.insert("right", "import base as b.{type Read as Renamed}; pub fn use_view(v: &dyn Renamed<Item = Int32>) { b.use_view(v) }").unwrap();
        modules
            .insert(
                "other",
                "pub trait Read { type Item; fn value(&self, n: Int32) @noescape -> Self.Item; }",
            )
            .unwrap();
        let source = "import base as b.{type Read as Renamed}; import left; import right; import other; fn a(v: b.View) { left.use_view(v); right.use_view(v) } fn z(v: &dyn Renamed<Item = Int32>) {} fn different(v: &dyn other.Read<Item = Int32>) {}";
        let types = TypeRegistry::default();
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
            .unwrap_or_else(|e| panic!("{e}"));
        // VM/AOT preparation normalizes an already expanded source module again.
        let prepared =
            crate::core::prepared::PreparedModule::new(&ast, &types, &Default::default())
                .unwrap_or_else(|e| panic!("{e}"));
        let plan = ModulePlan::check_with_import_layouts_and_types(
            prepared.ast(),
            &HashMap::new(),
            prepared.types(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let a = &plan.functions["a"].signature.params[0];
        assert_eq!(a, &plan.functions["z"].signature.params[0]);
        assert_ne!(a, &plan.functions["different"].signature.params[0]);
        let slot = &a.dyn_interface().unwrap().methods[0];
        assert_eq!(slot.signature.params[1], Type::Int32);
        let file = ast.sources().get(slot.span.source_id()).unwrap();
        assert!(file.text().contains("n: Number"));
    }
    #[test]
    fn borrowed_dyn_v1_checks_generic_erasure_and_assignment() {
        let source = format!("{BASE} fn use_view(v: &dyn Read<Item = Int32>) {{}} fn generic<T: Read<Item = Int32>>(v: &T) {{ use_view(v) }} fn f() {{ let a = Counter {{ value: 1 }}; let b = Counter {{ value: 2 }}; var view: &dyn Read<Item = Int32> = &a; use_view(view); view = &b; use_view(view); generic(&a) }}");
        let plan = checked(&source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(plan.bodies["f"].dyn_erasures.len(), 2);
        let source = format!("{BASE} fn use_view(v: &dyn Read<Item = Bool>) {{}} fn generic<T: Read<Item = Int32>>(v: &T) {{ use_view(v) }}");
        let error = checked(&source).err().expect("incorrect generic equality");
        assert!(
            error.message().contains("equality 'Item' mismatch"),
            "{error}"
        );
    }
}
