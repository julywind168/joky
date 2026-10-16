use super::*;

fn literal(source: &str, expected: &str) {
    let tokens = Lexer::tokenize(source).unwrap();
    assert_eq!(
        tokens[0].kind,
        TokenKind::StrLit(expected.into()),
        "{source}"
    );
    assert_eq!(tokens[0].span.slice(source), Some(source));
    assert_eq!(tokens[1].kind, TokenKind::Eof);
}

#[test]
fn raw_and_multiline_string_values() {
    for (source, expected) in [
        (r###"r"""###, ""),
        (
            r###"r"\d+\.\d+ {x} } {{ C:\Users\windy\""###,
            r"\d+\.\d+ {x} } {{ C:\Users\windy\",
        ),
        (r###"r#"{"k": "v"}"#"###, r#"{"k": "v"}"#),
        (r###"r##"ends with "#"##"###, "ends with \"#"),
        (
            "\"\"\"\n    SELECT *\n      FROM t\n    \"\"\"",
            "SELECT *\n  FROM t",
        ),
        ("\"\"\" \t\n    \"\"\"", ""),
        ("\"\"\"\n    line\n\n    \"\"\"", "line\n"),
        ("\"\"\"\n\n    line\n    \"\"\"", "\nline"),
        ("\"\"\"\n    a\n \t\n    b\n    \"\"\"", "a\n\nb"),
        ("\"\"\"\n\t a\n\t   b\n\t \"\"\"", "a\n  b"),
        ("\"\"\"\n  a\n\"\"\"", "  a"),
        ("\"\"\"\r\n  a\r\n  b\r  \"\"\"", "a\nb"),
        (
            "\"\"\"\n  中\\n🙂\\t\\0\\u{41}\\r\n  \"\"\"",
            "中\n🙂\t\0A\r",
        ),
        (
            "\"\"\"\n  says \"hi\" and \"\"\n  \"\"\"",
            "says \"hi\" and \"\"",
        ),
        (
            "r\"\"\"\n  path ~ '\\d+' {x}\n  \"\"\"",
            r"path ~ '\d+' {x}",
        ),
        (
            "r#\"\"\"\n  says \"\"\" inside\n  \"\"\"#",
            "says \"\"\" inside",
        ),
        ("r\"\"\"\r\n  a\\n\r  {b}\r\n  \"\"\"", "a\\n\n{b}"),
    ] {
        literal(source, expected);
    }
    for hashes in [1, 2, 16] {
        let marks = "#".repeat(hashes);
        literal(
            &format!("r{marks}\"quoted \" and \\ {{}}\"{marks}"),
            "quoted \" and \\ {}",
        );
    }
    let tokens = Lexer::tokenize("r + rank r#(1)").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Ident("r".into()));
    assert_eq!(tokens[2].kind, TokenKind::Ident("rank".into()));
    assert_eq!(tokens[3].kind, TokenKind::Ident("r".into()));
}

#[test]
fn raw_and_multiline_string_errors_keep_source_locations() {
    for (source, message, line, fragment) in [
        ("r#\"open", "Unterminated string", 1, "r#\"open"),
        ("r##\"open\"#", "Unterminated string", 1, "r##\"open\"#"),
        ("r\"a\nb\"", "Line break", 1, "\n"),
        ("\"\"\"oops\n\"\"\"", "must start", 1, "\"\"\"o"),
        ("\"\"\"\n  text\"\"\"", "own line", 2, "  text"),
        (
            "\"\"\"\n    deep\nshallow\n    \"\"\"",
            "indentation",
            3,
            "shallow",
        ),
        ("\"\"\"\n    a\n\t\"\"\"", "indentation", 2, "    a"),
        ("\"\"\"\n  a\n", "Unterminated string", 1, "\"\"\"\n  a\n"),
        (
            "r#\"\"\"\n  a\n  \"\"\"",
            "Unterminated string",
            1,
            "r#\"\"\"\n  a\n  \"\"\"",
        ),
        (
            "\"\"\"\n  🙂\\q\n  \"\"\"",
            "Unknown string escape",
            2,
            "\\q",
        ),
        (
            "\"\"\"\n  🙂\\u{D800}\n  \"\"\"",
            "Invalid Unicode",
            2,
            "\\u{D800}",
        ),
        (
            "\"\"\"\n  a\\\n  b\n  \"\"\"",
            "Line continuation",
            2,
            "\\\n",
        ),
    ] {
        let error = Lexer::tokenize(source).unwrap_err();
        assert!(error.message.contains(message), "{source}: {error}");
        assert_eq!(error.line, line, "{source}");
        assert_eq!(error.span.slice(source), Some(fragment), "{source}");
    }
}

#[test]
fn multiline_interpolation_preserves_holes_and_following_token_lines() {
    let source = "\"\"\"\n    中\\n{value}\n      {r\"{raw}\"}\n    \"\"\"\nnext";
    let tokens = Lexer::tokenize(source).unwrap();
    assert_eq!((tokens[0].line, tokens[0].end_line), (1, 4));
    assert_eq!(tokens[1].line, 5);
    let TokenKind::Interpolated(parts) = &tokens[0].kind else {
        panic!("Expected interpolation");
    };
    assert!(matches!(&parts[0], InterpolationPart::Text(text, _) if text == "中\n"));
    let InterpolationPart::Expression(hole) = &parts[1] else {
        panic!("Expected hole");
    };
    assert_eq!(hole[0].span.slice(source), Some("value"));
    assert_eq!(hole[0].line, 2);
    assert!(matches!(&parts[2], InterpolationPart::Text(text, _) if text == "\n  "));
    let InterpolationPart::Expression(hole) = &parts[3] else {
        panic!("Expected hole");
    };
    assert_eq!(hole[0].kind, TokenKind::StrLit("{raw}".into()));
    assert_eq!(hole[0].span.slice(source), Some("r\"{raw}\""));
    assert_eq!(hole[0].line, 3);
    let tokens = Lexer::tokenize("\"\"\"\r  a\r  \"\"\"\nnext").unwrap();
    assert_eq!(tokens[0].end_line, 3);
    assert_eq!(tokens[1].line, 4);
}
