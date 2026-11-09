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
    assert_format("fn f(){Ok(( ))}", "fn f() {\n    Ok(())\n}\n");
    assert_format(
        "fn  main( ) {\n\tvar x=1+2\n if x==3{\nprintln( x );x=x*-2\n }\n}\n",
        "fn main() {\n    var x = 1 + 2\n    if x == 3 {\n        println(x);\n        x = x * -2\n    }\n}\n",
    );
    assert_format(
        "fn f(\nx: Int32,\ny: Int32,\n) -> Int32 {x+y}",
        "fn f(\n    x: Int32,\n    y: Int32,\n) -> Int32 {\n    x + y\n}\n",
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
        "import joky/array.{type Array}\nfn f(x: Array<Array<Int32>>) {\n    let y = 4 / 2\n}\n",
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
        "import missing/module\npub fn f() {\n    unknown()\n}\n",
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
        "fn f(var value: Int32) -> Int32 {\n    value += 1;\n    value\n}\n",
    );
}

#[test]
fn duration_literals_preserve_spelling_and_member_boundaries() {
    assert_format(
        "fn f(){let d=1_000.50ms\nlet n=250us.as_nanos()\nlet text=\"{1.5s}\"}",
        "fn f() {\n    let d = 1_000.50ms\n    let n = 250us.as_nanos()\n    let text = \"{1.5s}\"\n}\n",
    );
}

#[test]
fn format_distinguishes_type_angles_from_comparisons_and_shifts() {
    assert_format(
        "import joky / array . {type Array}\nfn f < T : Copy > (x:Array < Array < T > >,a:Int32,b:Int32)->Bool{a<b || a>b || a/b==a>>b}\n",
        "import joky/array.{type Array}\nfn f<T: Copy>(x: Array<Array<T>>, a: Int32, b: Int32) -> Bool {\n    a < b || a > b || a / b == a >> b\n}\n",
    );
    assert_format(
        "type Item<T> = < T as Iterator > :: Item\nfn f<T>(p:& <T as Iterator>::Item){let x:Option<Int32>=None::<Int32>}\n",
        "type Item<T> = <T as Iterator>::Item\nfn f<T>(p: &<T as Iterator>::Item) {\n    let x: Option<Int32> = None::<Int32>\n}\n",
    );
}

#[test]
fn format_distinguishes_closure_delimiters_borrows_and_binary_operators() {
    assert_format(
        "fn f(x:&Int32){let half=|value:Int32|{value/2};let zero=move||{0};let y=1|2;let p = & *x}\n",
        "fn f(x: &Int32) {\n    let half = |value: Int32| {\n        value / 2\n    };\n    let zero = move || {\n        0\n    };\n    let y = 1 | 2;\n    let p = &*x\n}\n",
    );
}

#[test]
fn format_indents_expression_continuations_and_method_chains() {
    assert_format(
        "fn f(){let value=10+\n20\nlet size=\"abc\"\n.byte_length()\ncall(\nvalue,\nsize,\n)\n}\n",
        "fn f() {\n    let value = 10 +\n        20\n    let size = \"abc\"\n        .byte_length()\n    call(\n        value,\n        size,\n    )\n}\n",
    );
    assert_format(
        "fn f()->\nInt32{if 10>\n2{call()}\n42}\n",
        "fn f() ->\n    Int32 {\n    if 10 >\n        2 {\n        call()\n    }\n    42\n}\n",
    );
}

#[test]
fn format_expands_blocks_and_preserves_branch_sequence_boundaries() {
    assert_format(
        "fn f(value:Bool)->Int32{match value{true=>1;2,false=>3}}\nfn empty(){\n}\n",
        "fn f(value: Bool) -> Int32 {\n    match value {\n        true => 1; 2,\n        false => 3\n    }\n}\nfn empty() {}\n",
    );
    assert_format(
        "struct Point{x:Int32,y:Int32}\nimpl Point{fn sum(&self)->Int32{self.x+self.y}}\n",
        "struct Point { x: Int32, y: Int32 }\nimpl Point {\n    fn sum(&self) -> Int32 {\n        self.x + self.y\n    }\n}\n",
    );
}

#[test]
fn format_keeps_trailing_comments_with_their_original_tokens() {
    assert_format(
        "fn f(){first();/* why */second() // trailing  \n}\n",
        "fn f() {\n    first(); /* why */\n    second() // trailing  \n}\n",
    );
    assert_format(
        "\u{feff}fn f(){let value=1;value;}\r\n",
        "\u{feff}fn f() {\r\n    let value = 1;\r\n    value;\r\n}\r\n",
    );
}

#[test]
fn format_guard_rejects_parseable_changes_to_expression_boundaries() {
    for (source, changed) in [
        ("fn f(){return\n42}", "fn f(){return 42}"),
        ("fn f(){f\n(42)}", "fn f(){f(42)}"),
        ("fn f(){let x=1\n-2}", "fn f(){let x=1-2}"),
        ("fn f(){foo.method\n(42)}", "fn f(){foo.method(42)}"),
    ] {
        let original = super::syntax::Syntax::parse(source).unwrap();
        super::syntax::Syntax::parse(changed).unwrap();
        assert!(
            super::verify(source, &original, changed).is_err(),
            "{source:?} -> {changed:?}"
        );
        let formatted = format(source).unwrap();
        assert_eq!(format(&formatted).unwrap(), formatted);
    }
}

#[test]
fn format_keeps_negative_patterns_and_attribute_literals_together() {
    assert_format(
        "#repr(C, align: - 1) struct Point{x:Int32}\nfn f(value:Int32)->Int32{match value{-2..=-1=>0,_=>1}}\n",
        "#repr(C, align: -1) struct Point { x: Int32 }\nfn f(value: Int32) -> Int32 {\n    match value {\n        -2 ..= -1 => 0,\n        _ => 1\n    }\n}\n",
    );
}
