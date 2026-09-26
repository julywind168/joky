use super::super::*;

#[test]
fn parser_preserves_local_binding_mutability() {
    let program =
        parse_program("fn main() { let count = 1; var count: Int32 = count; count = count + 1 }")
            .unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected block");
    };
    assert!(matches!(
        &expressions[0].kind,
        ExprKind::Let { name, mutable: false, annotation: None, .. } if name == "count"
    ));
    assert!(matches!(
        &expressions[1].kind,
        ExprKind::Let { name, mutable: true, annotation: Some(annotation), .. }
            if name == "count" && annotation.to_string() == "Int32"
    ));
    assert!(matches!(&expressions[2].kind, ExprKind::Assign { .. }));
    let bytes = bincode::serialize(&program).unwrap();
    let restored: Program = bincode::deserialize(&bytes).unwrap();
    assert_eq!(restored, program);
}

#[test]
fn parser_rejects_var_outside_local_bindings_and_class_fields() {
    for source in [
        "var count = 1; fn main() {}",
        "pub var count = 1; fn main() {}",
        "fn main(var count: Int32) {}",
        "struct Counter { var count: Int32 } fn main() {}",
        "fn main() { var count: Int32 }",
        "fn main() { var count }",
    ] {
        assert!(parse_program(source).is_err(), "{source}");
    }
    parse_program("class Counter { var count: Int32 } fn main() {}").unwrap();
}

#[test]
fn parser_preserves_intrinsic_struct_and_class_kinds() {
    let source = "@intrinsic pub struct List(T: type) { fn sorted(&self) -> List(T) where T: Ord } @intrinsic pub class MutList(T: type) { fn sort(&self) -> Unit where T: Ord }";
    let program = parse_program(source).unwrap();
    assert!(program.structs.is_empty());
    assert!(program.classes.is_empty());
    assert_eq!(program.intrinsic_types[0].kind, IntrinsicTypeKind::Struct);
    assert_eq!(program.intrinsic_types[1].kind, IntrinsicTypeKind::Class);
    let bytes = bincode::serialize(&program).unwrap();
    let restored: Program = bincode::deserialize(&bytes).unwrap();
    assert_eq!(restored, program);
    for source in ["@intrinsic", "@intrinsic pub enum List {}"] {
        assert!(parse_program(source).is_err(), "{source}");
    }
}

#[test]
fn parser_preserves_intrinsic_method_where_predicates() {
    let source = "@intrinsic class Table(K: type, V: type) {\nfn get(&self, key: K) -> V\nwhere K:\nOrd +\nDebug,\nV: util.Ready\nfn length(&self) -> UInt64\n}";
    let program = parse_program(source).unwrap();
    let methods = &program.intrinsic_types[0].methods;
    let predicates = &methods[0].where_predicates;
    assert_eq!(predicates.len(), 2);
    assert_eq!(predicates[0].parameter, "K");
    assert_eq!(
        predicates[0]
            .bounds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["Ord", "Debug"]
    );
    assert_eq!(predicates[1].bounds[0].to_string(), "util.Ready");
    assert!(methods[1].where_predicates.is_empty());
    assert_eq!(methods[0].span.end(), source.find("\nfn length").unwrap());
    assert!(predicates[0].associated.is_none());
}

#[test]
fn parser_preserves_function_where_and_associated_type_constraints() {
    let source = "fn show_item(I: type + Cursor, cursor: I) -> Unit\nwhere I.Item: Show,\nI: Debug\n{ () } fn main() {}";
    let program = parse_program(source).unwrap();
    let predicates = &program.functions[0].where_predicates;
    assert_eq!(predicates.len(), 2);
    assert_eq!(predicates[0].parameter, "I");
    assert_eq!(predicates[0].associated.as_deref(), Some("Item"));
    assert_eq!(
        predicates[0]
            .bounds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["Show"]
    );
    assert_eq!(predicates[1].parameter, "I");
    assert!(predicates[1].associated.is_none());
    assert_eq!(predicates[1].bounds[0].to_string(), "Debug");
}

#[test]
fn parser_rejects_incomplete_method_where_predicates() {
    for clause in [
        "where",
        "where T",
        "where T:",
        "where T: Ord +",
        "where T: Ord,",
    ] {
        let source = format!("@intrinsic class C(T: type) {{ fn f(&self) -> Unit {clause}\n}}");
        assert!(parse_program(&source).is_err(), "{source}");
    }
}

#[test]
fn parser_preserves_dynamic_trait_composition_and_generic_bounds() {
    let program = parse_program(
        "fn f(T: type + Show + Eq, value: &Dyn(A +\n util.B + Show, Item: Int64)) -> Dyn(util.B + A + Show, Item: Int64) { value } fn main() {}",
    ).unwrap();
    let function = &program.functions[0];
    assert_eq!(function.type_parameters.len(), 1);
    assert_eq!(function.type_parameters[0].bounds.len(), 2);
    let TypeExpr::Apply { arguments, .. } = &function.parameters[0].ty.kind else {
        panic!("expected type application");
    };
    let TypeExpr::TraitComposition(traits) = &arguments[0].value.kind else {
        panic!("expected trait composition");
    };
    assert_eq!(
        traits.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["A", "util.B", "Show"]
    );
    assert_eq!(
        function.parameters[0].ty.to_string(),
        "Dyn(A + util.B + Show,Item:Int64)"
    );
    for source in [
        "fn f(value: Dyn(A +)) {} fn main() {}",
        "fn f(value: List(A + B)) {} fn main() {}",
    ] {
        assert!(parse_program(source).is_err(), "{source}");
    }
}

#[test]
fn parser_builds_structured_nested_type_applications() {
    let program = parse_program(
        "fn f(x: Result(ListOf(Int64), String)) -> (Option(Int64), String) { x } fn main() {}",
    )
    .unwrap();
    let TypeExpr::Apply { callee, arguments } = &program.functions[0].parameters[0].ty.kind else {
        panic!("apply")
    };
    assert!(callee.is_name("Result"));
    assert_eq!(arguments.len(), 2);
    assert!(matches!(arguments[0].value.kind, TypeExpr::Apply { .. }));
    assert!(matches!(
        program.functions[0].return_type.as_ref().unwrap().kind,
        TypeExpr::Tuple(_)
    ));
}

#[test]
fn parser_accepts_nested_type_value_applications_in_effects() {
    parse_program(
        "eff Ask { @resumable fn question() -> Result(Option(String), String) }\n\
         fn main() {}",
    )
    .expect("nested type value application should parse");
}

#[test]
fn parser_builds_effect_aliases() {
    let program = parse_program(
        "eff tcp { fn connect() -> Unit }\n\
         effects Network = { tcp, tls }\n\
         fn main() effects { Network } {}",
    )
    .unwrap();
    assert_eq!(program.effect_aliases.len(), 1);
    assert_eq!(program.effect_aliases[0].name, "Network");
    assert_eq!(
        program.effect_aliases[0]
            .effects
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["tcp", "tls"]
    );
}

#[test]
fn parser_builds_rest_and_renamed_enum_patterns() {
    let program = parse_program(
        "enum State { Sasl(first: Int32, password: String, required: Bool); Done }\n\
         fn main() {\n\
             let state = State.Sasl(first: 1, password: \"pw\", required: true);\n\
             match state {\n\
                 State.Sasl(required: is_required, ..) => is_required;\n\
                 State.Done => false\n\
             }\n\
         }",
    )
    .unwrap();
    let debug = format!("{:#?}", program);
    assert!(debug.contains("rest: true"));
    assert!(debug.contains("name: \"is_required\""));
    for source in [
        "enum State { Done(value: Int32) } fn main() { match State.Done(value: 1) { State.Done(..,) => 0 } }",
        "enum State { Done(value: Int32) } fn main() { match State.Done(value: 1) { State.Done(.., value) => 0 } }",
    ] {
        assert!(parse_program(source).is_err(), "accepted malformed rest pattern: {source}");
    }
}

#[test]
fn parser_preserves_nested_function_type_boundaries() {
    let program =
        parse_program("fn f(x: fn(fn(Int64) -> String) -> List(Int64)) {} fn main() {}").unwrap();
    let TypeExpr::Function { parameters, result } = &program.functions[0].parameters[0].ty.kind
    else {
        panic!("fn")
    };
    assert!(matches!(
        parameters[0].value.kind,
        TypeExpr::Function { .. }
    ));
    assert!(matches!(result.kind, TypeExpr::Apply { .. }));
}

#[test]
fn parser_normalizes_type_parameters_before_inferring_container_parameters() {
    let program = parse_program(
        "fn first(T: type, values: List(T)) -> Option(T) { values.head() } fn main() {}",
    )
    .unwrap();
    let function = &program.functions[0];
    assert_eq!(function.type_parameters.len(), 1);
    assert_eq!(function.type_parameters[0].name, "T");
    assert_eq!(function.parameters.len(), 1);
    assert_eq!(function.parameters[0].name, "values");
    let program = parse_program("fn Wrap(T: type) -> type { List(T) } fn main() {}").unwrap();
    assert!(program.functions[0].parameters.is_empty());
    assert_eq!(program.functions[0].type_parameters.len(), 1);
}

#[test]
fn parser_accepts_bounded_type_parameters_in_value_position() {
    let program =
        parse_program("fn empty(T: type + Hash + Eq) -> Set(T) { Set#{} } fn main() {}").unwrap();
    assert_eq!(program.functions[0].type_parameters.len(), 1);
    assert_eq!(program.functions[0].type_parameters[0].name, "T");
    assert_eq!(
        program.functions[0].type_parameters[0]
            .bounds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["Hash", "Eq"]
    );
}

#[test]
fn parser_rejects_interleaved_and_mixed_type_parameters() {
    for source in [
        "fn f(x: Int64, T: type) {} fn main() {}",
        "fn f(T: type, x: T, U: type) {} fn main() {}",
    ] {
        assert!(parse_program(source).is_err(), "{source}");
    }
}

#[test]
fn parser_builds_effect_declarations_and_function_annotations() {
    let program = parse_program(
        "eff Clock { fn now() -> Int64 @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn read() -> Int64 effects { Clock } { Clock.now() } fn main() {}",
    )
    .expect("effect declaration should parse");
    assert_eq!(program.effects.len(), 1);
    assert_eq!(program.effects[0].operations.len(), 2);
    assert_eq!(
        program.effects[0].operations[1].mode,
        super::super::super::ast::EffectMode::Normal
    );
    assert!(program.effects[0].operations[1].suspends);
    assert_eq!(program.functions[0].effect_names[0].to_string(), "Clock");
}

#[test]
fn parser_accepts_independent_effect_annotations() {
    let program = parse_program(
        "eff Ask { @suspends @resumable fn question() -> Int32 }\n\
         fn main() {}",
    )
    .expect("independent effect annotations should parse");
    assert_eq!(
        program.effects[0].operations[0].mode,
        super::super::super::ast::EffectMode::Resumable
    );
    assert!(program.effects[0].operations[0].suspends);
}

#[test]
fn parser_rejects_suspending_abortive_operations() {
    assert!(
        parse_program("eff Failure { @suspends @aborts fn stop() -> Unit } fn main() {}").is_err()
    );
}

#[test]
fn parser_builds_function_type_parameters() {
    let program = parse_program(
        "fn identity(T: type, U: type, left: T, right: U) -> T { left } fn main() {}",
    )
    .expect("generic function should parse");
    assert_eq!(
        program.functions[0]
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>(),
        ["T", "U"]
    );
}

#[test]
fn parser_infers_type_parameters_from_generic_annotations() {
    let program =
        parse_program("fn length(value: Map(K, V)) -> UInt64 { value.length() } fn main() {}")
            .expect("implicit generic function should parse");
    assert_eq!(
        program.functions[0]
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>(),
        ["K", "V"]
    );
}

#[test]
fn parser_handles_tuple_parameters() {
    let program = parse_program("fn first(pair: (Int32, Int32)) -> Int32 { pair.0 }").unwrap();
    assert_eq!(
        program.functions[0].parameters[0].ty.to_string(),
        "(Int32,Int32)"
    );
}

#[test]
fn parser_handles_classes_and_assignment() {
    let program = parse_program(
        "class Counter { var value: Int32; fn increment() { self.value = self.value + 1 } } \
         fn main() { let counter = Counter(value: 0); counter.increment() }",
    )
    .unwrap();
    assert_eq!(program.classes[0].name, "Counter");
    assert!(program.classes[0].fields[0].mutable);
    let ExprKind::Block(expressions) = &program.classes[0].methods[0].body.kind else {
        panic!("expected a method block");
    };
    assert!(matches!(expressions[0].kind, ExprKind::Assign { .. }));
}

#[test]
fn parser_handles_type_annotations() {
    let program = parse_program("fn main() { let x: Int64 = 42; x }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::Let { annotation, .. } = &expressions[0].kind else {
        panic!("expected let");
    };
    assert!(annotation.is_some());
    assert_eq!(annotation.as_ref().unwrap().to_string(), "Int64");
}

#[test]
fn parser_accepts_unit_return_type_as_empty_tuple() {
    let program =
        parse_program("fn announce() -> () { println(\"ok\") } fn main() { announce() }").unwrap();
    assert_eq!(
        program.functions[0]
            .return_type
            .as_ref()
            .unwrap()
            .to_string(),
        "()"
    );
}

#[test]
fn parser_builds_imports_and_function_visibility() {
    let program =
        parse_program("import joky/string\nimport user\npub fn exported() {}\nfn main() {}")
            .unwrap();
    assert_eq!(program.imports.len(), 2);
    assert_eq!(program.imports[0].path, ["joky", "string"]);
    assert_eq!(program.imports[1].path, ["user"]);
    assert_eq!(program.functions[0].visibility, Visibility::Public);
    assert_eq!(program.functions[1].visibility, Visibility::Private);
}

#[test]
fn parser_builds_module_constants() {
    let program = parse_program(
        "pub const MAX_RETRIES: Int32 = 3\n\
         const SERVICE_NAME = \"api\"\n\
         fn main() {}",
    )
    .unwrap();
    assert_eq!(program.constants.len(), 2);
    assert_eq!(program.constants[0].name, "MAX_RETRIES");
    assert_eq!(
        program.constants[0]
            .annotation
            .as_ref()
            .unwrap()
            .to_string(),
        "Int32"
    );
    assert_eq!(program.constants[0].visibility, Visibility::Public);
    assert_eq!(program.constants[1].name, "SERVICE_NAME");
    assert!(program.constants[1].annotation.is_none());
    assert_eq!(program.constants[1].visibility, Visibility::Private);
}

#[test]
fn parser_accepts_intrinsic_generic_class_declarations() {
    let program = parse_program(
        "@intrinsic pub class MutList(T: type) {\n\
             fn length(&self) -> UInt64\n\
             fn push(&self, item: T) -> Unit\n\
         }\n\
         fn main() {}",
    )
    .unwrap();
    assert_eq!(program.intrinsic_types.len(), 1);
    assert_eq!(program.intrinsic_types[0].type_parameters.len(), 1);
    assert_eq!(program.functions.len(), 1);
    assert_eq!(program.functions[0].name, "main");
    assert!(program.classes.is_empty());
}

#[test]
fn old_c_extern_declaration_syntax_is_rejected() {
    for source in [
        "pub extern \"C\" fn magnitude(value: Int32) -> Int32 from \"libc.so.6\" as \"abs\";",
        "extern \"Rust\" fn f() -> Unit from \"lib\";",
        "extern \"C\" fn f() -> Unit {}",
        "extern \"C\" fn f() -> Unit from \"\";",
        "extern \"C\" fn f() -> Unit from \"lib\" as \"\";",
        "extern \"C\" fn f() -> Unit from \"lib\"",
        "extern \"C\" fn f(...) -> Unit from \"lib\";",
    ] {
        assert!(parse_program(source).is_err(), "{source}");
    }
}

#[test]
fn parser_accepts_c_extern_annotation() {
    let program = parse_program(
        "@extern(c, \"libc.so.6\", \"abs\") pub fn absolute(value: Int32) -> Int32; fn main() {}",
    )
    .unwrap();
    let function = &program.functions[0];
    assert_eq!(function.foreign.as_ref().unwrap().library, "libc.so.6");
    assert_eq!(function.foreign.as_ref().unwrap().symbol, "abs");
    let platform = parse_program(
        "@extern(c, \"libSystem.B.dylib\", \"tmpfile\", os = \"macos\") fn tmpfile() -> Unit; fn main() {}",
    )
    .unwrap();
    assert_eq!(
        platform.functions[0]
            .foreign
            .as_ref()
            .unwrap()
            .target_os
            .as_deref(),
        Some("macos")
    );
    for source in [
        "@extern(javascript, \"lib\", \"f\") fn f() -> Unit;",
        "@extern(c, \"lib\") fn f() -> Unit;",
    ] {
        assert!(parse_program(source).is_err(), "{source}");
    }
}

#[test]
fn parser_records_borrowed_and_owned_intrinsic_receivers() {
    let program = parse_program(
        "@intrinsic class File {\n\
             fn read_chunk(&self, max: UInt64) -> Bytes\n\
             fn close(self) -> Unit\n\
         }\n\
         fn main() {}",
    )
    .unwrap();
    assert_eq!(
        program.intrinsic_types[0].methods[0].receiver_mode,
        crate::syntax::ReceiverMode::Borrowed
    );
    assert_eq!(
        program.intrinsic_types[0].methods[1].receiver_mode,
        crate::syntax::ReceiverMode::Owned
    );
}

#[test]
fn parser_records_intrinsic_effect_binding_and_rejects_invalid_annotations() {
    let program = parse_program(
        "@intrinsic(effect = file) pub class File { fn close(self) -> Unit } fn main() {}",
    )
    .unwrap();
    assert_eq!(program.intrinsic_types[0].effect.as_deref(), Some("file"));
    for annotation in [
        "@intrinsic()",
        "@intrinsic(file)",
        "@intrinsic(target = file)",
        "@intrinsic(effect file)",
        "@intrinsic(effect =)",
    ] {
        assert!(
            parse_program(&format!("{annotation} class File {{}} fn main() {{}}")).is_err(),
            "{annotation}"
        );
    }
}

#[test]
fn parser_records_class_receivers_and_borrowed_parameters() {
    let program = parse_program("class C { fn implicit() {} fn borrow(&self, other: &C) {} fn consume(self) {} } fn main() {}").unwrap();
    let methods = &program.classes[0].methods;
    assert_eq!(methods[0].receiver_mode, None);
    assert_eq!(
        methods[1].receiver_mode,
        Some(crate::syntax::ReceiverMode::Borrowed)
    );
    assert!(methods[1].parameters[0].borrowed);
    assert_eq!(
        methods[2].receiver_mode,
        Some(crate::syntax::ReceiverMode::Owned)
    );
    assert!(parse_program("class C { fn read(\n&self\n) {} } fn main() {}").is_ok());
}

#[test]
fn parser_builds_show_trait_and_generic_bound() {
    let program = parse_program(
        "trait Show { fn show(&self) -> String }\n\
         fn log(T: type + Show, value: T) { println(value) }\n\
         fn main() {}",
    )
    .unwrap();
    assert_eq!(program.traits.len(), 1);
    assert_eq!(program.traits[0].name, "Show");
    assert_eq!(program.traits[0].methods[0].name, "show");
    assert_eq!(program.functions[0].type_parameters[0].name, "T");
    assert_eq!(
        program.functions[0].type_parameters[0].bounds[0].to_string(),
        "Show"
    );
}

#[test]
fn parser_builds_multiple_generic_bounds() {
    let program = parse_program("fn index(K: type + Hash + Eq, value: K) {} fn main() {}").unwrap();
    let bounds = &program.functions[0].type_parameters[0].bounds;
    assert_eq!(bounds.len(), 2);
    assert_eq!(bounds[0].to_string(), "Hash");
    assert_eq!(bounds[1].to_string(), "Eq");
}

#[test]
fn parser_accepts_public_generic_type_declarations() {
    parse_program("pub type Map(K: type + Hash + Eq, V: type) fn main() {}").unwrap();
}

#[test]
fn parser_builds_trait_method_parameters() {
    let program = parse_program(
        "trait Compare { fn equals(&self, other: Self, expected: Int32) -> Bool }\n\
         fn main() {}",
    )
    .unwrap();
    let method = &program.traits[0].methods[0];
    assert_eq!(method.name, "equals");
    assert_eq!(method.parameters.len(), 2);
    assert_eq!(method.parameters[0].name, "other");
    assert_eq!(method.parameters[0].ty.to_string(), "Self");
    assert_eq!(method.parameters[1].name, "expected");
    assert_eq!(method.parameters[1].ty.to_string(), "Int32");
}

#[test]
fn parser_builds_associated_types() {
    let program = parse_program(
        "trait Iterator { type Item fn next(&self) -> Option(Item) }\n\
         struct Values { let value: Int32 }\n\
         impl Iterator for Values { type Item = Int32 fn next() -> Option(Int32) { Some(self.value) } }\n\
         fn main() {}",
    )
    .unwrap();
    assert_eq!(program.traits[0].associated_types[0].name, "Item");
    assert_eq!(program.impls[0].associated_types[0].name, "Item");
    assert_eq!(program.impls[0].associated_types[0].ty.to_string(), "Int32");
}

#[test]
fn parser_builds_trait_impl() {
    let program = parse_program(
        "struct Point { let x: Int32 }\n\
         impl Show for Point { fn show() -> String { \"Point\" } }\n\
         fn main() {}",
    )
    .unwrap();
    assert_eq!(program.impls.len(), 1);
    assert_eq!(program.impls[0].trait_name, "Show");
    assert_eq!(program.impls[0].type_name, "Point");
    assert_eq!(program.impls[0].methods[0].name, "show");
}
