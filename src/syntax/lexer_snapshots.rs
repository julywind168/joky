//! Lexer snapshot tests

#[cfg(test)]
mod tests {
    use crate::syntax::lexer::lex;

    /// Formats the token list into a readable string
    fn format_tokens(source: &str) -> String {
        match lex(source) {
            Ok(tokens) => {
                let mut output = String::new();
                for token in tokens {
                    output.push_str(&format!("{:?}\n", token.kind));
                }
                output
            }
            Err(err) => format!("Error: {:?}", err),
        }
    }

    #[test]
    fn snapshot_basic_tokens() {
        let source = "42 3.14 \"hello\" true false";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_operators() {
        let source = "+ - * / == != < <= > >=";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_keywords() {
        let source = "fn struct class enum let var if match else while loop break";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_complex_expression() {
        let source = "let x = 42 + 3 * (10 - 5)";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_function_definition() {
        let source = r#"fn main() {
    let x: Int64 = 100
    println("hello")
}"#;
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_control_flow() {
        let source = r#"if x < 10 {
    y = 1
} else {
    y = 2
}"#;
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_loop_with_break() {
        let source = "loop { break 42 }";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_string_escapes() {
        let source = r#""hello\nworld\t\"quoted\"""#;
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_raw_strings() {
        let source = r###"r"\d+\.\d+" r#"{"k": "v"}"# r"{not interpolation}""###;
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_multiline_strings() {
        let source = "\"\"\"\n    SELECT *\n      FROM t\n    \"\"\"\nr\"\"\"\n    path ~ '\\d+'\n    \"\"\"";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_error_multiline_indent_mismatch() {
        let source = "\"\"\"\n    deep\nshallow\n    \"\"\"";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_comments() {
        let source = r#"// line comment
42 /* block comment */ + 3
/// doc comment"#;
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_error_unterminated_string() {
        let source = r#""unterminated"#;
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_error_invalid_character() {
        let source = "42 $ 3";
        insta::assert_snapshot!(format_tokens(source));
    }

    #[test]
    fn snapshot_error_integer_overflow() {
        let source = "18446744073709551616"; // u64::MAX + 1
        insta::assert_snapshot!(format_tokens(source));
    }
}
