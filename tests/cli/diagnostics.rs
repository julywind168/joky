//! Source diagnostics and CLI route consistency.
use super::*;

#[test]
fn user_file_diagnostics_name_the_dependency_and_full_cycle() {
    let work = Workspace::new();
    work.write(
        "main.jk",
        "import app/bad fn main() -> Int32 { bad.value() }",
    );
    for dependency in [
        "// λ 中文\npub fn value() -> Int32 { missing() }",
        "// λ 中文\npub fn value() -> Int32 { @ }",
        "// λ 中文\npub fn value() -> Missing { 0 }",
    ] {
        work.write("app/bad.jk", dependency);
        for args in [
            vec!["run", "main.jk"],
            vec!["run", "--interpreter", "main.jk"],
            vec!["build", "main.jk", "-o", "program"],
            vec!["check", "main.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "app/bad.jk:2");
            assert!(String::from_utf8_lossy(&output.stderr).contains("pub fn value()"));
        }
    }
    work.write(
        "app/bad.jk",
        "import app/next pub fn value() -> Int32 { 0 }",
    );
    let output = work.cli(&["run", "main.jk"]);
    failure(&output, "app/next.jk");
    failure(&output, "app/bad.jk:1");
    work.write("app/next.jk", "import app/last");
    work.write("app/last.jk", "import app/bad");
    failure(
        &work.cli(&["run", "main.jk"]),
        "app/bad -> app/next -> app/last -> app/bad",
    );
}

#[test]
fn strict_syntax_errors_match_all_frontends() {
    let work = Workspace::new();
    for (index, (source, message)) in [
        ("fn f(x: Int32 y: Int32) {}", "Expected ',' or ')'"),
        ("extern fn f(x: Int32 y: Int32)", "Expected ',' or ')'"),
        ("fn main() { f(1 2) }", "Expected ',' or ')'"),
        ("fn main() { f(1; 2 3) }", "Expected ',' or ')'"),
        ("fn main() {} import joky/vm", "Imports must precede"),
        ("extern fn f() import joky/vm", "Imports must precede"),
        (r#"fn main() { "hé\q" }"#, "Unknown string escape"),
        ("fn main() { \"hé\nnext\" }", "Line break in string literal"),
        (
            "fn main() { \"hé\r\nnext\" }",
            "Line break in string literal",
        ),
        (
            "fn main() { \"hé\\\nnext\" }",
            "Line continuation is not allowed",
        ),
        ("fn main() { \"hé", "Unterminated string literal"),
    ]
    .into_iter()
    .enumerate()
    {
        work.write("syntax.jk", &format!("// é🙂\n{source}"));
        // One case checks frontend parity; the parser is shared by all commands.
        for args in [
            vec!["run", "syntax.jk"],
            vec!["run", "--interpreter", "syntax.jk"],
            vec!["build", "syntax.jk"],
        ]
        .into_iter()
        .filter(|args| index == 0 || args[0] == "build")
        {
            let output = work.cli(&args);
            failure(&output, message);
            assert_eq!(output.status.code(), Some(1));
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("syntax.jk:2:"), "{error}");
            assert!(error.contains("Line 2"), "{error}");
        }
        assert!(!work.path().join("syntax").exists());
        work.assert_no_build_files();
    }
}

#[test]
fn invalid_assignment_diagnostic_marks_the_source_expression() {
    let work = Workspace::new();
    work.write("invalid.jk", "fn main() {\n    let x = 1\n    x = 2\n}\n");
    for args in [
        vec!["run", "invalid.jk"],
        vec!["run", "--interpreter", "invalid.jk"],
        vec!["build", "invalid.jk"],
    ] {
        let output = work.cli(&args);
        failure(&output, "Cannot assign to immutable binding `x`");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("invalid_assignment"), "{error}");
        assert!(error.contains("x = 2"), "{error}");
    }
}

#[test]
fn check_run_and_build_report_source_and_entry_errors_consistently() {
    let work = Workspace::new();
    for (source, message) in [
        ("fn helper() {}", "Missing entry point"),
        ("fn main(x: Int32) {}", "Entry point must be"),
        ("fn main() -> Bool { true }", "Entry point must be"),
        (
            "extern fn other() fn main() { other() }",
            "Unknown CLI host function",
        ),
        (
            "extern fn report_int32(x: Bool) fn main() {}",
            "Unknown CLI host function",
        ),
        ("fn main() {\n#\n}", "Line 2"),
        ("fn main() { let x: Bool = 1 }", "Type annotation mismatch"),
        (
            "fn main() { let x: Int32 = \"text\" }",
            "Type annotation mismatch",
        ),
        (
            "fn main() -> Int32 { true }",
            "Return type mismatch for `main`",
        ),
        (
            "fn __joky_cps_slots() {} fn main() {}",
            "Reserved runtime function",
        ),
    ] {
        work.write("bad.jk", source);
        for command in ["run", "build", "check"] {
            if command == "check" && message == "Missing entry point" {
                continue;
            }
            let output = work.cli(&[command, "bad.jk"]);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("bad.jk:"));
        }
        assert!(!work.path().join("bad").exists());
        work.assert_no_build_files();
    }
}

#[test]
fn parser_errors_render_ariadne_source_context() {
    let work = Workspace::new();
    work.write("bad.jk", "fn main() {\n$\n}\n");
    let output = work.cli(&["run", "bad.jk"]);
    failure(&output, "Unexpected character");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("bad.jk:2:1"), "{error}");
    assert!(error.contains("2 │ $"), "{error}");
    assert!(error.contains("Unexpected character '$'"), "{error}");
}

#[test]
fn parser_diagnostics_use_utf8_byte_spans() {
    let work = Workspace::new();
    work.write("unicode.jk", "fn main() { let x = \"🙂\" $ }\n");
    let output = work.cli(&["run", "unicode.jk"]);
    failure(&output, "Unexpected character");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("unicode.jk:1:25"), "{error}");
    assert!(
        error.contains("1 │ fn main() { let x = \"🙂\" $ }"),
        "{error}"
    );
}

#[test]
fn parser_diagnostics_at_eof_render_without_panicking() {
    let work = Workspace::new();
    work.write("eof.jk", "fn main() {\n");
    let output = work.cli(&["run", "eof.jk"]);
    failure(&output, "Expected `}`");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("eof.jk:1:11"), "{error}");
    assert!(error.contains("Line 1: Expected `}`"), "{error}");
}

#[test]
fn renamed_concurrent_constructs_report_join_migration() {
    let work = Workspace::new();
    work.write(
        "old_scope.jk",
        "fn wait() @suspends {}\nfn main() @suspends { scope { | wait() } }\n",
    );
    let output = work.cli(&["run", "old_scope.jk"]);
    failure(&output, "Use join for pipe-separated branches");

    work.write(
        "old_parallel.jk",
        "fn wait() @suspends {}\nfn main() @suspends { parallel { | wait() } }\n",
    );
    let output = work.cli(&["run", "old_parallel.jk"]);
    failure(&output, "Use join instead of parallel");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("old_parallel.jk:2:23"), "{error}");

    work.write(
        "new_scope.jk",
        "fn wait() @suspends {}\nfn main() @suspends { scope { wait() } }\n",
    );
    let output = work.cli(&["run", "new_scope.jk"]);
    success(&output);

    work.write("reserved.jk", "fn parallel() {}\n");
    let output = work.cli(&["run", "reserved.jk"]);
    failure(&output, "Expected an identifier, got Parallel");

    work.write(
        "reserved_branch.jk",
        "extern fn record(x: Int32) -> Int32\nfn main() { let branch = record(1) }\n",
    );
    let output = work.cli(&["run", "reserved_branch.jk"]);
    failure(&output, "Expected an identifier, got Branch");
}

#[test]
fn semantic_diagnostics_render_source_context_for_run_and_build() {
    let work = Workspace::new();
    work.write("type.jk", "fn main() { let x: Bool = 1 }\n");
    for command in ["run", "build"] {
        let output = work.cli(&[command, "type.jk"]);
        failure(&output, "Type annotation mismatch");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("type.jk:1:1"), "{error}");
        assert!(
            error.contains("1 │ fn main() { let x: Bool = 1 }"),
            "{error}"
        );
        // The Ariadne header carries the stable machine-readable error code.
        assert!(error.contains("[type_mismatch]"), "{error}");
    }
}

#[test]
fn resolver_diagnostics_render_source_context() {
    let work = Workspace::new();
    work.write(
        "resolve.jk",
        "import joky/vm fn main() { vm.call_int32_0() }\n",
    );
    let output = work.cli(&["run", "resolve.jk"]);
    failure(&output, "Argument count mismatch");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("resolve.jk:1:28"), "{error}");
    assert!(
        error.contains("1 │ import joky/vm fn main() { vm.call_int32_0() }"),
        "{error}"
    );
}

#[test]
#[cfg(unix)]
fn records_run_and_build_with_nominal_type_diagnostics() {
    let work = Workspace::new();
    work.write("records.jk", include_str!("../../examples/records.jk"));
    work.assert_all_backends("records.jk", &[], b"1\n42\n", 0);
    for (source, message) in [
        ("struct P { next: P } fn main() {}", "recursive_record"),
        (
            "struct P { x: Int32 } fn main() { let p = P {} }",
            "Missing field `x`",
        ),
        (
            "struct P { x: Int32 } fn main() { let p = P { x: 1 }; p.x = 2 }",
            "invalid_assignment",
        ),
        ("struct P {} fn main() -> P { P {} }", "Entry point must be"),
        (
            "struct P {} extern fn report_int32(value: P) fn main() {}",
            "host ABI",
        ),
    ] {
        work.write("invalid_record.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_record.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_record.jk:2:"));
        assert!(!work.path().join("invalid_record").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn enums_run_and_build_with_nominal_type_diagnostics() {
    let work = Workspace::new();
    work.write("enums.jk", include_str!("../../examples/enums.jk"));
    work.assert_all_backends("enums.jk", &[], b"0\n42\n", 0);
    for (source, message) in [
        ("enum E { Again(E) } fn main() {}", "recursive_type"),
        (
            "enum E { A, B } fn main() { match E.A { A => {} } }",
            "non_exhaustive_match",
        ),
        (
            "enum E { A } fn main() { match E.A { _ => {}, A => {} } }",
            "unreachable_code",
        ),
        (
            "enum E { A } fn main() { match E.A { Missing => {} } }",
            "invalid_pattern",
        ),
        ("enum E { A } fn main() -> E { E.A }", "Entry point must be"),
        (
            "enum E { A } extern fn report_int32(value: E) fn main() {}",
            "host ABI",
        ),
    ] {
        work.write("invalid_enum.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_enum.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_enum.jk:2:"));
        assert!(!work.path().join("invalid_enum").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn tuples_run_and_build_with_pattern_diagnostics() {
    let work = Workspace::new();
    work.write("tuples.jk", include_str!("../../examples/tuples.jk"));
    work.assert_all_backends("tuples.jk", &[], b"42\n13\n7\n", 0);
    for (source, message) in [
        ("type T = (T,) fn main() {}", "cyclic_type_alias"),
        ("fn main() { let (x, y) = (1,) }", "type_mismatch"),
        ("fn main() { let (x, x) = (1, 2) }", "duplicate_definition"),
        (
            "fn main() { let (x, y) = (1, 2); x = 3 }",
            "invalid_assignment",
        ),
        ("fn main() -> (Int32,) { (1,) }", "Entry point must be"),
        (
            "extern fn report_int32(value: (Int32,)) fn main() {}",
            "host ABI",
        ),
        ("fn main() { let (x y) = (1, 2) }", "Expected `)`"),
    ] {
        work.write("invalid_tuple.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_tuple.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_tuple.jk:2:"));
        assert!(!work.path().join("invalid_tuple").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn bitwise_radix_output_diagnostics_and_shift_failures_match_frontends() {
    let work = Workspace::new();
    work.write("bitwise.jk", include_str!("../../examples/bitwise.jk"));
    let expected = b"163\n3\n92\n9223372036854775808\n1\n-1\n";
    work.assert_all_backends("bitwise.jk", &[], expected, 0);
    for (source, message) in [
        ("fn main() { let x = 0b102 }", "Invalid integer literal"),
        ("fn main() { let x = 0x_aa }", "Invalid integer literal"),
        ("fn main() { let x = 1__000 }", "Invalid integer literal"),
        (
            "fn main() { let x = 0x1_0000_0000_0000_0000 }",
            "outside UInt64 magnitude range",
        ),
        ("fn main() { let x = 0xffff_ffff }", "outside Int32 range"),
        (
            "fn main() { let x = true & false }",
            "same supported numeric type",
        ),
        ("fn main() { let x = ~true }", "Unary `~` operand"),
        (
            "enum E { A, B } fn main() { match E.A { | A => 1 | 2 | B => 3 } }",
            "Expected an identifier",
        ),
    ] {
        work.write("invalid_bits.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_bits.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_bits.jk:2:"));
        assert!(!work.path().join("invalid_bits").exists());
    }
    for (ty, op, count) in [
        ("Int32", "<<", "-1"),
        ("UInt32", ">>", "32"),
        ("Int64", ">>", "64"),
        ("UInt64", "<<", "0x1_0000_0000"),
    ] {
        work.write("shift.jk", &format!("import joky/io.{{println}} fn report_int32(value: Int32) {{ println(&value) }} fn shift(n: {ty}) -> {ty} {{ 1 {op} n }} fn main() {{ shift({count}); report_int32(99) }}"));
        for args in [
            vec!["run", "shift.jk"],
            vec!["run", "--interpreter", "shift.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "Shift count");
            assert!(output.stdout.is_empty());
        }
        success(&work.cli(&["build", "shift.jk"]));
        let output = Command::new(work.path().join("shift")).output().unwrap();
        failure(&output, "Shift count");
        assert!(output.stdout.is_empty());
    }
    work.assert_no_build_files();
}

#[test]
fn integer_conversion_output_diagnostics_and_operand_failures_match_frontends() {
    let work = Workspace::new();
    work.write(
        "conversions.jk",
        include_str!("../../examples/conversions.jk"),
    );
    let expected = b"-1\n4294967295\n4294967295\n18446744073709551615\n42\n-2147483648\n";
    work.assert_all_backends("conversions.jk", &[], expected, 0);
    for (source, message) in [
        ("fn main() { int32() }", "expects exactly one argument"),
        ("fn main() { uint64(1, 2) }", "expects exactly one argument"),
        ("fn main() { int64(true) }", "requires an integer"),
        ("fn main() { int32(\"42\") }", "requires an integer"),
        ("fn main() { int32((1,)) }", "requires an integer"),
        ("fn main() { uint32({}) }", "requires an integer"),
        (
            "fn main() { let n: Int64 = int32(1) }",
            "Type annotation mismatch",
        ),
        ("fn main() { int64(2147483648) }", "outside Int32 range"),
        ("fn main() { uint64(0xffff_ffff) }", "outside Int32 range"),
        (
            "fn int32(n: Int32) -> Int32 { n } fn main() {}",
            "Reserved integer conversion",
        ),
        (
            "extern fn uint64(n: Int32) -> Int32 fn main() {}",
            "Reserved integer conversion",
        ),
    ] {
        work.write("invalid_conversion.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_conversion.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_conversion.jk:2:"));
        assert!(!work.path().join("invalid_conversion").exists());
    }
    for (name, argument, message) in [
        ("division", "1 / 0", "Division by zero"),
        ("shift", "1 << 32", "Shift count"),
    ] {
        let file = format!("{name}_conversion.jk");
        work.write(
            &file,
            &format!(
                "import joky/io.{{println}} fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ uint64({argument}); report_int32(99) }}"
            ),
        );
        for args in [vec!["run", &file], vec!["run", "--interpreter", &file]] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(output.stdout.is_empty());
        }
        success(&work.cli(&["build", &file]));
        let output = Command::new(work.path().join(format!("{name}_conversion")))
            .output()
            .unwrap();
        failure(&output, message);
        assert!(output.stdout.is_empty());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn interpolation_diagnostics_and_runtime_failure_match_all_backends() {
    let work = Workspace::new();
    for (source, message) in [
        (
            r#"fn main() { "{}" }"#,
            "Interpolation requires an expression",
        ),
        (r#"fn main() { "}" }"#, "Unmatched '}'"),
        (
            r#"fn main() { "{42:02}" }"#,
            "format specifiers are not supported",
        ),
        (r#"fn main() { "{missing}" }"#, "missing"),
        (
            r#"struct Missing {} fn main() { let value = Missing {}; "{value}" }"#,
            "Show",
        ),
        (r#"fn main() { f"{42}" }"#, "remove the `f` prefix"),
    ] {
        work.write("invalid.jk", source);
        let output = work.cli(&["build", "invalid.jk"]);
        failure(&output, message);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(message), "{error}");
        assert!(error.contains("invalid.jk"), "{error}");
    }
    work.write("failure.jk", r#"
        import joky/show.{type Show}
        import joky/string.{type StringBuilder}
        import joky/drop.{type Drop}
        import joky/io.{println}
        struct Resource { text: String }
        impl Drop for Resource { fn drop(&mut self) { let event = "drop"; println(&event) } }
        impl Show for Resource { fn show(&self, output: &mut StringBuilder) @noescape { output.push_str(&self.text); 1 / 0; } }
        fn make() -> Resource { Resource { text: "owned" } }
        fn main() { let message = "{make()}"; println(&message) }
    "#);
    success(&work.cli(&["build", "failure.jk"]));
    for output in [
        work.cli(&["run", "failure.jk"]),
        work.cli(&["run", "--interpreter", "failure.jk"]),
        Command::new(work.path().join("failure")).output().unwrap(),
    ] {
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"drop\n");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("zero"),
            "{output:?}"
        );
    }
}

#[test]
#[cfg(unix)]
fn script_multi_domain_late_binding_and_module_diagnostics_run_in_all_backends() {
    let old = include_str!("../fixtures/late_binding_script.jk");
    let new = old
        .replace("a.n-n", "a.n-n*10")
        .replace("b.n+n", "b.n+n*10")
        .replace("{a.n+b.n}", "{a.n+b.n+100}");
    let old = old.replace('{', "{{").replace('}', "}}");
    let new = new.replace('{', "{{").replace('}', "}}");
    for create in ["new", "interpreter"] {
        let work = Workspace::new();
        work.write(
            "late_binding.jk",
            &format!(
                r#"import joky/domain import joky/vm import joky/task import joky/check
fn assert(ok:Bool) {{let message="late binding host"; check.assert(ok,&message)}}
fn control(h:&vm.Vm,directory:domain.DirectoryHandle) @suspends {{
    var attempts=0
    while h.call::<domain.DirectoryHandle,Int32>("probe",directory)==-1 {{
        attempts=attempts+1; assert(attempts<1000); task.yield_now()
    }}
    let next={new:?}
    assert(vm.load_source(h,&next)==0)
    // A new caller before publication still runs the first domain's old code.
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==42)
    assert(vm.module_count(h)==2)
    assert(vm.publish_domain_code(h)==2)
    assert(h.call::<domain.DirectoryHandle,Int32>("probe",directory)==142)
    assert(vm.print_modules(h)==2)
    assert(h.call::<domain.DirectoryHandle,Bool>("release",directory))
}}
fn main() @suspends {{
    let h=vm.Vm.{create}()
    assert(vm.module_count(h)==0)
    let source={old:?}
    assert(vm.load_source(h,&source)==0)
    let owner=domain.directory(2)
    let directory=owner.handle()
    join {{ | h.call::<domain.DirectoryHandle,Unit>("hold",directory) | control(&h,directory) }};
    assert(vm.module_count(h)==1)
    assert(vm.close(h)==0)
    assert(vm.error_code()==0)
    println("late binding ok")
}}"#
            ),
        );
        let mut outputs = vec![
            work.cli(&["run", "late_binding.jk"]),
            work.cli(&["run", "--interpreter", "late_binding.jk"]),
        ];
        success(&work.cli(&["build", "late_binding.jk"]));
        outputs.push(
            std::process::Command::new(work.path().join("late_binding"))
                .output()
                .unwrap(),
        );
        for output in outputs {
            success(&output);
            assert_eq!(output.stdout, b"late binding ok\n");
            let stderr = String::from_utf8(output.stderr).unwrap();
            let lines: Vec<_> = stderr.lines().collect();
            assert_eq!(lines.len(), 2, "{stderr}");
            assert!(lines[0].contains("module=1 current=false"), "{stderr}");
            assert!(lines[0].contains("script_calls=1 domains=0"), "{stderr}");
            assert!(lines[1].contains("module=2 current=true"), "{stderr}");
            assert!(lines[1].contains("script_calls=0 domains=2"), "{stderr}");
        }
        work.assert_no_build_files();
    }
}

#[test]
#[cfg(unix)]
fn raw_and_multiline_string_diagnostics_preserve_file_and_line() {
    let work = Workspace::new();
    for (source, message, location) in [
        (
            "fn main() { r#\"open }",
            "Unterminated string",
            "invalid.jk:1:",
        ),
        (
            "fn main() { \"\"\"same line\n\"\"\" }",
            "must start",
            "invalid.jk:1:",
        ),
        (
            "fn main() { \"\"\"\n  text\"\"\" }",
            "own line",
            "invalid.jk:2:",
        ),
        (
            "fn main() { \"\"\"\n x\n  \"\"\" }",
            "indentation",
            "invalid.jk:2:",
        ),
        (
            "fn main() {\n    let value = \"\"\"\n    🙂\\n{missing}\n    \"\"\"\n}",
            "missing",
            "invalid.jk:3:",
        ),
    ] {
        work.write("invalid.jk", source);
        for args in [
            vec!["build", "invalid.jk"],
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(location), "{args:?}: {stderr}");
        }
    }
}

#[test]
fn explain_parallel_reports_groups_captures_and_runtime_conditions_without_running() {
    let work = Workspace::new();
    work.write(
        "main.jk",
        r#"
        import joky/array.{type Array}
        import joky/io.{println}
        fn read(values: &Array<Int32>) @suspends -> UInt64 { values.len() }
        fn local(value: &mut Int32) @suspends -> Int32 { 1 }
        fn owned() @suspends -> Int32 { 2 }
        fn main() @suspends {
            let values = [1, 2]
            var scalar = 1
            let (a, b) = join { | read(&values) | owned() }
            let c = race { | local(&mut scalar) | owned() }
            let view = &values
            scope { branch { read(view); } }
            let (e, f) = join { | owned() | owned() }
            println("PROGRAM_EXECUTED")
        }
    "#,
    );
    let output = work
        .command()
        .args(["explain-parallel", "main.jk"])
        .env("JOKY_TASK_WORKERS", "invalid")
        .env("PATH", "")
        .output()
        .unwrap();
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("entry: eligible"), "{text}");
    assert!(text.contains("join in main: runtime-check"), "{text}");
    assert!(text.contains("race in main: local"), "{text}");
    assert!(
        text.contains("unsupported cross-worker input type `&mut Int32`"),
        "{text}"
    );
    assert!(text.contains("branch in main: runtime-check"), "{text}");
    assert!(text.contains("join in main: eligible"), "{text}");
    assert!(text.contains("whole group stays local"), "{text}");
    assert!(text.contains("shares the root task budget"), "{text}");
    assert!(!text.contains("PROGRAM_EXECUTED"));
    assert!(output.stderr.is_empty());
    assert_eq!(
        text.as_bytes(),
        work.cli(&["explain-parallel", "main.jk"]).stdout
    );
    work.assert_no_build_files();
}

#[test]
fn explain_parallel_resolves_projects_and_reports_rejection_and_source_errors() {
    let work = Workspace::new();
    work.write(
        "joky.toml",
        "[project]\nname = 'probe'\nentry = 'main.jk'\n",
    );
    work.write(
        "main.jk",
        "import app/reader fn main() @suspends { reader.start() }",
    );
    work.write("app/reader.jk", "fn read(value: &Int32) @suspends {} pub fn start() @suspends { let x = 1; let view = &x; scope { branch { read(view) } } }");
    let output = work.cli(&["explain-parallel", "--no-prelude"]);
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("app/reader.jk:1:"), "{text}");
    assert!(
        text.contains("branch in app/reader.start: runtime-check"),
        "{text}"
    );
    work.write(
        "main.jk",
        "extern fn unknown() fn main() @suspends { scope { branch { unknown() } } }",
    );
    let output = work.cli(&["explain-parallel", "."]);
    failure(&output, "Unknown CLI host function `unknown`");
    assert!(output.stdout.is_empty());
    work.write("main.jk", "fn main() { }");
    let output = work.cli(&["explain-parallel", "joky.toml"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("@suspends"));
    work.write("main.jk", "fn main() @suspends { missing() }");
    failure(&work.cli(&["explain-parallel", "."]), "missing");
    failure(
        &work.cli(&["explain-parallel", "--task-workers", "3"]),
        "Unknown explain-parallel option",
    );
    failure(
        &work.cli(&["explain-parallel", "a.jk", "b.jk"]),
        "exactly one source",
    );
    failure(
        &work.cli(&["explain-parallel", "--no-prelude", "--no-prelude"]),
        "more than once",
    );
}

#[test]
fn expression_nesting_reports_a_located_limit_in_check_and_fmt() {
    let work = Workspace::new();
    for (open, close) in [
        ("(", ")"),
        ("(", ",)"),
        ("{", "}"),
        ("!", ""),
        ("", ".field"),
        ("", "()"),
        ("", " && true"),
    ] {
        let value = if open == "!" { "true" } else { "1" };
        let source = format!(
            "fn main() {{ let x = {}{value}{} }}",
            open.repeat(2000),
            close.repeat(2000)
        );
        work.write("deep.jk", &source);
        for args in [["check", "deep.jk"], ["fmt", "deep.jk"]] {
            let output = work.cli(&args);
            assert_eq!(output.status.code(), Some(1), "{args:?}");
            failure(&output, "Expression nesting exceeds");
        }
    }
}

#[test]
fn interpreter_trace_folds_only_adjacent_identical_frames() {
    let work = Workspace::new();
    work.write("rec.jk", "fn rec(n: Int32) -> Int32 {\n    if n == 5 { return rec(n - 1) }\n    rec(n - 1)\n}\nfn main() { rec(8); }");
    let output = work.cli(&["run", "--interpreter", "rec.jk"]);
    failure(&output, "Interpreter call depth limit exceeded");
    let text = String::from_utf8_lossy(&output.stderr);
    let frames: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("  at "))
        .collect();
    assert_eq!(frames.len(), 4);
    let limit = joky::vm::InterpreterLimits::default().max_call_depth;
    assert!(
        frames[0].contains("at rec")
            && frames[0].ends_with(&format!("(repeated {} times)", limit - 5))
    );
    assert!(frames[1].contains("rec.jk:2,") && !frames[1].contains("repeated"));
    assert!(frames[2].contains("rec.jk:3,") && frames[2].ends_with("(repeated 3 times)"));
    assert!(frames[3].contains("at main") && !frames[3].contains("repeated"));
}
