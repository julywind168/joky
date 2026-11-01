use super::format;

fn assert_format(source: &str, expected: &str) {
    let formatted = format(source).unwrap();
    assert_eq!(formatted, expected);
    assert_eq!(
        format(&formatted).unwrap(),
        formatted,
        "formatting must be idempotent"
    );
}

#[test]
fn format_normalizes_indentation_and_horizontal_spacing() {
    assert_format(
        "fn  main( ) {\n\tvar x=1+2\n if x==3{\nprintln( x );x=x*-2\n }\n}\n",
        "fn main() {\n    var x = 1 + 2\n    if x == 3 {\n        println(x); x = x * -2\n    }\n}\n",
    );
    assert_format(
        "fn f(\nx: Int32,\ny: Int32,\n) -> Int32 {x+y}",
        "fn f(\n    x: Int32,\n    y: Int32,\n) -> Int32 { x + y }\n",
    );
}

#[test]
fn format_preserves_comments_and_literal_bytes() {
    let source = concat!(
        "// heading  \nfn main(){\n",
        "/* outer\n  /* nested */  body\n */let n=7 // trailing  \n",
        "let text=\"\"\"\n  n={n}\n  \"\"\"\n",
        "let raw=r##\"\"\"\n  {literal} \\\\ \"#\n  \"\"\"##\n",
        "let nested=\"{ if true { \"{n}\" } else { r#\"}\"# } }\"\n",
        "let rune='\\u{4e2d}'\n}\n",
    );
    let output = format(source).unwrap();
    for text in [
        "// heading  ",
        "/* outer\n  /* nested */  body\n */",
        "// trailing  ",
        "\"\"\"\n  n={n}\n  \"\"\"",
        "r##\"\"\"\n  {literal} \\\\ \"#\n  \"\"\"##",
        "\"{ if true { \"{n}\" } else { r#\"}\"# } }\"",
        "'\\u{4e2d}'",
    ] {
        assert!(output.contains(text), "Missing original text: {text:?}");
    }
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_retains_expression_lines_and_ambiguous_token_separation() {
    assert_format(
        "fn f() {\nlet x=5\n-2\nlet y = & &x\nlet z = 1 . 0\n}\n",
        "fn f() {\n    let x = 5\n    -2\n    let y = & &x\n    let z = 1 .0\n}\n",
    );
    assert_format(
        "import joky/array.{type Array}\nfn f(x: Array<Array<Int32> >) {let y = 4/2}\n",
        "import joky/array.{ type Array }\nfn f(x: Array<Array<Int32> >) { let y = 4/2 }\n",
    );
}

#[test]
fn format_preserves_crlf_and_blank_lines_without_trailing_code_whitespace() {
    assert_format(
        "// comment\r\nfn f(){  \r\n  \r\nlet x=1  \r\n}\r\n  ",
        "// comment\r\nfn f() {\r\n\r\n    let x = 1\r\n}\r\n",
    );
    assert_format("", "");
    assert_format(" \t ", "");
    assert_format("/* only a comment */", "/* only a comment */\n");
    assert_format("// comment", "// comment\n");
}

#[test]
fn format_requires_syntax_but_not_import_resolution_or_a_main_function() {
    assert_format(
        "import missing/module\npub fn f(){unknown()}",
        "import missing/module\npub fn f() { unknown() }\n",
    );
    for source in [
        "fn broken(",
        "fn f() { let x = }",
        "/* unfinished",
        "fn f() { let x = r#\"open }",
    ] {
        assert!(format(source).is_err(), "{source}");
    }
}

#[test]
fn format_examples_and_standard_modules_are_idempotent() {
    fn visit(path: &std::path::Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path);
            } else if path.extension().is_some_and(|extension| extension == "jk") {
                let source = std::fs::read_to_string(&path).unwrap();
                let formatted =
                    format(&source).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                assert_eq!(format(&formatted).unwrap(), formatted, "{}", path.display());
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    visit(&root.join("examples"));
    visit(&root.join("src/stdlib"));
}

#[test]
fn format_mutable_parameters() {
    assert_format(
        "fn f(var  value:Int32)->Int32{value+=1;value}",
        "fn f(var value: Int32) -> Int32 { value += 1; value }\n",
    );
}

#[test]
fn duration_literals_preserve_spelling_and_member_boundaries() {
    assert_format(
        "fn f(){let d=1_000.50ms\nlet n=250us.as_nanos()\nlet text=\"{1.5s}\"}",
        "fn f() { let d = 1_000.50ms\n    let n = 250us.as_nanos()\n    let text = \"{1.5s}\" }\n",
    );
}
