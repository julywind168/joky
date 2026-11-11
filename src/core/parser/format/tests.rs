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
        "fn main() {\n    var x = 1 + 2\n    if x == 3 {\n        println(x)\n        x = x * -2\n    }\n}\n",
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
        "import joky/array.{type Array}\n\nfn f(x: Array<Array<Int32>>) {\n    let y = 4 / 2\n}\n",
    );
}

#[test]
fn format_preserves_crlf_and_blank_lines_without_trailing_code_whitespace() {
    assert_format(
        "// comment\r\nfn f(){  \r\n  \r\nlet x=1  \r\n}\r\n  ",
        "// comment\r\nfn f() {\r\n    let x = 1\r\n}\r\n",
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
        "import missing/module\n\npub fn f() {\n    unknown()\n}\n",
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
        "fn f(var value: Int32) -> Int32 {\n    value += 1\n    value\n}\n",
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
        "import joky/array.{type Array}\n\nfn f<T: Copy>(x: Array<Array<T>>, a: Int32, b: Int32) -> Bool {\n    a < b || a > b || a / b == a >> b\n}\n",
    );
    assert_format(
        "type Item<T> = < T as Iterator > :: Item\nfn f<T>(p:& <T as Iterator>::Item){let x:Option<Int32>=None::<Int32>}\n",
        "type Item<T> = <T as Iterator>::Item\n\nfn f<T>(p: &<T as Iterator>::Item) {\n    let x: Option<Int32> = None::<Int32>\n}\n",
    );
}

#[test]
fn format_distinguishes_closure_delimiters_borrows_and_binary_operators() {
    assert_format(
        "fn f(x:&Int32){let half=|value:Int32|{value/2};let zero=move||{0};let y=1|2;let p = & *x}\n",
        "fn f(x: &Int32) {\n    let half = |value: Int32| {\n        value / 2\n    }\n    let zero = move || {\n        0\n    }\n    let y = 1 | 2\n    let p = &*x\n}\n",
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
        "fn f() -> Int32 {\n    if 10 >\n        2 {\n        call()\n    }\n    42\n}\n",
    );
}

#[test]
fn format_expands_blocks_and_preserves_branch_sequence_boundaries() {
    assert_format(
        "fn f(value:Bool)->Int32{match value{true=>1;2,false=>3}}\nfn empty(){\n}\n",
        "fn f(value: Bool) -> Int32 {\n    match value {\n        true => 1; 2,\n        false => 3\n    }\n}\n\nfn empty() {}\n",
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
        "fn f() {\n    first() /* why */\n    second() // trailing  \n}\n",
    );
    assert_format(
        "\u{feff}fn f(){let value=1;value;}\r\n",
        "\u{feff}fn f() {\r\n    let value = 1\r\n    value;\r\n}\r\n",
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
        "#repr(C, align: -1) struct Point { x: Int32 }\n\nfn f(value: Int32) -> Int32 {\n    match value {\n        -2 ..= -1 => 0,\n        _ => 1\n    }\n}\n",
    );
}

#[test]
fn format_preserves_byte_literals_and_range_patterns() {
    assert_format(
        r"fn f(n:UInt8)->UInt8{match n{b'a'..=b'z'=>b'\xAf',_=>b'\0'}}",
        "fn f(n: UInt8) -> UInt8 {\n    match n {\n        b'a' ..= b'z' => b'\\xAf',\n        _ => b'\\0'\n    }\n}\n",
    );
}

#[test]
fn format_open_slice_bounds() {
    assert_format(
        "fn f() { let a = [1, 2, 3]; let full = &a[ .. ]; let prefix = &a[ .. 1 ]; let tail = &a[ 1 .. ] }",
        "fn f() {\n    let a = [1, 2, 3]\n    let full = &a[..]\n    let prefix = &a[..1]\n    let tail = &a[1..]\n}\n",
    );
}

#[test]
fn format_match_alternatives_and_bitwise_or() {
    assert_format(
        "fn f(n:Int32)->Int32{match n{-1|2=>16|32,_=>0}}",
        "fn f(n: Int32) -> Int32 {\n    match n {\n        -1 | 2 => 16 | 32,\n        _ => 0\n    }\n}\n",
    );
}

#[test]
fn format_wraps_long_lists_records_and_signatures() {
    let source = concat!(
        "struct Record{first_field_name:Int32,second_field_name:Int32,third_field_name:Int32,fourth_field_name:Int32,fifth_field_name:Int32}\n",
        "struct Header{a_very_long_field_name_that_already_uses_most_of_the_available_horizontal_space:AnEquallyLongNominalTypeNameForTheField}\n",
        "fn calculate(first_argument:Int32,second_argument:Int32,third_argument:Int32,fourth_argument:Int32,fifth_argument:Int32)->Int32{",
        "first_argument+second_argument+third_argument+fourth_argument+fifth_argument}\n",
        "fn f(){call(first_argument_with_a_long_name,second_argument_with_a_long_name,third_argument_with_a_long_name);",
        "let values=[1111111111,2222222222,3333333333,4444444444,5555555555,6666666666,7777777777,8888888888,9999999999];",
        "let record=Record{first_field_name:11111111,second_field_name:22222222,third_field_name:33333333,fourth_field_name:44444444,fifth_field_name:55555555}}\n",
    );
    let output = format(source).unwrap();
    assert!(output.contains("fn calculate(\n"), "{output}");
    assert!(output.contains("call(\n"), "{output}");
    assert!(output.contains("let values = [\n"), "{output}");
    assert!(output.contains("let record = Record {\n"), "{output}");
    for line in output.lines() {
        assert!(
            line.chars().count() <= 100,
            "{}: {line}",
            line.chars().count()
        );
    }
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_wraps_parameters_before_short_return_types() {
    let expected = concat!(
        "impl Request {\n",
        "    pub fn header_unique(\n",
        "        &self,\n",
        "        name: &String\n",
        "    ) @noescape(name) -> Result<Option<&String>, DuplicateHeader> {\n",
        "        None\n",
        "    }\n",
        "}\n",
    );
    for signature in [
        "pub fn header_unique(&self, name: &String) @noescape(name) -> Result<Option<&String>, DuplicateHeader>",
        "pub fn header_unique(&self, name: &String) @noescape(name) -> Result<\nOption<&String>,\nDuplicateHeader\n>",
        "pub fn header_unique(&self, name: &String) @noescape(name) ->\nResult<Option<&String>, DuplicateHeader>",
    ] {
        assert_format(&format!("impl Request {{ {signature} {{ None }} }}"), expected);
    }
    assert_format(
        "fn short(value: Int32) -> Result<\nInt32,\nError\n> { Ok(value) }",
        "fn short(value: Int32) -> Result<Int32, Error> {\n    Ok(value)\n}\n",
    );
}

#[test]
fn format_plans_generic_trait_and_extern_signatures() {
    for source in [
        "fn transform<T, F: FnOnce(T) -> T>(transform: F, value: T) @noescape(transform) -> Result<Option<T>, TransformationError> { None }",
        "trait Headers { fn find_unique_named_header(&self, name: &String) @noescape(name) -> Result<Option<&String>, DuplicateHeader> }",
        "extern fn find_unique_named_header(source: &String, name: &String) @suspends -> Result<Option<String>, DuplicateHeader>",
    ] {
        let output = format(source).unwrap();
        assert!(output.contains("(\n"), "{output}");
        assert!(!output.contains("Result<\n"), "{output}");
        assert_eq!(format(&output).unwrap(), output);
        assert!(output.lines().all(|line| line.chars().count() <= 100), "{output}");
    }
}

#[test]
fn format_splits_only_return_types_that_cannot_fit_a_continuation() {
    let output = format(
        "fn a_very_long_function_name_that_leaves_little_space_for_its_return_type() -> Result<Option<String>, DuplicateHeader> { None }",
    ).unwrap();
    assert!(
        output.contains("->\n    Result<Option<String>, DuplicateHeader> {"),
        "{output}"
    );
    assert_eq!(format(&output).unwrap(), output);
    let output = format(
        "fn elaborate(value: Int32) -> Result<AnExtremelyLongSuccessTypeNameThatCannotFitBesideTheErrorType, AnEquallyLongErrorTypeNameThatRequiresAnotherLine> { None }",
    ).unwrap();
    assert!(output.contains("fn elaborate(\n"), "{output}");
    assert!(output.contains("Result<\n"), "{output}");
    assert!(
        output.lines().all(|line| line.chars().count() <= 100),
        "{output}"
    );
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_preserves_signature_comments_and_disabled_layouts() {
    let source = concat!(
        "fn short(value: Int32) -> Result<\n",
        "// Keep the error explanation with its type.\n",
        "Int32, Error> { Ok(value) }\n",
        "// fmt: off\n",
        "fn disabled(value:Int32)->Result<\nInt32,Error\n>{Ok(value)}\n",
        "// fmt: on\n",
    );
    let output = format(source).unwrap();
    assert!(
        output.contains("Result<\n    // Keep the error explanation with its type.\n"),
        "{output}"
    );
    assert!(
        output.contains("fn disabled(value:Int32)->Result<\nInt32,Error\n>{Ok(value)}"),
        "{output}"
    );
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_wraps_operators_chains_and_nested_closure_arguments_stably() {
    let source = concat!(
        "fn f(){let total=first_very_long_operand_name+second_very_long_operand_name+third_very_long_operand_name+fourth_very_long_operand_name;",
        "let result=receiver.first_long_method_name().second_long_method_name().third_long_method_name().fourth_long_method_name();",
        "call(||{42},first_very_long_operand_name+second_very_long_operand_name+third_very_long_operand_name+fourth_very_long_operand_name)}\n",
    );
    let output = format(source).unwrap();
    assert!(output.contains("+\n"), "{output}");
    assert!(output.contains("\n        ."), "{output}");
    for line in output.lines() {
        assert!(line.chars().count() <= 100, "{line}");
    }
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_preserves_disabled_regions_including_bom_and_line_endings() {
    let kept = "  // fmt: off\n let   deliberately_spaced=1;second(  deliberately_spaced )\n  // fmt: on\n";
    let other = "// fmt: off\nfn   another( ){  }\n// fmt: on\n";
    let source = format!("fn f(){{\n{kept}let active=2\n}}\n{other}");
    let output = format(&source).unwrap();
    assert!(output.contains(kept), "{output}");
    assert!(output.contains(other), "{output}");
    assert!(output.contains("\n    let active = 2\n"));
    assert_eq!(format(&output).unwrap(), output);
    let source = "\u{feff}// fmt: off\r\nfn f( ) {   }\r\n// fmt: on";
    assert_format(source, source);
    let source = "fn f(){call(\n// fmt: off\n  x , y,\n// fmt: on\nz)}\n";
    let output = format(source).unwrap();
    assert!(output.contains("// fmt: off\n  x , y,\n// fmt: on\n"));
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_directives_require_standalone_balanced_comments() {
    for source in [
        "// fmt: off\nfn f(){}\n",
        "// fmt: on\nfn f(){}\n",
        "// fmt: off\n// fmt: off\n// fmt: on\nfn f(){}\n",
    ] {
        assert!(format(source).unwrap_err().message.contains("fmt:"));
    }
    let source = "fn f(){let text=\"// fmt: off\"\n/* // fmt: on */\ncall() // fmt: off\n}\n";
    let output = format(source).unwrap();
    assert!(output.contains("let text = \"// fmt: off\""));
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_normalizes_blank_lines_and_treats_width_as_a_soft_limit() {
    assert_format(
        "\n\nfn a(){}\n\n\n\nfn b(){}\n\n\n",
        "fn a() {}\n\nfn b() {}\n",
    );
    assert_format("\r\n \r\n", "");
    let literal = format!("\"{}\"", "long literal ".repeat(20));
    let source = format!("fn f(){{println({literal})}}\n");
    let output = format(&source).unwrap();
    assert!(output.contains(&literal));
    assert!(output.contains("println(\n"), "{output}");
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_separates_binding_keywords_from_tuple_and_array_patterns() {
    assert_format(
        "fn f(){let(x,y)=(1,2);var[a,b]=[3,4];for(key,value) in entries{call(key,value)}}\n",
        "fn f() {\n    let (x, y) = (1, 2)\n    var [a, b] = [3, 4]\n    for (key, value) in entries {\n        call(key, value)\n    }\n}\n",
    );
}

#[test]
fn format_removes_only_proven_non_tail_statement_separators() {
    assert_format(
        "fn fail(error:String)->Int32{eprintln(error);return 1}\n",
        "fn fail(error: String) -> Int32 {\n    eprintln(error)\n    return 1\n}\n",
    );
    assert_format(
        "fn f(){let x=1;;;var y=2;y=x;print(y);(42)}\n",
        "fn f() {\n    let x = 1\n    var y = 2\n    y = x\n    print(y)\n    (42)\n}\n",
    );
    let source = "fn f(){call();/* trailing */next(); // keep\nlast()}\n";
    let output = format(source).unwrap();
    assert!(output.contains("call() /* trailing */\n"), "{output}");
    assert!(output.contains("next() // keep\n"), "{output}");
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_keeps_discard_value_context_and_disabled_semicolons() {
    let source = concat!(
        "fn f(){42;call();next();}\n",
        "fn g(){(call();next());let x=(first();42)}\n",
        "fn h(value:Bool)->Int32{match value{true=>call();1,false=>2}}\n",
        "fn marked(){call()\n; // fmt: off\nnext()}\n",
        "// fmt: off\nfn kept(){call();next()}\n// fmt: on\n",
    );
    let output = format(source).unwrap();
    assert!(output.contains("42;\n"), "{output}");
    assert!(output.contains("next();\n"), "{output}");
    assert!(output.contains("(call(); next());"), "{output}");
    assert!(output.contains("(first(); 42)"), "{output}");
    assert!(output.contains("true => call(); 1,"), "{output}");
    assert!(output.contains("; // fmt: off"), "{output}");
    assert!(output.contains("fn kept(){call();next()}"), "{output}");
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_retains_required_separation_after_generic_trailing_commas() {
    assert_format(
        "fn f<T: Trait<Item = Int32,> >(x:&T){}",
        "fn f<T: Trait<Item = Int32,> >(x: &T) {}\n",
    );
    let source = include_str!("../../../../tests/fixtures/function_values.jk");
    let output = format(source).unwrap();
    assert_eq!(format(&output).unwrap(), output);
}

#[test]
fn format_expands_containing_calls_and_entire_wrapped_chains() {
    assert_format(
        "fn f(){outer(first(|x| {x+1}),second(|y| {y+2}))}",
        "fn f() {\n    outer(\n        first(\n            |x| {\n                x + 1\n            }\n        ),\n        second(\n            |y| {\n                y + 2\n            }\n        )\n    )\n}\n",
    );
    let comments = format("fn f(){outer(||{/* note */},||{})}").unwrap();
    assert!(comments.contains("outer(\n"), "{comments}");
    assert_eq!(format(&comments).unwrap(), comments);
    let source = "fn f(){let result = receiver.first_method_with_a_long_name().second_method_with_a_long_name().third_method_with_a_long_name().fourth()}";
    let output = format(source).unwrap();
    for name in [
        "first_method_with_a_long_name",
        "second_method_with_a_long_name",
        "third_method_with_a_long_name",
        "fourth",
    ] {
        assert!(output.contains(&format!("\n        .{name}()")), "{output}");
    }
    assert_eq!(format(&output).unwrap(), output);
    assert_format(
        "fn f(){receiver.short().other()}",
        "fn f() {\n    receiver.short().other()\n}\n",
    );
}

#[test]
fn format_normalizes_declaration_gaps_block_edges_and_else() {
    assert_format(
        "fn a(){\n\nif true {first()}\nelse {second()}\n\n} // trailing\n// next function\n#test\nfn b(){}",
        "fn a() {\n    if true {\n        first()\n    } else {\n        second()\n    }\n} // trailing\n\n// next function\n#test\nfn b() {}\n",
    );
    assert_format(
        "fn a(){if true {first()} // explain\nelse {second()}}",
        "fn a() {\n    if true {\n        first()\n    } // explain\n    else {\n        second()\n    }\n}\n",
    );
}

#[test]
fn format_reindents_only_conventional_block_comment_stars() {
    assert_format(
        "fn f(){\n/* details\n * example:\n *   indented code\n */\ncall()}\n",
        "fn f() {\n    /* details\n     * example:\n     *   indented code\n     */\n    call()\n}\n",
    );
    for comment in [
        "/* table\n  left  right\n   1      2\n */",
        "/* nested\n * /* inner */\n */",
    ] {
        let output = format(&format!("fn f(){{\n{comment}\ncall()}}")).unwrap();
        assert!(output.contains(comment), "{output}");
        assert_eq!(format(&output).unwrap(), output);
    }
    for ending in ["\n", "\r\n", "\r"] {
        let comment = format!("/* info{ending}\t* details  {ending} */");
        let source = format!("fn f(){{{ending}{comment}{ending}call()}}{ending}");
        let output = format(&source).unwrap();
        assert!(output.contains(&format!("/* info{ending}     * details  {ending}     */")));
        assert_eq!(format(&output).unwrap(), output);
    }
    let source = "// fmt: off\nfn f(){\n/* details\n * keep\n */\n}\n// fmt: on\n";
    assert_format(source, source);
    let source = "fn f(){\n/* info\n * same\n */\n}";
    let original = super::syntax::Syntax::parse(source).unwrap();
    assert!(super::verify(source, &original, &source.replace("same", "changed")).is_err());
}
