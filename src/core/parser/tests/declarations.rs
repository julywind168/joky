use super::*;

#[test]
fn malformed_aliases_and_non_top_level_aliases_are_rejected() {
    for (source, marked, message) in [
        ("type = Int32", "=", "Expected an identifier"),
        ("type Count Int32", "Int32", "Expected `=`"),
        ("type Count =", "", "Expected a type"),
        ("type Count = 1", "1", "Expected a type"),
        ("type Int32 = Bool", "Int32", "Expected an identifier"),
        ("type Count.Value = Int32", ".", "Expected `=`"),
        ("type Count<T> Int32", "Int32", "Expected `=`"),
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
    let source = "#test\nfn checks_value() {}\n\n#test\npub fn suspends() @suspends {}\n";
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
