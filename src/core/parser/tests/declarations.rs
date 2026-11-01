use super::*;

#[test]
fn intrinsic_declarations_parse_without_placeholder_bodies() {
    let module =
        parse(r#"#intrinsic("text.clone") pub fn duplicate(text: &String) @noescape -> String;"#)
            .unwrap();
    let Item::FnDef(f) = &module.items[0] else {
        unreachable!()
    };
    assert!(f.public);
    assert!(f.noescape.as_ref().unwrap().all);
    let (index, attribute) = &module.intrinsic_attributes[0];
    assert_eq!(*index, 0);
    assert_eq!(attribute.name, "text.clone");
    assert!(!attribute.has_body);
    assert!(f.compiler_intrinsic.is_none());
    for source in [
        "#intrinsic fn f();",
        "#intrinsic(42) fn f();",
        "#intrinsic(name: \"text.clone\") fn f();",
        "#intrinsic(\"a\", \"b\") fn f();",
        "#intrinsic(\"a\") #intrinsic(\"b\") fn f();",
        "fn f();",
        "fn f()",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}

#[test]
fn intrinsic_declarations_allow_optional_semicolons() {
    for signature in [
        "pub fn duplicate(text: &String) @noescape -> String",
        "fn stop(message: &String) @noescape",
        "fn primitive<T>(value: T) -> T",
        "fn primitive()",
    ] {
        for separator in ["", ";"] {
            for following in [
                "",
                " fn next() {}",
                "\n// Another intrinsic.\n#intrinsic(\"next\") fn next()",
                " extern fn next()",
            ] {
                let declaration = format!("#intrinsic(\"test\") {signature}{separator}");
                let source = format!("{declaration}{following}");
                let module = parse(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
                assert_eq!(module.items.len(), if following.is_empty() { 1 } else { 2 });
                let Item::FnDef(function) = &module.items[0] else {
                    panic!()
                };
                assert_eq!(function.span.slice(&source), Some(declaration.as_str()));
                assert!(!module.intrinsic_attributes[0].1.has_body);
                let Expr::Block { exprs, span } = &function.body else {
                    panic!()
                };
                assert!(exprs.is_empty());
                assert_eq!(span.slice(&source), Some(separator));
            }
        }
    }
}

#[test]
fn enums_and_match_preserve_spans_and_sequence_boundaries() {
    let source = "// 🙂\nenum Choice { Empty, Value(Int32,), } fn f(e: Choice) -> Int32 { match e { Choice.Empty => 0, Value(n,) => n; n + 1 } }";
    let module = parse(source).unwrap();
    let enumeration = &module.enums[0];
    assert_eq!(enumeration.name_span.slice(source), Some("Choice"));
    assert_eq!(
        enumeration.variants[1].span.slice(source),
        Some("Value(Int32,)")
    );
    let Item::FnDef(function) = &module.items[0] else {
        panic!()
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!()
    };
    let Expr::Match {
        value, arms, span, ..
    } = &exprs[0]
    else {
        panic!()
    };
    assert_eq!(value.span().slice(source), Some("e"));
    assert_eq!(arms[0].pattern.span().slice(source), Some("Choice.Empty"));
    assert_eq!(arms[1].pattern.span().slice(source), Some("Value(n,)"));
    assert_eq!(arms[1].span.slice(source), Some("Value(n,) => n; n + 1"));
    assert!(
        matches!(&arms[1].body, Expr::Sequence { trailing: false, exprs, .. } if exprs.len() == 2)
    );
    assert!(span.slice(source).unwrap().starts_with("match e {"));
    for source in [
        "fn f(e: E) { match e { _ => return } }",
        "fn f(e: E) { match e { _ => 1; } }",
        "fn f(e: E) { match (P { x: e }).x { _ => {} } }",
        "fn f(e: E) { match e { A => match e { _ => 1 }, _ => 2 } }",
        "fn f(e: E) { match e { A(B(x)) => x } }",
        "fn f(e: E) { match e { _ => true || false } }",
        "fn f() { E.Value(1; 2) }",
    ] {
        assert!(parse(source).is_ok(), "{source}");
    }
    for source in [
        "enum E { A } import joky/vm",
        "enum E { ,A }",
        "enum E { A,, B }",
        "enum E { A() }",
        "enum E { A(Int32, Bool) }",
        "fn f() { enum E { A } }",
        "fn enum() {}",
        "fn f(match: Int32) {}",
        "fn f(e: E) { match e { | A => 1 } }",
        "fn f(e: E) { match e { A() => 1 } }",
        "fn f(e: E) { match e { A(x, y) => x } }",
        "fn f(e: E) { match e { A(x) if x > 0 => x } }",
        "fn f(e: E) { match e { _ => } }",
        "fn f(e: E) { match e { | 1 => 0 } }",
        "fn f(e: E) { match e { | A = 1 } }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}

#[test]
fn records_parse_fields_places_and_condition_boundaries() {
    let source = "// 🙂\nstruct 点 { x: Int32, y: Int32 } fn f() { var p = 点 { y: 2, x: 1, }\n p.x = 3\n if p.x == 3 {}\n while (点 { x: 1, y: 2 }).x < 0 {} }";
    let module = parse(source).unwrap();
    let record = &module.records[0];
    assert_eq!(record.name_span.slice(source), Some("点"));
    assert_eq!(record.fields[0].span.slice(source), Some("x: Int32"));
    let Item::FnDef(function) = &module.items[0] else {
        panic!()
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!()
    };
    assert!(
        matches!(&exprs[1], Expr::FieldAssign { target, .. } if matches!(target.as_ref(), Expr::Field { name, .. } if name == "x"))
    );
    assert!(matches!(&exprs[2], Expr::If { .. }));
    assert!(matches!(&exprs[3], Expr::While { .. }));
    // A following ordinary block stays separate. An empty or field-shaped
    // brace after a name is always a constructor outside conditions.
    assert!(parse("fn f() { let x = 1\n { let y = x\n y } }").is_ok());
    assert!(parse("fn f() { let x = 1\n x {} }").is_ok());
    for source in [
        "struct P {} import joky/vm",
        "struct P { ,x: Int32 }",
        "struct P { x: Int32,, y: Int32 }",
        "struct P { x: }",
        "struct P",
        "struct P {} fn f() { P\n { , } }",
        "struct P {} fn f() { P { x: 1,, } }",
        "struct P {} fn f() { P { x: 1; y: 2 } }",
        "fn f() { make().x = 1 }",
        "fn f() { let p = 1\n p.x(1 2) }",
        "fn f() { struct P {} }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}

#[test]
fn record_shorthand_preserves_field_spans_and_constructor_boundaries() {
    for (constructor, body, count) in [
        ("P", "x", 1),
        ("P", "x, y,", 2),
        ("P", "x\ny", 2),
        ("P", "x\r\ny", 2),
        ("P", "x // separator\n y: 2", 2),
        ("P", "x /* separator\n */ y", 2),
        ("P", "x, y: 2", 2),
        ("P", "y: 2, x", 2),
        ("model.P::<Int32>", "x", 1),
    ] {
        let source = format!("fn f() {{ {constructor} {{ {body} }} }}");
        let module = parse(&source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        let Expr::Record { fields, .. } = &exprs[0] else {
            panic!("{source}")
        };
        assert_eq!(fields.len(), count, "{source}");
        let field = fields.iter().find(|field| field.name == "x").unwrap();
        assert_eq!(field.span.slice(&source), Some("x"));
        assert!(matches!(&field.value, Expr::Ident { name, span }
            if name == "x" && *span == field.span));
    }
    for source in [
        "fn f() { P { x, y z } }",
        "fn f() { P { x,, } }",
        "fn f() { P { x, y; } }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    let module = parse(
        "fn f() { value\n { x }\n if flag { x }\n while flag { x }\n match value { x => x } }",
    )
    .unwrap();
    let Item::FnDef(function) = &module.items[0] else {
        panic!()
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!()
    };
    assert!(matches!(
        &exprs[..],
        [
            Expr::Ident { .. },
            Expr::Block { .. },
            Expr::If { .. },
            Expr::While { .. },
            Expr::Match { .. }
        ]
    ));
}

#[test]
fn type_aliases_keep_source_spans_and_allow_forward_declarations() {
    let source = "// 🙂\nimport joky/vm; type 数 = Later\nfn f(x: 数,) -> Later { let y: 数 = x\n y } type Later = Int32; type Handle = vm.Function";
    let module = parse(source).unwrap();
    assert_eq!(module.imports.len(), 1);
    assert_eq!(module.items.len(), 1);
    assert_eq!(module.type_aliases.len(), 3);
    for (alias, name, target, spelling) in [
        (
            &module.type_aliases[0],
            "数",
            Type::Named("Later".into()),
            "type 数 = Later",
        ),
        (
            &module.type_aliases[1],
            "Later",
            Type::Int32,
            "type Later = Int32",
        ),
        (
            &module.type_aliases[2],
            "Handle",
            Type::Named("vm.Function".into()),
            "type Handle = vm.Function",
        ),
    ] {
        assert_eq!(alias.name, name);
        assert_eq!(alias.target, target);
        assert_eq!(alias.name_span.slice(source), Some(name));
        assert_eq!(
            alias.target_span.slice(source),
            Some(target.to_string().as_str())
        );
        assert_eq!(alias.span.slice(source), Some(spelling));
    }
    let Item::FnDef(function) = &module.items[0] else {
        panic!()
    };
    assert_eq!(function.params[0].ty, Type::Named("数".into()));
    assert_eq!(function.ret_ty, Type::Named("Later".into()));
}

#[test]
fn malformed_aliases_and_non_top_level_aliases_are_rejected() {
    for (source, marked, message) in [
        ("type = Int32", "=", "Expected an identifier"),
        ("type Count Int32", "Int32", "Expected Eq"),
        ("type Count =", "", "Expected a type"),
        ("type Count = 1", "1", "Expected a type"),
        ("type Int32 = Bool", "Int32", "Expected an identifier"),
        ("type Count.Value = Int32", ".", "Expected Eq"),
        ("type Count<T> Int32", "Int32", "Expected Eq"),
        (
            "fn f() { type Count = Int32 }",
            "type",
            "Expected an expression",
        ),
        (
            "type Count = Int32 import joky/vm",
            "import",
            "Imports must precede",
        ),
        ("fn type() {}", "type", "Expected an identifier"),
    ] {
        let error = parse(source).unwrap_err();
        assert_eq!(error.span.slice(source), Some(marked), "{source}");
        assert!(error.message.contains(message), "{source}: {error}");
    }
}

#[test]
fn imports_precede_definitions_and_extern_declarations() {
    for declaration in ["fn f() {}", "extern fn f()", "fn f() {}; extern fn g();"] {
        let source = format!("// é🙂\n{declaration}\n; import joky/vm");
        let error = parse(&source).unwrap_err();
        assert_eq!(error.line, 3);
        assert_eq!(error.span.slice(&source), Some("import"));
        assert_eq!(error.message, "Imports must precede all declarations");
    }
    let module =
        parse("; // leading trivia\nimport joky/vm;;\nimport user/math\nextern fn f(); fn g() {}")
            .unwrap();
    assert_eq!(module.imports.len(), 2);
    assert_eq!(module.items.len(), 2);
    assert_eq!(parse("import joky/vm;").unwrap().imports.len(), 1);
}

#[test]
fn imports_support_rust_style_type_and_value_selection() {
    let source = "import joky/option.{type Option, Some as Present, None} as option fn f() {}";
    let module = parse(source).unwrap();
    let import = &module.imports[0];
    assert_eq!(import.path, "joky/option");
    assert_eq!(import.alias.as_deref(), Some("option"));
    assert_eq!(import.members.len(), 3);
    assert_eq!(import.members[0].name, "Option");
    assert_eq!(import.members[0].kind, ImportMemberKind::Type);
    assert_eq!(import.members[1].alias.as_deref(), Some("Present"));
    assert_eq!(import.members[2].kind, ImportMemberKind::Value);
    assert_eq!(
        import.span.slice(source),
        Some("import joky/option.{type Option, Some as Present, None} as option")
    );
}

#[test]
fn parses_add_function() {
    let src = "fn add(x: Int32, y: Int32) -> Int32 { x + y }";
    let module = parse(src).expect("Parsing failed");
    assert_eq!(module.items.len(), 1);
    match &module.items[0] {
        Item::FnDef(f) => {
            assert_eq!(f.name, "add");
            assert_eq!(f.params.len(), 2);
            assert_eq!(f.ret_ty, Type::Int32);
            assert!(!f.suspends);
        }
        _ => panic!("Expected FnDef"),
    }
}

#[test]
fn parses_extern_function() {
    let src = "extern fn host_multiply(x: Int32, y: Int32) -> Int32";
    let module = parse(src).expect("Parsing failed");
    match &module.items[0] {
        Item::ExternFn(e) => {
            assert_eq!(e.name, "host_multiply");
            assert_eq!(e.ret_ty, Type::Int32);
        }
        _ => panic!("Expected ExternFn"),
    }
}

#[test]
fn preserves_stable_spans_for_imports_and_parameters() {
    let source = "import joky/vm fn add(x: Int32, y: Bool) -> Int32 { x + 1 }";
    let module = parse(source).unwrap();
    assert_eq!(module.imports[0].path, "joky/vm");
    assert_eq!(module.imports[0].span.slice(source), Some("import joky/vm"));
    let Item::FnDef(function) = &module.items[0] else {
        panic!("expected function definition");
    };
    assert_eq!(function.params[0].span.slice(source), Some("x: Int32"));
    assert_eq!(function.params[1].span.slice(source), Some("y: Bool"));
}

#[test]
fn parses_suspending_function() {
    let src = "fn delayed_add(x: Int32, y: Int32) @suspends -> Int32 { x + y }";
    let module = parse(src).expect("Parsing failed");
    match &module.items[0] {
        Item::FnDef(f) => {
            assert!(f.suspends);
            assert_eq!(f.ret_ty, Type::Int32);
        }
        _ => panic!("Expected FnDef"),
    }
}

#[test]
fn parses_multiple_items() {
    let src = r#"
fn add(x: Int32, y: Int32) -> Int32 { x + y }
extern fn host_multiply(x: Int32, y: Int32) -> Int32
"#;
    let module = parse(src).expect("Parsing failed");
    assert_eq!(module.items.len(), 2);
}

#[test]
fn module_dispatch_preserves_import_aliases_visibility_and_declaration_order() {
    let source = "// λ\nimport app/data as before.{type Row as Item, make as build} import app/other.{read as get} as after #repr(C) pub struct Row { value: Int32 } pub enum Choice { Empty } pub type Count = Int32 pub trait Read { fn read(self) -> Count } impl Read for Row { fn read(self) -> Count { self.value } } pub fn check() -> Count { 42 }";
    let module = parse(source).unwrap();
    assert_eq!(module.imports[0].alias.as_deref(), Some("before"));
    assert_eq!(module.imports[0].members[0].kind, ImportMemberKind::Type);
    assert_eq!(module.imports[0].members[0].alias.as_deref(), Some("Item"));
    assert_eq!(module.imports[0].members[1].alias.as_deref(), Some("build"));
    assert_eq!(module.imports[1].alias.as_deref(), Some("after"));
    assert_eq!(module.imports[1].members[0].alias.as_deref(), Some("get"));
    assert_eq!(module.records[0].attributes[0].path, "repr");
    assert!(module.records[0].public);
    assert!(module.enums[0].public);
    assert!(module.type_aliases[0].public);
    assert!(module.traits[0].public);
    assert!(matches!(&module.items[0], Item::FnDef(f) if f.public));
    for declaration in [
        "struct R {}",
        "enum E { A }",
        "type T = Int32",
        "trait Read { fn read(self) -> Int32 }",
        "impl R {}",
        "fn f() {}",
        "extern fn f()",
        "unsafe extern \"C\" fn f()",
    ] {
        let source = format!("{declaration}; import app/data");
        let error = parse(&source).unwrap_err();
        assert_eq!(error.message, "Imports must precede all declarations");
        assert_eq!(error.span.slice(&source), Some("import"));
    }
}

#[test]
fn record_fields_require_commas_or_real_newlines_and_default_to_private() {
    // Positives: commas, newlines, CRLF, trailing comma, empty records,
    // comments that carry their terminating newline, multi-line types and
    // initializers, and mixed `pub` visibility.
    for source in [
        "struct P { x: Int32, y: Int32 }",
        "struct P { x: Int32, y: Int32, }",
        "struct P {\n    x: Int32\n    y: Int32\n}",
        "struct P {\r\n    x: Int32,\r\n    y: Int32,\r\n}",
        "struct P {\n    // note\n    x: Int32\n    // gap\n    y: Int32\n}",
        "struct P {\n    x: Int32, // trailing\n    y: Int32,\n}",
        "struct P {\n    x: Map<\n        Int32,\n        Int32,\n    >\n    y: Int32\n}",
        "struct P { x: Int32 }",
        "struct P {}",
        "struct P { x: Int32, } fn f() -> P { P { x: 1, } }",
        "struct P { x: Int32\n    y: Int32 } fn f() -> P {\n    P {\n        x: 1\n        y: 2\n    }\n}",
        "struct P {\n    x: Int32\n    y: Int32\n}\nfn f() -> P {\n    P {\n        x: g(\n            1,\n            2,\n        )\n        y: 2\n    }\n}\nfn g(a: Int32, b: Int32) -> Int32 { a }",
        "pub struct P { pub x: Int32, y: Int32 }",
        "fn f() { let p = P { x: g(\n    1,\n)\n    y: 2 } }\nfn g(a: Int32) -> Int32 { a }",
    ] {
        assert!(parse(source).is_ok(), "{source}");
    }
    // A multi-line type does not end the field: a same-line next field still
    // needs a comma, so comparing field start lines would wrongly accept it.
    assert!(
        parse("struct P {\n    x: Map<\n        Int32,\n        Int32,\n    > y: Int32\n}")
            .is_err()
    );
    // Negatives: same-line separators, semicolons, empty fields, and the
    // removed `private` modifier.
    for source in [
        "struct P { x: Int32 y: Int32 }",
        "struct P {} fn f() { P { x: 1 y: 2 } }",
        "struct P { x: Int32; y: Int32 }",
        "struct P { x: Int32 ,; y: Int32 }",
        "struct P {} fn f() { P { x: 1; y: 2 } }",
        "struct P { private x: Int32 }",
        "struct P {} fn f() { let p = P { x: 1 } } struct Q { private values: Array<Int32> }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    let error = parse("struct P { private x: Int32 }").unwrap_err();
    assert!(
        error.message.contains("private by default"),
        "{}",
        error.message
    );
    let error = parse("struct P { x: Int32 y: Int32 }").unwrap_err();
    assert_eq!(
        error.message,
        "Expected ',' or a newline between record fields"
    );
    assert_eq!(
        error.span.slice("struct P { x: Int32 y: Int32 }"),
        Some("y")
    );
    // `pub` fields are recorded; unmarked fields default to private.
    let module = parse("pub struct P { pub x: Int32, y: Int32 }").unwrap();
    let record = &module.records[0];
    assert_eq!(record.public_fields, vec!["x".to_owned()]);
}

#[test]
fn enum_variants_require_commas_or_real_newlines() {
    for source in [
        "enum E { A, B(Int32), }",
        "enum E { A\n    B(Int32)\n}",
        "enum E { A((Int32, Bool)), B }",
        "enum E<T> { None, Some(T) }",
        "enum E { A(Int32,), B }",
    ] {
        assert!(parse(source).is_ok(), "{source}");
    }
    // A multi-line payload type does not end the variant: a next variant on
    // the payload's closing line still needs a comma.
    assert!(parse("enum E {\n    A(Map<\n        Int32,\n    >)\n    B\n}").is_ok());
    assert!(parse("enum E {\n    A(Map<\n        Int32,\n    >) B\n}").is_err());
    // Negatives: same-line separators, semicolons, and empty variants.
    for source in [
        "enum E { A B(Int32) }",
        "enum E { A(Int32) B }",
        "enum E { , A }",
        "enum E { A,, B }",
        "enum E { A; B }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    let error = parse("enum E { A B(Int32) }").unwrap_err();
    assert_eq!(
        error.message,
        "Expected ',' or a newline between enum variants"
    );
    assert_eq!(error.span.slice("enum E { A B(Int32) }"), Some("B"));
}

#[test]
fn match_arms_use_commas_or_real_newlines_like_record_fields() {
    for body in [
        "A => 1, B => 2,",
        "A => 1\nB => 2",
        "A => { 1 }\nB => { 2 },",
        "A => 1;\nB => 2",
        "A => 1; // discard\nB => 2",
        "A => (1\n| 2)\nB => 4 | 8",
        "A => match e { A => 1\nB => 2 }\nB => 3",
    ] {
        let source = format!("fn f(e: E) {{ match e {{ {body} }} }}");
        parse(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
    for body in [
        "| A => 1",
        ", A => 1",
        "A => 1,, B => 2",
        "A => 1 B => 2",
        "A => { 1 } B => { 2 }",
        "A => {\n1\n} B => 2",
        "A => (\n1\n) B => 2",
    ] {
        let source = format!("fn f(e: E) {{ match e {{ {body} }} }}");
        let error = parse(&source).unwrap_err();
        assert!(error.span.slice(&source).is_some(), "{source}: {error}");
    }
}

#[test]
fn conditional_patterns_preserve_spans_and_match_payload_syntax() {
    let source = "fn f() { if let Some((None, Point { x, .. })) = value { x; } else if let None = value {}\n while let Some(n) = next() { n } }";
    let module = parse(source).unwrap();
    let Item::FnDef(function) = &module.items[0] else {
        panic!()
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!()
    };
    let Expr::Match {
        conditional,
        value,
        arms,
        span,
        ..
    } = &exprs[0]
    else {
        panic!()
    };
    assert!(*conditional);
    assert_eq!(value.span().slice(source), Some("value"));
    assert_eq!(
        arms[0].pattern.span().slice(source),
        Some("Some((None, Point { x, .. }))")
    );
    assert!(span.slice(source).unwrap().starts_with("if let"));
    assert!(matches!(
        arms[1].body,
        Expr::Match {
            conditional: true,
            ..
        }
    ));
    let Expr::While { body, span, .. } = &exprs[1] else {
        panic!()
    };
    assert_eq!(span.slice(source), Some("while let Some(n) = next() { n }"));
    let Expr::Match {
        conditional: true,
        arms,
        ..
    } = body.as_ref()
    else {
        panic!()
    };
    assert!(matches!(
        arms[0].body,
        Expr::Sequence { trailing: true, .. }
    ));
    assert!(matches!(arms[1].body, Expr::Break { .. }));
    for source in [
        "fn f() { if let Some(n) value {} }",
        "fn f() { while let Some(n) == value {} }",
        "fn f() { if let Some(n) = value {} else let None = value {} }",
        "fn f() { if let Some(n) = value && let Some(m) = other {} }",
        "fn f() { while let Some(n) = value {}\n else {} }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}

#[test]
fn every_declarations_parse_literal_periods_and_reject_invalid_attributes() {
    for (period, value) in [
        ("1ns", 1),
        ("250us", 250_000),
        ("1_000ms", 1_000_000_000),
        ("1.5s", 1_500_000_000),
        ("18446744073709551615ns", u64::MAX),
    ] {
        let source = format!("#state struct State{{n:Int32}} impl State {{#every({period}) pub fn tick(&mut self) {{}}}}");
        let module = parse(&source).unwrap();
        let f = &module.implementations[0].methods[0];
        assert_eq!(f.every.unwrap().get(), value);
        assert!(f.public);
        assert!(f.span.slice(&source).unwrap().starts_with("#every"));
    }
    for attr in [
        "#every",
        "#every()",
        "#every(0ms)",
        "#every(-1ms)",
        "#every(1000)",
        "#every(0x10)",
        "#every(0.1ns)",
        "#every(18446744073709551616ns)",
        "#every(1,2)",
        "#every(ms:1)",
        "#every(SECOND)",
        "#every(true)",
        "#every(1ms) #every(2ms)",
        "#every(1ms) #intrinsic(\"domain.every\")",
    ] {
        assert!(
            parse(&format!("impl State {{{attr} fn tick(&mut self){{}}}}")).is_err(),
            "{attr}"
        );
    }
    for source in [
        "#every(1ms) fn tick(s:&mut State){}",
        "#every(1ms) extern fn tick(s:&mut State)",
        "impl Trait for State {#every(1ms) fn tick(&mut self){}}",
        "impl State {#every(1ms) fn tick(&mut self);}",
        "impl State {#every(1ms) const N:Int32=1}",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}

#[test]
fn test_declarations_validate_signatures_and_preserve_formatting() {
    let source = "#test\nfn checks_value() {}\n#test\npub fn suspends() @suspends {}\n";
    let module = parse(source).unwrap();
    assert!(module
        .items
        .iter()
        .all(|item| matches!(item, Item::FnDef(f) if f.is_test())));
    assert_eq!(super::super::format(source).unwrap(), source);
    for source in [
        "#test() fn f() {}",
        "#test(1) fn f() {}",
        "#test #test fn f() {}",
        "#test #intrinsic(\"check.panic\") fn f() {}",
        "#test fn main() {}",
        "#test fn f(n: Int32) {}",
        "#test fn f<T>() {}",
        "#test fn f() -> Int32 { 1 }",
        "#test extern fn f()",
        "#test struct Value { value: Int32 }",
        "#test enum Value { One }",
        "#test const VALUE: Int32 = 1",
        "impl Int32 { #test fn f() {} }",
        "fn f() @test {}",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}
