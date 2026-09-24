//! Parser snapshot tests

#[cfg(test)]
mod tests {
    use crate::syntax::parse_program;

    /// Formats the AST into a readable string
    fn format_ast(source: &str) -> String {
        match parse_program(source) {
            Ok(program) => format!("{:#?}", program),
            Err(err) => format!("Error: {}", err),
        }
    }

    #[test]
    fn snapshot_simple_expression() {
        let source = "fn main() { 42 }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_arithmetic_expression() {
        let source = "fn main() { 1 + 2 * 3 }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_comparison_expression() {
        let source = "fn main() { x < 10 }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_let_binding() {
        let source = "fn main() { let x = 42; x }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_let_with_type_annotation() {
        let source = "fn main() { let x: Int64 = 42; x }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_if_expression() {
        let source = "fn main() { if true 1 else 2 }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_if_with_blocks() {
        let source = r#"fn main() {
    if x < 10 {
        1
    } else {
        2
    }
}"#;
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_while_loop() {
        let source = r#"fn main() {
    while true {
        break
    }
}"#;
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_loop_with_break() {
        let source = "fn main() { loop { break 42 } }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_nested_blocks() {
        let source = "fn main() { { { 42 } } }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_function_call() {
        let source = r#"fn main() { println("hello world") }"#;
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_struct_values() {
        let source = r#"struct Point { let x: Int32, let y: Int32 }
fn main() { Point(y: 2, x: 1).x }"#;
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_complex_expression() {
        let source = r#"fn main() {
    let x = (1 + 2) * 3
    let y = if x > 5 { x } else { 0 }
    y
}"#;
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_all_literal_types() {
        let source = r#"fn main() {
    42
    3.14
    "hello"
    true
    false
}"#;
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_unary_negation() {
        let source = "fn main() { -42 }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_parenthesized_expression() {
        let source = "fn main() { (1 + 2) * 3 }";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_error_empty_program() {
        let source = "";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_error_unclosed_brace() {
        let source = "fn main() { let x = 1";
        insta::assert_snapshot!(format_ast(source));
    }

    #[test]
    fn snapshot_error_missing_function_body() {
        let source = "fn main()";
        insta::assert_snapshot!(format_ast(source));
    }
}
