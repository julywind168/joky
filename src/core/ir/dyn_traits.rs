//! V1 semantic checks. No backend receives a dyn capability until V2 lowering.
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
    pub(super) fn require_executable(&self) -> Result<(), Error> {
        let erased = self
            .bodies
            .values()
            .flat_map(|b| &b.dyn_erasures)
            .find(|e| {
                e.source.ty.reference_type().is_some() && e.target.ty.dyn_interface().is_some()
            });
        if let Some(span) = self.dyn_span.or_else(|| erased.map(|e| e.span)) {
            return Err(execution_error(span));
        }
        Ok(())
    }
}

pub(super) fn execution_error(span: Span) -> Error {
    Error::new_with_code(
        ErrorPhase::Compile,
        ErrorCode::TypeMismatch,
        "Borrowed dyn trait execution is not implemented; dynamic dispatch requires V2",
    )
    .with_span(span)
}

/// Resolve against the declared interface only; V2 supplies the call operation.
pub(super) fn check_method(
    ty: &Type,
    name: &str,
    requested: Option<u64>,
    span: Span,
) -> Result<(), Error> {
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
        .find(|m| m.name == name)
        .ok_or_else(|| {
            type_error(
                &span,
                ErrorCode::UnknownName,
                format!("Dyn interface has no method '{name}'"),
            )
        })?;
    if method.mutable && !ty.reference_type().unwrap().mutable {
        return Err(type_error(
            &span,
            ErrorCode::InvalidCall,
            "An exclusive dyn method requires an exclusive view",
        ));
    }
    Err(execution_error(span))
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
    fn borrowed_dyn_v1_checks_erasure_before_the_execution_gate() {
        for borrow in ["&", "&mut "] {
            let source = format!("{BASE} fn use_view(value: {borrow}dyn Read<Item = Int32>) @noescape {{}} fn answer() -> Int32 {{ var counter = Counter {{ value: 42 }} let view: {borrow}dyn Read<Item = Int32> = {borrow}counter use_view(view) 42 }}");
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
            assert_eq!(
                plan.require_executable().unwrap_err().phase(),
                ErrorPhase::Compile
            );
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
            let source = format!("{BASE} fn answer() {{ var counter = Counter {{ value: 42 }} let missing = 1 let view: {binding} = {value} }}");
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
