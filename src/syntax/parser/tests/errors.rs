use super::super::*;

#[test]
fn parser_rejects_empty_program() {
    let error = parse_program("").unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[test]
fn parser_rejects_missing_function_body() {
    let error = parse_program("fn main()").unwrap_err();
    assert!(error.to_string().contains("expected"));
}

#[test]
fn parser_rejects_unclosed_parentheses() {
    let error = parse_program("fn main() { (1 + 2 }").unwrap_err();
    assert!(error.to_string().contains("')'"));
}

#[test]
fn parser_rejects_unclosed_braces() {
    let error = parse_program("fn main() { let x = 1").unwrap_err();
    assert!(error.to_string().contains("'}'"));
}

#[test]
fn parser_rejects_removed_fixed_array_syntax() {
    for source in [
        "fn main() { let xs: [Int32; 2] = List(1, 2) }",
        "fn main() { [1, 2] }",
        "fn main() { let xs = List(1, 2); xs[0] }",
    ] {
        assert!(parse_program(source).is_err(), "accepted: {source}");
    }
}

#[test]
fn parser_rejects_legacy_generic_syntax() {
    for source in [
        "fn identity<T>(value: T) -> T { value } fn main() {}",
        "fn main() { let value: List<Int32> = List(1) }",
        "fn main() { let value = \"1\".parse<Int32>() }",
        "pub type List<T> fn main() {}",
        "@intrinsic class MutList<T> { fn length(&self) -> UInt64 } fn main() {}",
    ] {
        assert!(
            parse_program(source).is_err(),
            "accepted legacy syntax: {source}"
        );
    }
}

#[test]
fn parser_rejects_expression_nesting_beyond_the_defined_limit() {
    let mut body = String::from("1");
    for _ in 0..=crate::syntax::MAX_EXPRESSION_NESTING {
        body = format!("if true {{ {body} }} else {{ 0 }}");
    }
    let error = parse_program(&format!("fn main() {{ {body} }}")).unwrap_err();
    assert!(
        error.to_string().contains("expression nesting exceeds"),
        "{error}"
    );
}

#[test]
fn parser_accepts_wide_operator_chains_and_moderate_if_nesting() {
    let sum = (0..128)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" + ");
    parse_program(&format!("fn main() {{ {sum} }}")).expect("left-associated + chain");

    let mut body = String::from("1");
    for _ in 0..16 {
        body = format!("if true {{ {body} }} else {{ 0 }}");
    }
    parse_program(&format!("fn main() {{ {body} }}")).expect("16 nested ifs");
}

#[test]
fn parser_rejects_a_typed_self_parameter() {
    for source in [
        "fn f(self: Int32) {} fn main() {}",
        "trait Show { fn show(self: String) -> String }",
    ] {
        let error = parse_program(source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("the 'self' receiver cannot have a type annotation"),
            "{source}: {error}"
        );
    }
}
