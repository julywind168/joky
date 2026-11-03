//! Language syntax and scalar behavior through the CLI.
use super::*;

#[test]
#[cfg(unix)]
fn for_into_iter_executes_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/for_into_iter.jk").replace(
        "extern fn record(x: Int32) -> Int32",
        "import joky/io.{println}\nfn record(x: Int32) -> Int32 { println(x); x }",
    );
    work.write(
        "conversion_lib.jk",
        &source.replace("fn compute()", "pub fn compute()"),
    );
    work.write("conversion.jk", "import conversion_lib\nimport joky/io.{println}\nfn main() { println(conversion_lib.compute()) }");
    success(&work.cli(&["build", "--no-prelude", "conversion.jk"]));
    let output = Command::new(work.path().join("conversion"))
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"100\n101\n101\n42\n");
    work.write(
        "conversion_failure.jk",
        &format!("{source}\nfn main() {{ conversion_failure() }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "conversion_failure.jk"]));
    let output = Command::new(work.path().join("conversion_failure"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Division by zero"));
    assert_eq!(output.stdout, b"0\n");
}

#[test]
#[cfg(unix)]
fn for_iterators_execute_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/for_iterators.jk").replace(
        "extern fn record(x: Int32) -> Int32",
        "import joky/io.{println}\nfn record(x: Int32) -> Int32 { println(x); x }",
    );
    for suspends in [false, true] {
        let source = if suspends {
            source
                .replace("fn compute() -> Int32", "fn compute() @suspends -> Int32")
                .replace("fn ownership()", "fn ownership() @suspends")
                .replace(
                    "if value.id == 1 { continue }",
                    "task.yield_now(); if value.id == 1 { continue }",
                )
        } else {
            source.clone()
        };
        let effect = if suspends { "@suspends" } else { "" };
        work.write(
            "iterators.jk",
            &format!("import joky/task\n{source}\nfn main() {effect} {{ println(compute()) }}"),
        );
        success(&work.cli(&["build", "--no-prelude", "iterators.jk"]));
        let output = Command::new(work.path().join("iterators"))
            .output()
            .unwrap();
        success(&output);
        assert_eq!(
            output.stdout,
            b"3\n-1\n1\n-2\n2\n99\n-1\n1\n-2\n2\n-3\n3\n99\n-1\n1\n99\n42\n"
        );
    }
    work.write(
        "failure.jk",
        &format!("{source}\nfn main() {{ force_failure() }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "failure.jk"]));
    let output = Command::new(work.path().join("failure")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Division by zero"));
    assert_eq!(output.stdout, b"-1\n1\n99\n");
}

#[test]
#[cfg(unix)]
fn string_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "queries.jk",
        include_str!("../../examples/string_queries.jk"),
    );
    work.assert_all_backends("queries.jk", &[], b"7\n4\n20013\n128578\n101\n769\n20\n", 0);
}

#[test]
#[cfg(unix)]
fn string_builder_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "builder.jk",
        include_str!("../../examples/string_builder.jk"),
    );
    work.assert_all_backends("builder.jk", &[], b"15\n", 0);
}

#[test]
#[cfg(unix)]
fn string_storage_probe_executes_ownership_and_failures_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "probe.jk",
        &recording_cli_source(include_str!("../fixtures/string_storage_probe.jk")),
    );
    work.write(
        "success.jk",
        "import probe fn main() { probe.report(probe.success()); probe.report(probe.transfer()) }",
    );
    work.assert_all_backends("success.jk", &[], b"-1\n1\n42\n901\n903\n42\n", 0);
    for (entry, expected, message) in [
        ("failure", "-2\n2\n", "Storage probe failure"),
        ("tight_failure", "-3\n902\n3\n", "Primary probe failure"),
    ] {
        work.write(
            "failure.jk",
            &format!("import probe fn main() {{ probe.{entry}(); }}"),
        );
        let built = work.cli(&["build", "failure.jk"]);
        success(&built);
        for output in [
            work.cli(&["run", "failure.jk"]),
            work.cli(&["run", "--interpreter", "failure.jk"]),
            Command::new(work.path().join("failure")).output().unwrap(),
        ] {
            assert!(!output.status.success());
            assert_eq!(output.stdout, expected.as_bytes());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(message), "{error}");
            assert!(!error.contains("Secondary probe failure"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn unicode_string_escapes_execute_and_reject_invalid_scalars_on_all_backends() {
    let work = Workspace::new();
    let mut source = String::from(
        "import joky/string import joky/check import joky/io.{println} fn report_int32(value: Int32) { println(&value) } fn relay(s: String) -> String { s } fn main() {\n",
    );
    for scalar in [
        0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
    ] {
        let ch = char::from_u32(scalar).unwrap();
        source.push_str(&format!(
            r#"{{
                let actual = relay("\u{{{scalar:X}}}")
                let expected = "{ch}"
                let message = "Unicode content mismatch"
                check.assert(actual.equals(&expected), &message)
                let padded = "\u{{{scalar:06x}}}"
                let message = "Unicode length mismatch"
                check.assert(padded.byte_length() == {}, &message)
            }}
            "#,
            ch.len_utf8(),
        ));
    }
    source.push_str(
        r#"
        let nul = "\u{0}Z"; let expected = "\0Z"
        let message = "NUL must preserve following bytes"
        check.assert(nul.equals(&expected), &message)
        let slash = "\u{5c}u{41}"; let expected = "\\u{41}"
        let message = "Escape must decode once"
        check.assert(slash.equals(&expected), &message)
        let newline = "\u{a}"; let expected = "\n"
        let message = "Encoded newline is allowed"
        check.assert(newline.equals(&expected), &message)
        report_int32(42)
    }"#,
    );
    work.write("unicode.jk", &source);
    work.assert_all_backends("unicode.jk", &[], b"42\n", 0);
    for escape in [
        r"\u{}",
        r"\u{0000041}",
        r"\u{d800}",
        r"\u{dfff}",
        r"\u{110000}",
        r"\u{g}",
        r"\u{1_2}",
        r"\u{中}",
        r"\u{41",
        r"\u41",
    ] {
        work.write(
            "invalid.jk",
            &format!("// 中🙂\nfn main() {{ \"中{escape}\" }}"),
        );
        for args in [
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
            vec!["build", "invalid.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "Invalid Unicode escape");
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid.jk:2:"));
        }
        assert!(!work.path().join("invalid").exists());
    }
}

#[test]
#[cfg(unix)]
fn uint8_storage_arithmetic_and_failures_match_all_backends() {
    let work = Workspace::new();
    let mut source = String::from("import joky/io.{println}\n");
    source.push_str(include_str!("../fixtures/uint8.jk"));
    source.push_str("\nfn expect(condition: Bool) { if !condition { let message = \"Byte arithmetic mismatch\"; assert(false, &message) } }\nfn report_int32(value: Int32) { println(&value) }\nfn add(a: UInt8, b: UInt8) -> UInt8 { a + b }\nfn sub(a: UInt8, b: UInt8) -> UInt8 { a - b }\nfn mul(a: UInt8, b: UInt8) -> UInt8 { a * b }\nfn div(a: UInt8, b: UInt8) -> UInt8 { a / b }\nfn shl(a: UInt8, b: UInt8) -> UInt8 { a << b }\nfn shr(a: UInt8, b: UInt8) -> UInt8 { a >> b }\nfn main() { { let assertion_condition = byte_check() == 42; let assertion_message = \"Byte fixture\"; assert(assertion_condition, &assertion_message) }\n");
    for a in [0u8, 1, 3, 127, 128, 254, 255] {
        for b in [0u8, 1, 3, 127, 128, 254, 255] {
            for (name, expected) in [
                ("add", a.wrapping_add(b)),
                ("sub", a.wrapping_sub(b)),
                ("mul", a.wrapping_mul(b)),
            ] {
                source.push_str(&format!("expect({name}({a}, {b}) == {expected})\n"));
            }
            if let Some(expected) = a.checked_div(b) {
                source.push_str(&format!("expect(div({a}, {b}) == {expected})\n"));
            }
        }
        for b in [0, 1, 4, 7] {
            source.push_str(&format!(
                "expect(shl({a}, {b}) == {} && shr({a}, {b}) == {})\n",
                a.wrapping_shl(b),
                a.wrapping_shr(b)
            ));
        }
    }
    source.push_str("report_int32(42) }");
    work.write("bytes.jk", &source);
    work.assert_all_backends("bytes.jk", &[], b"42\n", 0);
    for (expression, message) in [
        ("a << 8", "Shift count"),
        ("a >> 255", "Shift count"),
        ("a / 0", "Division by zero"),
    ] {
        work.write(
            "fail.jk",
            &format!("fn fail(a: UInt8) -> UInt8 {{ {expression} }} fn main() {{ fail(1); }}"),
        );
        success(&work.cli(&["build", "fail.jk"]));
        for output in [
            work.cli(&["run", "fail.jk"]),
            work.cli(&["run", "--interpreter", "fail.jk"]),
            Command::new(work.path().join("fail")).output().unwrap(),
        ] {
            failure(&output, message);
        }
    }
}

#[test]
#[cfg(unix)]
fn float_literals_arithmetic_and_semantics_match_all_backends() {
    let work = Workspace::new();
    work.write("floats.jk", include_str!("../fixtures/float.jk"));
    // `main` runs the semantic and aggregate composites and returns 42.
    work.assert_all_backends("floats.jk", &[], b"", 42);
    for (source, message) in [
        (
            "fn main() -> Int32 { let x: Float32 = 1.0e39; 0 }",
            "finite range",
        ),
        (
            "fn main() -> Int32 { let x: Float64 = 1.0e309; 0 }",
            "finite range",
        ),
        (
            "fn main() { let x = float32(1.0) + float64(2.0) }",
            "same type",
        ),
        (
            "fn main() { let x: Int32 = 1; let y: Float64 = 1.0 + x }",
            "same type",
        ),
        ("fn main() { let x = 1.0 & 2.0 }", "numeric type"),
        ("fn main() { let x = ~1.0 }", "Unary `~` operand"),
        (
            "fn main() -> Int32 { let x: Float64 = .5 0 }",
            "Expected an expression",
        ),
        (
            "fn main() -> Int32 { let x: Float64 = 1.5e 0 }",
            "Invalid floating literal",
        ),
    ] {
        work.write("invalid_float.jk", source);
        failure(&work.cli(&["run", "invalid_float.jk"]), message);
        failure(
            &work.cli(&["run", "--interpreter", "invalid_float.jk"]),
            message,
        );
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn float_math_checked_conversions_and_formatting_match_all_backends() {
    let work = Workspace::new();
    work.write("float_math.jk", include_str!("../fixtures/float_math.jk"));
    // `main` runs math, checked-conversion, bit and formatting composites.
    work.assert_all_backends("float_math.jk", &[], b"", 42);
    for (source, message) in [
        (
            "fn main() -> Int32 { let x = 1.to_bits(); 0 }",
            "has no method 'to_bits'",
        ),
        (
            "fn main() -> Int32 { let x = Float64.from_bits(1.5); 0 }",
            "Argument type mismatch",
        ),
        (
            "fn main() -> Int32 { let x = 1.5.trunc_to_i32() + 1; 0 }",
            "does not implement joky/ops.Add",
        ),
    ] {
        work.write("invalid_float_math.jk", source);
        failure(&work.cli(&["run", "invalid_float_math.jk"]), message);
        failure(
            &work.cli(&["run", "--interpreter", "invalid_float_math.jk"]),
            message,
        );
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn narrow_integers_match_all_backends_and_reject_bad_literals() {
    let work = Workspace::new();
    work.write(
        "narrow_integers.jk",
        include_str!("../fixtures/narrow_integers.jk"),
    );
    // `main` runs the arithmetic, extension, bridge and array composites.
    work.assert_all_backends("narrow_integers.jk", &[], b"", 42);
    for (source, message) in [
        (
            "fn main() -> Int32 { let x: Int8 = 128; 0 }",
            "outside Int8 range",
        ),
        (
            "fn main() -> Int32 { let x: Int8 = -129; 0 }",
            "outside Int8 range",
        ),
        (
            "fn main() -> Int32 { let x: Int16 = -32769; 0 }",
            "outside Int16 range",
        ),
        (
            "fn main() -> Int32 { let x: UInt16 = 65536; 0 }",
            "outside UInt16 range",
        ),
        (
            "fn main() -> Int32 { let x: UInt16 = -1; 0 }",
            "Unary `-` is not supported for UInt16",
        ),
        (
            "fn main() -> Int32 { let x: Int8 = 1; let y: Int16 = 2; if x < y { 0 } else { 1 } }",
            "the same type",
        ),
        (
            "fn main() -> Int32 { let x: UInt16 = 1; if -x == 0 { 0 } else { 1 } }",
            "Unary `-` operand",
        ),
    ] {
        work.write("invalid_narrow.jk", source);
        failure(&work.cli(&["run", "invalid_narrow.jk"]), message);
        failure(
            &work.cli(&["run", "--interpreter", "invalid_narrow.jk"]),
            message,
        );
    }
    // Runtime traps surface with the shared diagnostics in every backend.
    work.write(
        "traps.jk",
        "fn main() -> Int32 { let a: Int8 = 1; let b: Int8 = 0; int32(a / b) }",
    );
    for args in [
        vec!["run", "traps.jk"],
        vec!["run", "--interpreter", "traps.jk"],
    ] {
        failure(&work.cli(&args), "Division by zero");
    }
    work.write(
        "shift.jk",
        "fn main() -> Int32 { let a: Int16 = 1; let n: Int16 = 16; int32(a << n) }",
    );
    for args in [
        vec!["run", "shift.jk"],
        vec!["run", "--interpreter", "shift.jk"],
    ] {
        failure(&work.cli(&args), "less than the integer width");
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn rune_storage_conversions_and_rejections_match_all_backends() {
    let work = Workspace::new();
    work.write(
        "runes.jk",
        &format!(
            "import joky/io.{{println}}\n{}\n fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(rune_check()) }}",
            include_str!("../fixtures/rune.jk")
        ),
    );
    work.assert_all_backends("runes.jk", &[], b"42\n", 0);
    for body in [
        "0xd800",
        "rune(0xd800)",
        "'中' + '文'",
        "~'中'",
        "'中' < '文'",
    ] {
        work.write(
            "invalid_rune.jk",
            &format!("fn bad() -> Rune {{ {body} }} fn main() {{ bad(); }}"),
        );
        for args in [
            vec!["run", "invalid_rune.jk"],
            vec!["run", "--interpreter", "invalid_rune.jk"],
            vec!["build", "invalid_rune.jk"],
        ] {
            assert!(!work.cli(&args).status.success(), "{body}");
        }
    }
}

#[test]
fn rune_patterns_reject_missing_and_redundant_arms() {
    let work = Workspace::new();
    for (body, message) in [
        ("match value { '中' => 1 }", "non_exhaustive_match"),
        (
            "match value { '中' => 1, '中' => 2, _ => 3 }",
            "unreachable",
        ),
        ("match value { _ => 1, '中' => 2 }", "unreachable"),
    ] {
        work.write(
            "invalid_rune.jk",
            &format!("fn classify(value: Rune) -> Int32 {{ {body} }}"),
        );
        failure(&work.cli(&["check", "invalid_rune.jk"]), message);
    }
}

#[test]
#[cfg(unix)]
fn module_constant_aliases_resolve_in_their_defining_scopes() {
    let work = Workspace::new();
    work.write(
        "config.jk",
        r#"
        type Hidden = UInt32
        pub type Scalar<T> = T
        pub const LIMIT: Hidden = 40
        pub const STEP: Scalar<Int8> = 2
    "#,
    );
    work.write(
        "main.jk",
        r#"
        import config.{type Scalar, LIMIT, STEP}
        type Hidden = Bool
        type Wide = Scalar<Float64>
        const SCALE: Wide = 0.5
        const ANSWER: Scalar<Int32> = int32(LIMIT) + int32(STEP)
        fn main() -> Int32 { if float64(ANSWER) * SCALE == 21.0 { ANSWER } else { 0 } }
    "#,
    );
    work.assert_all_backends("main.jk", &[], b"", 42);
    for (ty, message) in [
        ("String", "Module constants require"),
        ("Missing", "Unknown type"),
    ] {
        work.write(
            "invalid.jk",
            &format!("type Alias = {ty} const UNUSED: Alias = 0 fn main() {{}}"),
        );
        failure(&work.cli(&["build", "invalid.jk"]), message);
    }
}

#[test]
#[cfg(unix)]
fn alias_entry_points_and_host_signatures_work_in_all_execution_modes() {
    let work = Workspace::new();
    for (source, expected_output, expected_status) in [
        (include_str!("../../examples/aliases.jk"), "43\n", 0),
        ("type Exit = Int32 fn main() -> Exit { 42 }", "", 42),
        ("type Nothing = Unit fn main() -> Nothing {}", "", 0),
    ] {
        work.write("aliases.jk", source);
        work.assert_all_backends(
            "aliases.jk",
            &[],
            expected_output.as_bytes(),
            expected_status,
        );
    }
    // Built-in callback validation must see Int32 rather than source aliases.
    work.write("callback.jk", r#"
        import joky/vm
        import joky/io.{println}
        type Count = Int32
        type Handle = vm.Vm
        fn report_int32(value: Count) { println(&value) }
        fn next(value: Count) -> Count { value + 1 }
        fn main() {
            let engine: Handle = vm.Vm.new()
            { let vm_text = "next"; vm.register_int32_1(engine, &vm_text, next) }
            { let vm_text = "type Count = Int32 extern fn next(x: Count) -> Count fn value() -> Count {{ next(41) }}"; vm.load_source(engine, &vm_text) }
            report_int32({ let vm_text = "value"; vm.call_int32_0(engine, &vm_text) })
            vm.close(engine);
        }
    "#);
    let output = work.cli(&["run", "callback.jk"]);
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    success(&work.cli(&["build", "callback.jk"]));
    let output = Command::new(work.path().join("callback")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    work.assert_no_build_files();
}

#[test]
fn alias_errors_are_located_before_execution_or_publication() {
    let work = Workspace::new();
    for (index, (declaration, message)) in [
        ("type Count = Missing", "unknown_type"),
        (
            "type Count = Int32 type Count = Bool",
            "duplicate_definition",
        ),
        ("type Count = Total type Total = Count", "cyclic_type_alias"),
        (
            "type Flag = Bool fn main() -> Flag { true }",
            "Entry point must be",
        ),
        (
            "type Flag = Bool extern fn report_int32(value: Flag)",
            "Unknown CLI host function",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let source = if declaration.contains("fn main") {
            declaration.to_owned()
        } else {
            format!("{declaration} fn main() {{}}")
        };
        work.write("invalid.jk", &format!("// 🙂\n{source}"));
        // Resolution and entry validation run before backend dispatch.
        for args in [
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
            vec!["build", "invalid.jk"],
        ]
        .into_iter()
        .filter(|args| index == 0 || args[0] == "build")
        {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid.jk:2:"));
        }
        assert!(!work.path().join("invalid").exists());
        work.assert_no_build_files();
    }
}

#[test]
fn interpreter_runtime_error_reports_source_and_call_trace() {
    let work = Workspace::new();
    work.write(
        "runtime.jk",
        "fn divide() -> Int32 {\n  7 / 0\n}\nfn main() -> Int32 { divide() }\n",
    );
    let output = work.cli(&["run", "--interpreter", "runtime.jk"]);
    failure(&output, "Division by zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("7 / 0"), "{stderr}");
    assert!(stderr.contains("at divide (runtime.jk:2, pc"), "{stderr}");
    assert!(stderr.contains("at main (runtime.jk:4, pc"), "{stderr}");
}

#[test]
#[cfg(unix)]
fn division_errors_and_wrapping_match_all_execution_modes() {
    let work = Workspace::new();
    for return_type in ["", "-> Int32"] {
        let tail = if return_type.is_empty() {
            "report_int32(99)"
        } else {
            "report_int32(99); 0"
        };
        let source = format!("// é\nimport joky/io.{{println}} fn report_int32(value: Int32) {{ println(&value) }}\nfn divide(x: Int32) -> Int32 {{ 7 / x }}\nfn main() {return_type} {{ divide(0); {tail} }}");
        work.write("division.jk", &source);
        let start = source.find("7 / x").unwrap();
        for args in [
            vec!["run", "division.jk"],
            vec!["run", "--interpreter", "division.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "Division by zero");
            assert_eq!(output.status.code(), Some(1));
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                error.contains("runtime_error") && error.contains("7 / x"),
                "{error}"
            );
        }
        success(&work.cli(&["build", "division.jk"]));
        let output = Command::new(work.path().join("division")).output().unwrap();
        failure(&output, "Division by zero");
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains(&format!("bytes {start}..{}", start + 5)));
    }
    work.write("wrapping.jk", "import joky/io.{println} fn report_int32(value: Int32) { println(&value) } fn divide(a: Int32, b: Int32) -> Int32 { a / b } fn main() { report_int32(divide(-2147483648, -1)); report_int32(divide(-7, 3)); report_int32(divide(7, -3)) }");
    work.assert_all_backends("wrapping.jk", &[], b"-2147483648\n-2\n-2\n", 0);
}

#[test]
#[cfg(unix)]
fn loop_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("loops.jk", include_str!("../../examples/loops.jk"));
    work.assert_all_backends("loops.jk", &[], b"45\n", 0);
}

#[test]
#[cfg(unix)]
fn sequence_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("sequences.jk", include_str!("../../examples/sequences.jk"));
    work.assert_all_backends("sequences.jk", &[], b"4\n3\n", 0);
}

#[test]
#[cfg(unix)]
fn newline_boundaries_and_method_chains_match_all_frontends() {
    let names = [
        "newline_deref",
        "newline_prefix",
        "newline_borrow",
        "newline_operators",
        "newline_grouped",
        "newline_nested_block",
        "newline_methods",
        "newline_named_methods",
        "newline_generic_method",
        "newline_owning_chain",
        "newline_tail",
        "newline_return",
        "newline_static_dispatch",
        "newline_dyn_dispatch",
    ];
    let calls = names
        .iter()
        .map(|name| format!("{{ let value = {name}(); println(&value) }}\n"))
        .collect::<String>();
    let output = b"42\n-42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n";
    for effect in ["", " @suspends"] {
        let work = Workspace::new();
        let source = format!(
            "import joky/io.{{println}}\n{}\n{}\nfn main(){effect} {{\n{calls} newline_discard()\n}}",
            include_str!("../fixtures/newlines.jk"),
            include_str!("../fixtures/newline_dispatch.jk")
        );
        work.write("newlines.jk", &source);
        work.assert_all_backends("newlines.jk", &[], output, 0);
    }
}

#[test]
#[cfg(unix)]
fn conditional_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write(
        "conditionals.jk",
        include_str!("../../examples/conditionals.jk"),
    );
    work.assert_all_backends("conditionals.jk", &[], b"-1\n0\n1\n0\n4\n", 0);
}

#[test]
#[cfg(unix)]
fn jump_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("jumps.jk", include_str!("../../examples/jumps.jk"));
    work.assert_all_backends("jumps.jk", &[], b"0\n4\n13\n", 0);
}

#[test]
#[cfg(unix)]
fn int64_literals_output_and_failures_match_all_frontends() {
    let work = Workspace::new();
    work.write("int64.jk", include_str!("../../examples/int64.jk"));
    let expected = b"3000000042\n5000000000\n-9223372036854775808\n";
    work.assert_all_backends("int64.jk", &[], expected, 0);
    for (source, message) in [
        ("fn main() { let n = 2147483648 }", "outside Int32 range"),
        (
            "fn f() -> Int64 { 9223372036854775808 } fn main() {}",
            "outside Int64 range",
        ),
        (
            "fn main() { let n: Int64 = -9223372036854775809 }",
            "outside Int64 range",
        ),
        (
            "fn main() { let n = 1; let wide: Int64 = n }",
            "annotation mismatch",
        ),
        (
            "extern fn report_int64(n: Int32) fn main() {}",
            "Unknown CLI host function",
        ),
        ("fn main() -> Int64 { 1 }", "Entry point must be"),
    ] {
        work.write("invalid64.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid64.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid64.jk:2:"));
        assert!(!work.path().join("invalid64").exists());
    }
    work.write("division64.jk", "import joky/io.{println} fn report_int64(value: Int64) { println(&value) } fn divide(n: Int64) -> Int64 { 3000000000 / n } fn main() { report_int64(divide(0)) }");
    for args in [
        vec!["run", "division64.jk"],
        vec!["run", "--interpreter", "division64.jk"],
    ] {
        let output = work.cli(&args);
        failure(&output, "Division by zero");
        assert!(output.stdout.is_empty());
    }
    success(&work.cli(&["build", "division64.jk"]));
    let output = Command::new(work.path().join("division64"))
        .output()
        .unwrap();
    failure(&output, "Division by zero");
    assert!(output.stdout.is_empty());
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn unsigned_output_boundaries_and_errors_match_all_frontends() {
    let work = Workspace::new();
    work.write("unsigned.jk", include_str!("../../examples/unsigned.jk"));
    let expected = b"4294967295\n18446744073709551615\n9223372036854775807\n0\n0\n";
    work.assert_all_backends("unsigned.jk", &[], expected, 0);
    for (source, message) in [
        (
            "fn main() { let n: UInt32 = 4294967296 }",
            "outside UInt32 range",
        ),
        (
            "fn main() { let n: UInt64 = 18446744073709551616 }",
            "outside UInt64 magnitude range",
        ),
        (
            "fn main() { let n: UInt64 = -1 }",
            "not supported for UInt64",
        ),
        (
            "fn main() { let n = 1; let u: UInt32 = n }",
            "annotation mismatch",
        ),
        ("fn main() -> UInt32 { 0 }", "Entry point must be"),
        (
            "extern fn report_uint32(n: Int32) fn main() {}",
            "Unknown CLI host function",
        ),
        (
            "extern fn report_uint64(n: Int64) fn main() {}",
            "Unknown CLI host function",
        ),
    ] {
        work.write("invalid_unsigned.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_unsigned.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_unsigned.jk:2:"));
        assert!(!work.path().join("invalid_unsigned").exists());
    }
    for (ty, print, max) in [
        ("UInt32", "report_uint32", "4294967295"),
        ("UInt64", "report_uint64", "18446744073709551615"),
    ] {
        work.write("division_unsigned.jk", &format!("import joky/io.{{println}} fn {print}(value: {ty}) {{ println(&value) }} fn divide(n: {ty}) -> {ty} {{ {max} / n }} fn main() {{ {print}(divide(0)) }}"));
        for args in [
            vec!["run", "division_unsigned.jk"],
            vec!["run", "--interpreter", "division_unsigned.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "Division by zero");
            assert!(output.stdout.is_empty());
        }
        success(&work.cli(&["build", "division_unsigned.jk"]));
        let output = Command::new(work.path().join("division_unsigned"))
            .output()
            .unwrap();
        failure(&output, "Division by zero");
        assert!(output.stdout.is_empty());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_functions_run_build_and_reject_invalid_templates_in_all_frontends() {
    let work = Workspace::new();
    work.write("generics.jk", include_str!("../../examples/generics.jk"));
    let expected = b"42\n4294967295\n18446744073709551615\n7\n";
    work.assert_all_backends("generics.jk", &[], expected, 0);
    for (source, message) in [
        (
            "fn bad<T>(n: T) -> T { n + n } fn main() {}",
            "same supported numeric type",
        ),
        (
            "fn bad<T>(n: T) -> Int32 { int32(n) } fn main() {}",
            "requires an integer",
        ),
        (
            "fn bad<T>(n: T) -> Int32 { n.field } fn main() {}",
            "has no fields",
        ),
        (
            "fn id<T>(n: T) -> T { n } fn main() { id(\"text\") }",
            "type_mismatch",
        ),
        (
            "fn tag<T>() -> Int32 { 7 } fn main() { tag() }",
            "Cannot infer type parameter",
        ),
        (
            "fn id<T>(n: T) -> T { n } fn main() { id::<Int64, Bool>(1) }",
            "Type argument count mismatch",
        ),
        (
            "fn id<T>(n: T) -> T { n } fn main() { id::<Int64>(true) }",
            "Conflicting types",
        ),
        (
            "fn id<T>(n: T) -> T { n } fn main() { id(2147483648) }",
            "outside Int32 range",
        ),
        (
            "fn id<T>(n: T) -> T { n } fn main() { let input: Int32 = 1; let n: Int64 = id(input) }",
            "Conflicting types",
        ),
        (
            "fn f(n: Int32) -> Int32 { n } fn main() { f::<Int32>(1) }",
            "does not accept type arguments",
        ),
        ("fn main<T>() {}", "Entry point"),
        (
            "fn id<T,T>(n: T) -> T { n } fn main() {}",
            "Duplicate or reserved type parameter",
        ),
        (
            "fn id<T>(n: T) -> T { n } fn main() { id::<>() }",
            "Expected a type",
        ),
        ("extern fn id<T>(n: T) -> T fn main() {}", "Expected LParen"),
        (
            "fn grow<T>(n: T) { grow((n,)) } fn main() { grow({}) }",
            "Generated function expansion exceeds 32 dependency levels (requested depth 33)",
        ),
    ] {
        work.write("invalid_generic.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_generic.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_generic.jk:2:"));
        assert!(!work.path().join("invalid_generic").exists());
    }
    work.write("generic_failure.jk", "import joky/io.{println} fn report_int32(value: Int32) { println(&value) } fn fail<T>(x: T, n: Int32) -> T { 1 / n; x } fn main() { fail((1, true), 0); report_int32(99) }");
    for args in [
        vec!["run", "generic_failure.jk"],
        vec!["run", "--interpreter", "generic_failure.jk"],
    ] {
        let output = work.cli(&args);
        failure(&output, "Division by zero");
        assert!(output.stdout.is_empty());
    }
    success(&work.cli(&["build", "generic_failure.jk"]));
    let output = Command::new(work.path().join("generic_failure"))
        .output()
        .unwrap();
    failure(&output, "Division by zero");
    assert!(output.stdout.is_empty());
    work.assert_no_build_files();
}

#[test]
fn generic_enums_run_build_and_report_constructor_and_layout_errors() {
    let work = Workspace::new();
    work.write(
        "generic_enums.jk",
        include_str!("../../examples/generic_enums.jk"),
    );
    let expected = b"42\n7\n18446744073709551615\n42\n-1\n";
    work.assert_all_backends("generic_enums.jk", &[], expected, 0);
    let source = include_str!("../fixtures/generic_enums.jk");
    work.write(
        "enum_fixture.jk",
        &format!("import joky/io.{{println}}\n{source}\n fn report_int32(value: Int32) {{ println(&value) }}\nfn main() {{ report_int32(check()) }}"),
    );
    work.assert_all_backends("enum_fixture.jk", &["--no-prelude"], b"42\n", 0);
    for (source, message) in [
        ("fn main() { Option.None }", "Cannot infer type parameter"),
        (
            "fn main() { Option.Some(\"text\") }",
            "Return type mismatch",
        ),
        (
            "fn main() { Option.Some::<Int32, Bool>(1) }",
            "Type argument count mismatch",
        ),
        (
            "fn main() { let x: Option = Option.None }",
            "requires type arguments",
        ),
        (
            "fn main() { let x: Option<Int32> = Option.None::<Bool> }",
            "Conflicting types",
        ),
        (
            "enum Bad<T> { Next(Bad<T>) } fn main() {}",
            "recursive_type",
        ),
        (
            "enum Bad<T> { Value(vm.Function) } fn main() {}",
            "Copy value",
        ),
        (
            "fn main() { match Option.Some(1) { None => {} } }",
            "non_exhaustive_match",
        ),
        (
            "enum E<T, T> { Value(T) } fn main() {}",
            "Duplicate or reserved type parameter",
        ),
        (
            "fn main() { let x: Option<> = Option.None }",
            "Expected a type",
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
fn generic_aliases_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_aliases.jk",
        include_str!("../../examples/generic_aliases.jk"),
    );
    work.write(
        "model.jk",
        "pub type PairOf<T> = (T, T) pub fn duplicate<T: Copy>(n: T) -> PairOf<T> { (n, n) }",
    );
    work.write(
        "imported_alias.jk",
        r#"
        import model as m.{type PairOf as Duo}
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        type PairOf<T> = m.PairOf<T>
        fn first<T>(p: PairOf<T>) -> T { let (a, b) = p; a }
        fn show() { let p: Duo<Int32> = m.duplicate(42); report_int32(first(p)) }
        fn main() { show() }
    "#,
    );
    for (file, program, expected) in [
        (
            "generic_aliases.jk",
            "generic_aliases",
            "4294967295\n42\n18446744073709551615\n18446744073709551615\n",
        ),
        ("imported_alias.jk", "imported_alias", "42\n"),
    ] {
        for args in [vec!["run", file], vec!["run", "--interpreter", file]] {
            let output = work.cli(&args);
            success(&output);
            assert_eq!(output.stdout, expected.as_bytes());
        }
        success(&work.cli(&["build", file]));
        let output = Command::new(work.path().join(program)).output().unwrap();
        success(&output);
        assert_eq!(output.stdout, expected.as_bytes());
    }
    for (source, message) in [
        (
            "type PairOf<T> = (T, T) fn main() { let p: PairOf = (1, 2) }",
            "requires type arguments",
        ),
        (
            "type Drop<T> = Int32 type Bad = Drop<vm.Function> fn main() {}",
            "must be a Copy type",
        ),
        ("type Loop<T> = Loop<T> fn main() {}", "cyclic_type_alias"),
        (
            "type Bad<T,T> = T fn main() {}",
            "Duplicate or reserved type parameter",
        ),
        ("type Bad<T> = Missing fn main() {}", "unknown_type"),
    ] {
        work.write("invalid_alias.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_alias.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_alias.jk:2:"));
        assert!(!work.path().join("invalid_alias").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn option_result_methods_run_and_build_with_explicit_or_default_imports() {
    let work = Workspace::new();
    work.write(
        "option_result.jk",
        include_str!("../../examples/option_result.jk"),
    );
    work.write("std_values.jk", &format!("import joky/io.{{println}}\n{}\n fn report_int32(value: Int32) {{ println(&value) }} fn report_uint64(value: UInt64) {{ println(&value) }} fn main() {{ report_int32(check()); report_uint64(wide()) }}", include_str!("../fixtures/option_result_methods.jk")));
    work.write("result_only.jk", "import joky/result as r import joky/io.{println} fn report_int32(value: Int32) { println(&value) } fn main() { report_int32(r.Result.Ok::<Int32, Bool>(42).ok().unwrap_or(0)) }");
    for (file, program, expected, no_prelude) in [
        (
            "option_result.jk",
            "option_result",
            "42\n7\n4294967295\n42\n9\n7\n1\n42\n42\n1\n42\ntrue\n",
            false,
        ),
        (
            "std_values.jk",
            "std_values",
            "42\n18446744073709551615\n",
            true,
        ),
        ("result_only.jk", "result_only", "42\n", true),
    ] {
        for mut args in [vec!["run", file], vec!["run", "--interpreter", file]] {
            if no_prelude {
                args.push("--no-prelude");
            }
            let output = work.cli(&args);
            success(&output);
            assert_eq!(output.stdout, expected.as_bytes());
        }
        let mut args = vec!["build", file];
        if no_prelude {
            args.push("--no-prelude");
        }
        success(&work.cli(&args));
        let output = Command::new(work.path().join(program)).output().unwrap();
        success(&output);
        assert_eq!(output.stdout, expected.as_bytes());
    }
    for (source, message) in [
        ("fn main() { Some(42).unwrap_or(false) }", "type_mismatch"),
        (
            "fn main() { Result.Ok::<Int32, Bool>(42).or(Result.Ok::<Bool, Unit>(true)) }",
            "type_mismatch",
        ),
    ] {
        work.write("invalid_std.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_std.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_std.jk:2:"));
        assert!(!work.path().join("invalid_std").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_methods_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_methods.jk",
        include_str!("../../examples/generic_methods.jk"),
    );
    for (file, fixture) in [
        (
            "shared_methods.jk",
            include_str!("../fixtures/generic_methods.jk"),
        ),
        (
            "library_methods.jk",
            include_str!("../fixtures/generic_library_methods.jk"),
        ),
    ] {
        work.write(
            file,
            &format!("import joky/io.{{println}}\n{fixture}\n fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(check()) }}"),
        );
    }
    for (file, program, expected, no_prelude) in [
        (
            "generic_methods.jk",
            "generic_methods",
            "42\n4294967295\n7\n42\n4294967295\n",
            false,
        ),
        ("shared_methods.jk", "shared_methods", "42\n", true),
        ("library_methods.jk", "library_methods", "42\n", true),
    ] {
        for mut args in [vec!["run", file], vec!["run", "--interpreter", file]] {
            if no_prelude {
                args.push("--no-prelude");
            }
            let output = work.cli(&args);
            success(&output);
            assert_eq!(output.stdout, expected.as_bytes());
        }
        let mut args = vec!["build", file];
        if no_prelude {
            args.push("--no-prelude");
        }
        success(&work.cli(&args));
        let output = Command::new(work.path().join(program)).output().unwrap();
        success(&output);
        assert_eq!(output.stdout, expected.as_bytes());
    }
    for (source, message) in [
        ("struct Pair<T> { value: T } impl<T> Pair<T> { fn bad<U>(self, value: U) -> T { value } } fn main() {}", "type_mismatch"),
        ("struct Pair<T> { value: T } impl<T> Pair<T> { fn bad<Pair>(self, value: Pair) -> Self { self } } fn main() {}", "cannot shadow impl parameters or the impl target"),
        ("fn main() { Some(1).zip::<Int32, Bool>(Some(true)) }", "expected 1"),
        ("fn main() { Some(1).zip::<String>(Some(\"text\")) }", "type_mismatch"),
        ("fn main() { Result.Ok::<Int32, Bool>(1).and(Result.Ok(\"text\")) }", "type_mismatch"),
    ] {
        work.write("invalid_method.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_method.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_method.jk:2:"));
        assert!(!work.path().join("invalid_method").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_impls_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_impls.jk",
        include_str!("../../examples/generic_impls.jk"),
    );
    work.write("model.jk", "pub struct Pair<T> { left: T, right: T } impl<T> Pair<T> { pub fn make(left: T, right: T) -> Self { Self { left: left, right: right } } pub fn first(self) -> T { self.left } }");
    work.write("imported_impl.jk", "import model as m.{type Pair as Renamed} import joky/io.{println} fn report_int64(value: Int64) { println(&value) } fn main() { report_int64(m.Pair::<Int64>.make(4294967295, 0).first()); report_int64(Renamed.make::<Int64>(42, 0).first()) }");
    work.write(
        "shared_impl.jk",
        &format!(
            "import joky/io.{{println}}\n{}\n fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(check()) }}",
            include_str!("../fixtures/generic_impls.jk")
        ),
    );
    for (file, program, expected) in [
        (
            "generic_impls.jk",
            "generic_impls",
            "42\n4294967295\n18446744073709551615\n42\n7\n2\n",
        ),
        ("imported_impl.jk", "imported_impl", "4294967295\n42\n"),
        ("shared_impl.jk", "shared_impl", "42\n"),
    ] {
        for args in [vec!["run", file], vec!["run", "--interpreter", file]] {
            let output = work.cli(&args);
            success(&output);
            assert_eq!(output.stdout, expected.as_bytes());
        }
        success(&work.cli(&["build", file]));
        let output = Command::new(work.path().join(program)).output().unwrap();
        success(&output);
        assert_eq!(output.stdout, expected.as_bytes());
    }
    for (source, message) in [
        ("struct Pair<T> { value: T } impl<T> Pair<T> { fn bad(self) -> T { self.value + self.value } } fn main() {}", "type_mismatch"),
        ("struct Pair<T> { value: T } impl<T> Pair<Int32> {} fn main() {}", "specialized implementations"),
        ("struct Pair<T> { value: T } impl<T> Pair<T> { fn method<T>(self, value: T) {} } fn main() {}", "cannot shadow impl parameters"),
        ("struct Marker<T> {} impl<T> Marker<T> { fn make() -> Self { Self {} } } fn main() { let value = Marker.make() }", "Cannot infer type parameter"),
        ("struct Marker<T> {} impl<T> Marker<T> { fn make() -> Self { Self {} } } fn main() { Marker.make::<String>() }", "type_mismatch"),
    ] {
        work.write("invalid_impl.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_impl.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_impl.jk:2:"));
        assert!(!work.path().join("invalid_impl").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_records_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_records.jk",
        include_str!("../../examples/generic_records.jk"),
    );
    let expected = b"42\n9\n4294967295\n18446744073709551615\n42\n";
    work.assert_all_backends("generic_records.jk", &[], expected, 0);
    for (source, message) in [
        (
            "struct Marker<T> {} fn main() { let m = Marker {} }",
            "Cannot infer type parameter",
        ),
        (
            "struct Pair<T> { value: T } fn main() { let p: Pair = Pair { value: 1 } }",
            "requires type arguments",
        ),
        (
            "struct Pair<T> { value: T } fn main() { let p = Pair::<Bool> { value: 1 } }",
            "type_mismatch",
        ),
        (
            "struct Bad<T> { next: Bad<T> } fn main() {}",
            "recursive_record",
        ),
        (
            "struct Bad<T,T> {} fn main() {}",
            "Duplicate or reserved type parameter",
        ),
        (
            "struct Bad<T> { value: vm.Function } fn main() {}",
            "Copy value",
        ),
    ] {
        work.write("invalid_record.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_record.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_record.jk:2:"));
        assert!(!work.path().join("invalid_record").exists());
    }
}

#[test]
fn integer_methods_run_build_and_report_invalid_members() {
    let work = Workspace::new();
    for (file, source, expected) in [
        (
            "integer_parse.jk",
            include_str!("../fixtures/integer_parse.jk").to_owned(),
            "",
        ),
        (
            "integer_methods.jk",
            include_str!("../../examples/integer_methods.jk").to_owned(),
            "42\n1\n18446744073709551615\n2147483647\n-1\n",
        ),
        (
            "methods.jk",
            format!(
                "import joky/io.{{println}}\n{}\n fn report_int32(value: Int32) {{ println(&value) }}\nfn main() {{ report_int32(check()) }}",
                include_str!("../fixtures/methods.jk")
            ),
            "42\n",
        ),
    ] {
        work.write(file, &source);
        for args in [vec!["run", file], vec!["run", "--interpreter", file]] {
            let output = work.cli(&args);
            success(&output);
            assert_eq!(output.stdout, expected.as_bytes());
        }
        success(&work.cli(&["build", file]));
        let output = Command::new(work.path().join(file.trim_end_matches(".jk")))
            .output()
            .unwrap();
        success(&output);
        assert_eq!(output.stdout, expected.as_bytes());
    }
    for (source, message) in [
        (
            "impl Int32 { const BAD: Int32 = 1 / 0 } fn main() {}",
            "constant expression",
        ),
        (
            "import joky/integer fn main() { true.to_u64(); }",
            "has no method",
        ),
        (
            "import joky/integer enum Option<T> { No, Yes(T) } fn main() {}",
            "already defined",
        ),
        (
            "import joky/integer fn main() { let n: UInt64 = 42.to_u64() }",
            "Type annotation mismatch",
        ),
    ] {
        work.write("invalid.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid.jk"]);
        failure(&output, message);
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid.jk:2:"));
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn borrowed_receiver_example_matches_all_frontends_and_reports_conflicts() {
    let work = Workspace::new();
    work.write(
        "methods.jk",
        include_str!("../../examples/borrowed_methods.jk"),
    );
    for args in [
        vec!["run", "methods.jk"],
        vec!["run", "--interpreter", "methods.jk"],
        vec!["run", "--no-prelude", "methods.jk"],
        vec!["run", "--interpreter", "--no-prelude", "methods.jk"],
    ] {
        let output = work.cli(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
    }
    let output = work.cli(&["build", "--no-prelude", "methods.jk", "-o", "methods"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(work.path().join("methods")).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
    work.write("bad.jk", "struct Counter { value: Int32 } impl Counter { fn set(&mut self) { self.value = 1 } } fn main() { let c = Counter { value: 0 }; c.set() }");
    let output = work.cli(&["build", "bad.jk", "-o", "bad"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("var binding"));
    assert!(!work.path().join("bad").exists());
}

#[test]
#[cfg(unix)]
fn static_trait_example_matches_all_frontends_and_reports_bound_errors() {
    let work = Workspace::new();
    work.write("traits.jk", include_str!("../../examples/static_traits.jk"));
    for args in [
        vec!["run", "traits.jk"],
        vec!["run", "--interpreter", "traits.jk"],
        vec!["run", "--no-prelude", "traits.jk"],
        vec!["run", "--interpreter", "--no-prelude", "traits.jk"],
    ] {
        let output = work.cli(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
    }
    let output = work.cli(&["build", "--no-prelude", "traits.jk", "-o", "traits"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(work.path().join("traits")).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
    work.write(
        "bad.jk",
        "trait Mark {} fn need<T: Mark>(x: T) {} fn main() { need(42) }",
    );
    let output = work.cli(&["build", "bad.jk", "-o", "bad"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not satisfy trait bound"));
    assert!(!work.path().join("bad").exists());
}

#[test]
#[cfg(unix)]
fn function_value_example_matches_all_frontends_and_rejects_signature_mismatches() {
    let work = Workspace::new();
    work.write(
        "functions.jk",
        include_str!("../../examples/function_values.jk"),
    );
    for args in [
        vec!["run", "functions.jk"],
        vec!["run", "--interpreter", "functions.jk"],
        vec!["run", "--no-prelude", "functions.jk"],
        vec!["run", "--interpreter", "--no-prelude", "functions.jk"],
    ] {
        let output = work.cli(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
    }
    let output = work.cli(&["build", "--no-prelude", "functions.jk", "-o", "functions"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(work.path().join("functions"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
    work.write(
        "bad.jk",
        "fn id(x: Int32) -> Int32 { x } fn main() { let f = id; f(true); }",
    );
    let output = work.cli(&["build", "bad.jk", "-o", "bad"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Argument type mismatch"));
    assert!(!work.path().join("bad").exists());
}

#[test]
#[cfg(unix)]
fn checked_failure_runs_and_reports_owned_messages_on_all_backends() {
    let work = Workspace::new();
    let source = &recording_cli_source(include_str!("../fixtures/checked_failure.jk"));
    work.write("checked_failure.jk", source);
    work.assert_all_backends("checked_failure.jk", &[], b"10\n20\n42\n", 0);
    for (name, message, events) in [
        ("check_failure", "Requested failure", "10\n20\n1\n"),
        ("check_argument_failure", "Condition failure", "2\n"),
        ("check_drop_failure", "Drop failure", "4\n3\n"),
        ("check_first_failure", "First failure", "5\n"),
        ("check_indirect", "Indirect failure", ""),
    ] {
        let source = source.replace("fn main()", "fn successful_main()")
            + &format!("\nfn main() {{ {name}(); }}\n");
        work.write("failure.jk", &source);
        let assert_failure = |output: std::process::Output| {
            assert!(!output.status.success(), "{output:?}");
            assert_eq!(output.stdout, events.as_bytes());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(message), "{stderr}");
            assert!(
                stderr.contains("assert") || stderr.contains("bytes"),
                "{stderr}"
            );
        };
        assert_failure(work.cli(&["run", "failure.jk"]));
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(Command::new(work.path().join("failure")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn panic_divergence_and_cleanup_match_all_backends() {
    let work = Workspace::new();
    let source = recording_cli_source(include_str!("../fixtures/panic.jk"));
    work.write(
        "panic.jk",
        &(source.clone() + "\nfn main() { record_int(panic_success()) }"),
    );
    work.assert_all_backends("panic.jk", &["--no-prelude"], b"42\n", 0);
    for (name, message, events, suspends) in [
        ("panic_cleanup", "Primary panic 42", "10\n2\n1\n", false),
        ("panic_argument", "Argument failure", "11\n", false),
        ("panic_match", "Missing value", "", false),
        ("panic_both", "Both branches", "", false),
        ("panic_return", "Return operand", "", false),
        ("panic_indirect", "Indirect panic", "", false),
        ("panic_task", "Task panic", "3\n", true),
    ] {
        let effect = if suspends { "@suspends" } else { "" };
        work.write(
            "panic.jk",
            &(source.clone() + &format!("\nfn main() {effect} {{ {name}(); }}")),
        );
        let check = |output: std::process::Output| {
            assert_eq!(output.status.code(), Some(1), "{name}: {output:?}");
            assert_eq!(output.stdout, events.as_bytes(), "{name}: {output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(message), "{name}: {stderr}");
        };
        check(work.cli(&["run", "panic.jk"]));
        check(work.cli(&["run", "--interpreter", "panic.jk"]));
        success(&work.cli(&["build", "panic.jk"]));
        check(Command::new(work.path().join("panic")).output().unwrap());
    }
    work.write("prelude.jk", "fn main() { panic(\"Prelude panic\") }");
    failure(&work.cli(&["run", "prelude.jk"]), "Prelude panic");
    failure(
        &work.cli(&["run", "--interpreter", "prelude.jk"]),
        "Prelude panic",
    );
    success(&work.cli(&["build", "prelude.jk"]));
    failure(
        &Command::new(work.path().join("prelude")).output().unwrap(),
        "Prelude panic",
    );
}

#[test]
#[cfg(unix)]
fn intrinsic_declarations_and_assert_prelude_match_all_backends() {
    let work = Workspace::new();
    work.write(
        "intrinsics.jk",
        r#"
        fn main() {
            assert(true, "Literal assertion")
            let original = "copy={42}"
            let copied = original.clone()
            assert(copied.equals(original), "Text clone")
            assert(original.byte_length() == 7, "Borrowed source stays live")
            assert((4.0).sqrt() == 2.0, "Float primitive")
            assert(Rune.from_u32(65).unwrap_or('Z') == 'A', "Rune primitive")
            println(copied)
        }
    "#,
    );
    work.assert_all_backends("intrinsics.jk", &[], b"copy=42\n", 0);
    work.write(
        "explicit.jk",
        "import joky/check.{assert} fn main() { assert(true, \"Explicit import\") }",
    );
    work.assert_all_backends("explicit.jk", &["--no-prelude"], b"", 0);
    for source in [
        r#"#intrinsic("text.clone") fn fake(text: &String) -> String; fn main() {}"#,
        r#"#intrinsic("check.assert") fn fake(ok: Bool, text: &String); fn main() {}"#,
        r#"#intrinsic("text.clone") fn fake(text: &String) -> String fn main() {}"#,
        r#"#intrinsic("check.assert") fn fake(ok: Bool, text: &String) fn main() {}"#,
    ] {
        work.write("invalid.jk", source);
        failure(&work.cli(&["build", "invalid.jk"]), "restricted to trusted");
    }
    work.write(
        "old.jk",
        "import joky/check fn main() { check.require(true, \"Removed name\") }",
    );
    failure(&work.cli(&["build", "old.jk"]), "require");
}

#[test]
#[cfg(unix)]
fn show_print_and_interpolation_match_all_backends() {
    let work = Workspace::new();
    work.write("example.jk", include_str!("../../examples/show.jk"));
    work.assert_all_backends(
        "example.jk",
        &[],
        b"player=Joky:42, next=43\nscores=[41, 42]\nJoky:42\n",
        0,
    );
    work.write("show.jk", include_str!("../fixtures/show.jk"));
    work.write(
        "main.jk",
        r#"
        import show as samples
        import joky/io.{print, println}
        fn main() {
            let scalar = samples.scalar_text(); println(&scalar)
            let custom = samples.custom_text(); println(&custom)
            let array = samples.array_text(); println(&array)
            let option = samples.option_text(); println(&option)
            let result = samples.result_text(); println(&result)
            let order = samples.evaluation_text(); println(&order)
            let empty = ""; print(&empty)
            let prefix = "tail="; print(&prefix)
            let rune = '\0'; print(&rune)
            let flag = false; println(&flag)
        }
    "#,
    );
    let expected = "-128|-32768|-2147483648|-9223372036854775808|255|65535|4294967295|18446744073709551615|1.5|-0|true|界|()|NaN|inf\n<Item(42:hé\0🙂)>|Item(42:hé\0🙂)|hé\0🙂\n[Item(1:one), Item(2:two)]|[Item(1:one), Item(2:two)]\nSome(Item(42:yes))|None|Some(Item(42:yes))\nOk(Item(42:yes))|Err(no)|Ok(Item(42:yes))\n{x}=1:2:[2]:kept:block:2:kept\ntail=\0false\n";
    work.assert_all_backends("main.jk", &[], expected.as_bytes(), 0);
}

#[test]
#[cfg(unix)]
fn standard_errors_print_to_stderr_across_backends() {
    let work = Workspace::new();
    work.write(
        "errors.jk",
        r#"
        import joky/fs
        import joky/io
        fn main() {
            let error = UInt16.parse("65536").err()!
            eprintln(error)
            assert(match error { OutOfRange(offset) => offset == 4, _ => false }, "borrowed error")
            eprintln(UInt16.parse("x"))
            eprintln(io.ReadError.Io(fs.IoError { kind: fs.ErrorKind.Closed, message: "" }))
            eprintln(io.ReadError.Utf8(String.from_utf8([uint8(255)]).err()!))
            eprintln(io.ReadError.InvalidLimit)
            eprintln(io.ReadError.LimitExceeded)
            eprintln(io.ReadError.Closed)
        }
        "#,
    );
    success(&work.cli(&["build", "errors.jk"]));
    for output in [
        work.cli(&["run", "errors.jk"]),
        work.cli(&["run", "--interpreter", "errors.jk"]),
        Command::new(work.path().join("errors")).output().unwrap(),
    ] {
        success(&output);
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            concat!(
                "integer out of range at byte 4\n",
                "Err(invalid decimal integer at byte 0)\n",
                "Closed\n",
                "invalid UTF-8 sequence at byte 0\n",
                "invalid stdin read limit\n",
                "stdin read limit exceeded\n",
                "stdin reader is closed\n",
            )
            .as_bytes()
        );
    }
}

#[test]
#[cfg(unix)]
fn cps_local_borrows_print_and_resume_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "locals.jk",
        &format!(
            r#"{}
        fn main() @suspends {{
            let count = 42
            println(count)
            println(&count)
            yield_now()
            println(count + 1)
            println(&count)
            println(1.5)
            println(true)
            println({{}})
            let result = exercise()
            println(result)
        }}
    "#,
            include_str!("../fixtures/cps_local_borrows.jk")
        ),
    );
    work.assert_all_backends("locals.jk", &[], b"42\n42\n43\n42\n1.5\ntrue\n()\n42\n", 0);
}

#[test]
fn cps_local_borrows_allow_live_loans_at_suspension() {
    let work = Workspace::new();
    let bodies = [
        "let x = 42; let r = &x; yield_now(); println(r)",
        "let x = 42; let r = &x; let h = Holder { value: r }; yield_now(); println(h.value)",
        "let x = 42; read(x, later());",
        "assert(read(41, later()) == 42, \"temporary borrow survives argument suspension\")",
        "let x = 42; read(&x, later());",
        "let x = 42; consume(&x)",
        "let x = 42; join { | consume(&x) }",
        "let x = 42; let r = &x; scope { branch { yield_now() } }; println(r)",
        "let x = 42; let r = &x; scope { branch { println(r) } }",
    ];
    let mut source = String::from(
        r#"
        import joky/task.{yield_now}
        struct Holder { value: &Int32 }
        fn consume(value: &Int32) @suspends { yield_now(); assert(*value == 42, "borrowed input") }
        fn later() @suspends -> Int32 { yield_now(); 1 }
        fn read(value: &Int32, other: Int32) @noescape -> Int32 { *value + other }
    "#,
    );
    for (index, body) in bodies.iter().enumerate() {
        source.push_str(&format!("fn case_{index}() @suspends {{ {body} }}\n"));
    }
    source.push_str("fn main() @suspends {\n");
    for index in 0..bodies.len() {
        source.push_str(&format!("case_{index}()\n"));
    }
    source.push('}');
    work.write("locals.jk", &source);
    work.assert_all_backends("locals.jk", &[], b"42\n42\n42\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn automatic_shared_arguments_and_io_prelude_match_all_backends() {
    let work = Workspace::new();
    work.write(
        "automatic.jk",
        r#"
        import joky/show.{type Show}
        import joky/string.{type StringBuilder}
        struct Item { text: String }
        impl Show for Item {
            fn show(&self, output: &mut StringBuilder) @noescape { output.push_str(&self.text) }
        }
        struct Pair { code: Int32, text: String }
        struct Point { x: Int32, y: Int32 }
        fn inspect(value: &Pair) @noescape -> Int32 { value.code }
        fn sum(value: &Point) @noescape -> Int32 { value.x + value.y }
        fn first<T>(value: &T, ignored: Int32) @noescape -> String { "{ignored}" }
        fn read(value: &Int32, ignored: Int32) @noescape -> Int32 { (*value) }
        fn width(value: &UInt8) @noescape -> UInt8 { (*value) }
        fn add(value: &Int32, other: Int32) @noescape(value) -> Int32 { (*value) + other }
        fn noop(value: Unit) {}
        fn echo<T: Show>(value: &T) @noescape -> String { "<{value}>" }
        fn make() -> Item { println("make"); Item { text: "temporary" } }
        fn main() {
            let count = 42
            let message = "count={count}, next={count + 1}"
            println(message)
            println(count)
            println(count + 1)
            println("again={message}")
            println(&message)
            println(width(255))
            println(echo(41))
            println(echo::<Int64>(42))
            let item = Item { text: "kept" }
            println(item)
            println(item.text)
            println(item)
            println(make())
            let pair = Pair { code: 42, text: "kept" }
            println(inspect(pair))
            println(inspect(pair))
            println(sum(Point { x: 19, y: 23 }))
            var n = 1
            println(read(n, { n = 2; n }))
            println(n)
            println(first::<UInt8>(7, 8))
            println(add(40, 2))
            noop(let bound = "argument binding")
            println(bound)
            print("tail=")
            println(true)
            println(1.5)
            println('界')
            println({})
        }
    "#,
    );
    work.assert_all_backends("automatic.jk", &[], b"count=42, next=43\n42\n43\nagain=count=42, next=43\ncount=42, next=43\n255\n<41>\n<42>\nkept\nkept\nkept\nmake\ntemporary\n42\n42\n42\n1\n2\n8\n42\nargument binding\ntail=true\n1.5\n\xe7\x95\x8c\n()\n", 0);
    work.write(
        "async_auto.jk",
        r#"
        import joky/task.{yield_now}
        fn main() @suspends { println("before"); yield_now(); println("after") }
    "#,
    );
    work.assert_all_backends("async_auto.jk", &[], b"before\nafter\n", 0);
    work.write(
        "explicit_auto.jk",
        "import joky/io.{print, println} fn main() { print(42); println(43) }",
    );
    work.assert_all_backends("explicit_auto.jk", &["--no-prelude"], b"4243\n", 0);
    work.assert_all_backends("explicit_auto.jk", &[], b"4243\n", 0);
    work.write(
        "qualified_io.jk",
        "import joky/prelude as p fn main() { p.print(42); p.println(43) }",
    );
    work.assert_all_backends("qualified_io.jk", &["--no-prelude"], b"4243\n", 0);
    work.write("no_io.jk", "fn main() { println(42) }");
    for command in ["run", "build"] {
        failure(&work.cli(&[command, "--no-prelude", "no_io.jk"]), "println");
    }
    work.write(
        "shadow_io.jk",
        "fn println(value: Int32) -> Int32 { value } fn main() -> Int32 { println(42) }",
    );
    work.assert_all_backends("shadow_io.jk", &[], b"", 42);
}

#[test]
#[cfg(unix)]
fn automatic_shared_argument_temporaries_drop_at_the_call_boundary() {
    let work = Workspace::new();
    let definitions = r#"
        import joky/drop.{type Drop}
        struct Resource { text: String }
        impl Drop for Resource { fn drop(&mut self) { println("drop") } }
        fn make() -> Resource { println("make"); Resource { text: "owned" } }
        fn inspect(value: &Resource) @noescape -> String { value.text.clone() }
        fn explode(value: &Resource) @noescape { 1 / 0; }
        fn pair(value: &Resource, ignored: Int32) @noescape {}
        fn fail() -> Int32 { 1 / 0 }
    "#;
    work.write(
        "lifetime.jk",
        &format!("{definitions} fn main() {{ let result = inspect(make()); println(result) }}"),
    );
    work.assert_all_backends("lifetime.jk", &[], b"make\ndrop\nowned\n", 0);
    let failing_drop = definitions.replace("println(\"drop\") }", "println(\"drop\"); 1 / 0; }");
    for (definitions, entry) in [
        (definitions, "explode(make())"),
        (definitions, "pair(make(), fail())"),
        (
            failing_drop.as_str(),
            "let result = inspect(make()); println(result)",
        ),
    ] {
        work.write(
            "failure_auto.jk",
            &format!("{definitions} fn main() {{ {entry} }}"),
        );
        success(&work.cli(&["build", "failure_auto.jk"]));
        for output in [
            work.cli(&["run", "failure_auto.jk"]),
            work.cli(&["run", "--interpreter", "failure_auto.jk"]),
            Command::new(work.path().join("failure_auto"))
                .output()
                .unwrap(),
        ] {
            assert!(!output.status.success(), "{output:?}");
            assert_eq!(output.stdout, b"make\ndrop\n");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("zero"),
                "{output:?}"
            );
        }
    }
}

#[test]
fn automatic_shared_arguments_preserve_borrowing_boundaries() {
    let work = Workspace::new();
    for source in [
        "fn read(value: &Int32) -> &Int32 { value } fn main() { read(42); }",
        "fn edit(value: &mut Int32) @noescape {} fn main() { var n = 42; edit(n) }",
        "fn read(value: &Int32) @suspends {} fn main() @suspends { read(42) }",
        "fn read(value: &Int32) @noescape {} fn main() { let f: fn(&Int32) -> Unit = read; f(42) }",
        "fn read(value: &Int32) @noescape -> &Int32 { value } fn main() { read(42); }",
        "fn consume(value: String) {} fn main() { let text = \"owned\"; consume(text); println(text) }",
        "fn both(value: &String, other: &mut String) @noescape {} fn main() { var text = \"owned\"; both(text, &mut text) }",
    ] {
        work.write("contract.jk", source);
        let output = work.cli(&["build", "contract.jk"]);
        assert!(!output.status.success(), "{source}: {output:?}");
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"), "{source}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn show_print_from_sync_segment_in_cps_main_matches_all_backends() {
    let work = Workspace::new();
    work.write(
        "async.jk",
        r#"
        import joky/io.{println}
        import joky/task
        fn report(value: Int32) { let message = "value={value}"; println(&message) }
        fn main() @suspends { report(41); task.yield_now(); report(42) }
    "#,
    );
    work.assert_all_backends("async.jk", &[], b"value=41\nvalue=42\n", 0);
}

#[test]
fn legacy_print_hosts_are_rejected_without_publishing_output() {
    let work = Workspace::new();
    for (name, ty) in [
        ("print_int", "Int32"),
        ("print_int64", "Int64"),
        ("print_uint32", "UInt32"),
        ("print_uint64", "UInt64"),
        ("print_float64", "Float64"),
    ] {
        work.write(
            "legacy.jk",
            &format!("extern fn {name}(value: {ty}) fn main() {{}}"),
        );
        for args in [
            vec!["run", "legacy.jk"],
            vec!["run", "--interpreter", "legacy.jk"],
            vec!["build", "legacy.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, &format!("Unknown CLI host function `{name}`"));
            assert!(String::from_utf8_lossy(&output.stderr).contains("import joky/io"));
        }
        assert!(!work.path().join("legacy").exists());
        work.assert_no_build_files();
    }
    // Former host names remain ordinary user-defined function names.
    work.write(
        "ordinary.jk",
        "fn print_int(value: Int32) -> Int32 { value } fn main() -> Int32 { print_int(42) }",
    );
    for args in [
        vec!["run", "ordinary.jk"],
        vec!["run", "--interpreter", "ordinary.jk"],
    ] {
        assert_eq!(work.cli(&args).status.code(), Some(42));
    }
}

#[test]
#[cfg(unix)]
fn record_patterns_run_and_reject_invalid_patterns_on_all_frontends() {
    let work = Workspace::new();
    work.write(
        "record_patterns.jk",
        include_str!("../../examples/record_patterns.jk"),
    );
    work.assert_all_backends(
        "record_patterns.jk",
        &[],
        b"34\n13\n13\n2\nAda\n42\n11\nrest\n30\n31\n",
        0,
    );
    for (source, message) in [
        (
            "struct A { x: Int32 } struct B { x: Int32 } fn unused<T>(v: A) -> Int32 { let B { x } = v; x } fn main() {}",
            "requires B",
        ),
        (
            "struct Pair<T> { x: T } fn unused<T>(v: Pair<T>) -> T { let Pair::<Int32> { x } = v; x } fn main() {}",
            "requires Pair",
        ),
        (
            "struct Point { x: Int32, y: Int32 } fn main() { let Point { x } = Point { x: 1, y: 2 } }",
            "missing field",
        ),
        (
            "struct Point { x: Int32, y: Int32 } fn main() { let Point { x, x } = Point { x: 1, y: 2 } }",
            "duplicate",
        ),
        (
            "struct Point { x: Int32, y: Int32 } fn main() { let Point { x, z } = Point { x: 1, y: 2 } }",
            "no field `z`",
        ),
        (
            "struct Point { x: Int32, y: Int32 } struct Pair { a: Int32, b: Int32 } fn main() { let Point { x, y } = Pair { a: 1, b: 2 } }",
            "requires Point",
        ),
        (
            "struct Point { x: Int32, y: Int32 } fn main() { let Point { .., x } = Point { x: 1, y: 2 } }",
            "`..` must be the last",
        ),
        (
            "struct Point { x: Int32, y: Int32 } fn main() { let Point { x, .., y } = Point { x: 1, y: 2 } }",
            "`..` must be the last",
        ),
        (
            "struct User { name: String, score: Int32 } fn main() -> Int32 { let u = User { name: \"a\", score: 1 }; let User { score, .. } = u; u.score }",
            "moved or uninitialized",
        ),
    ] {
        work.write("invalid_pattern.jk", &format!("// 🙂\n{source}"));
        let output = work.cli(&["build", "invalid_pattern.jk"]);
        failure(&output, message);
        assert!(!work.path().join("invalid_pattern").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn record_patterns_borrow_drop_payloads_without_moving_them() {
    let work = Workspace::new();
    let source = r#"
        import joky/drop.{type Drop}
        struct Resource { name: String, score: Int32 }
        impl Drop for Resource { fn drop(&mut self) { println(99) } }
        enum Entry { Value(Resource) }
        fn inspect(value: &Entry) -> Int32 {
            match value { Value(Resource { name, score }) => int32(name.byte_length()) + score }
        }
        fn main() {
            let entry = Entry.Value(Resource { name: "hi", score: 40 })
            println(inspect(&entry))
            println(inspect(&entry))
        }
    "#;
    work.write("borrowed.jk", source);
    work.assert_all_backends("borrowed.jk", &[], b"42\n42\n99\n", 0);
    work.write(
        "moved.jk",
        &source
            .replace("value: &Entry", "value: Entry")
            .replace("inspect(&entry)", "inspect(entry)"),
    );
    failure(&work.cli(&["build", "moved.jk"]), "implementing Drop");
}

#[test]
#[cfg(unix)]
fn trait_sources_example_matches_all_backends() {
    let work = Workspace::new();
    work.write(
        "trait_sources.jk",
        include_str!("../../examples/trait_sources.jk"),
    );
    work.assert_all_backends("trait_sources.jk", &[], b"42\n", 0);
}

#[test]
#[cfg(unix)]
fn borrowed_dyn_example_matches_all_backends() {
    let work = Workspace::new();
    work.write(
        "borrowed_dyn.jk",
        include_str!("../../examples/borrowed_dyn.jk"),
    );
    work.assert_all_backends("borrowed_dyn.jk", &[], b"21\n42\n22\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn enum_same_name_members_execute_across_modules_and_backends() {
    let work = Workspace::new();
    work.write(
        "model.jk",
        r#"
        pub enum First<T> { Empty, Item(T), Unique }
        pub enum Second<T> { Empty, Item(T) }
        enum Hidden { Unique }
        pub enum Visible { Shared }
        enum Private { Shared }
        pub fn first() -> First<Int32> { First.Item(20) }
        pub fn second() -> Second<Int32> { Second.Item(22) }
        pub fn private_match() -> Int32 {
            match Private.Shared { Shared => 1 }
        }
        pub fn nested() -> Int32 {
            match First.Item(Second.Empty::<Int32>) {
                Item(Empty) => 1
                Item(Item(_)) => 0
                Empty => 0
                Unique => 0
            }
        }
    "#,
    );
    work.write(
        "left.jk",
        "import model pub fn make() -> model.First<Int32> { model.first() }",
    );
    work.write(
        "right.jk",
        "import model pub fn make() -> model.Second<Int32> { model.second() }",
    );
    work.write(
        "main.jk",
        r#"
        import model as m.{type First as A, type Second as B, Unique as Only, Shared}
        import left
        import right
        enum LocalA { Empty, Item(Int32), OnlyLocal }
        enum LocalB { Empty, Item(Int32) }
        fn main() -> Int32 {
            let a = match left.make() { A.Item(n) => n, A.Empty => 0, A.Unique => 0 }
            let b = match right.make() { B.Item(n) => n, B.Empty => 0 }
            let local = match LocalA.Item(LocalB.Item(1).value()) { Item(n) => n, Empty => 0, OnlyLocal => 0 }
            let local_unique = match OnlyLocal { OnlyLocal => 1, _ => 0 }
            let unique = match Only::<Int32> { A.Unique => 1, _ => 0 }
            let public = match Shared { m.Visible.Shared => 1 }
            let empty = match m.Second::<Int32>.Empty { Empty => 1, _ => 0 }
            let typed = match B.Item::<Int32>(1) { Item(n) => n, Empty => 0 }
            let Empty = 1
            let Item = |n: Int32| -> Int32 { n }
            if local + unique + public + empty + typed + m.private_match() + m.nested() + Empty + Item(1) + local_unique == 10 {
                a + b
            } else { 0 }
        }
        impl LocalB { fn value(self) -> Int32 { match self { Item(n) => n, Empty => 0 } } }
    "#,
    );
    work.assert_all_backends("main.jk", &[], b"", 42);
    work.assert_all_backends("main.jk", &["--no-prelude"], b"", 42);
}

#[test]
#[cfg(unix)]
fn enum_same_name_members_reject_ambiguous_uses_and_invalid_definitions() {
    let work = Workspace::new();
    let declarations = "pub enum A<T> { Empty, Item(T) } pub enum B<T> { Empty, Item(T) }";
    work.write("model.jk", declarations);
    for (source, message) in [
        (
            "import model fn main() { model.Empty::<Int32> }".to_owned(),
            "Ambiguous enum constructor",
        ),
        (
            "import model fn main() { model.Item(1) }".to_owned(),
            "Ambiguous enum constructor",
        ),
        (
            "import model.{Empty as Alias} fn main() {}".to_owned(),
            "Ambiguous enum constructor",
        ),
        (
            format!("{declarations} fn main() {{ Empty::<Int32> }}"),
            "Ambiguous enum constructor",
        ),
        (
            format!("{declarations} fn main() {{ Item(1) }}"),
            "Ambiguous enum constructor",
        ),
        (
            format!("{declarations} fn main() {{ let value: A<Int32> = Empty }}"),
            "Ambiguous enum constructor",
        ),
        (
            "enum A { Same } fn Same() -> Int32 { 1 } fn main() {}".to_owned(),
            "conflicts with an existing value",
        ),
        (
            "import model as Empty enum A { Empty } fn main() {}".to_owned(),
            "conflicts with an existing value",
        ),
        (
            "import model.{type A} enum A { Other } fn main() {}".to_owned(),
            "already defined",
        ),
        (
            "enum A { Same, Same } fn main() {}".to_owned(),
            "Duplicate or reserved variant",
        ),
        (
            "enum A { Same } const Same: Int32 = 1 fn main() {}".to_owned(),
            "already defined",
        ),
        (
            "import model fn main() { match model.A.Item(1) { model.B.Item(n) => n, _ => 0 } }"
                .to_owned(),
            "does not belong",
        ),
        (
            "import model fn main() { match model.A.Empty::<Int32> { model.Empty => 1, _ => 0 } }"
                .to_owned(),
            "does not belong",
        ),
    ] {
        work.write("bad.jk", &source);
        let output = work.cli(&["build", "bad.jk", "-o", "bad"]);
        assert!(!output.status.success(), "{source}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{source}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for body in ["Empty::<Int32>", "Item(1)"] {
        work.write(
            "model.jk",
            &format!("{declarations} pub fn bad() {{ {body} }}"),
        );
        work.write("bad.jk", "import model fn main() { model.bad() }");
        let output = work.cli(&["build", "bad.jk", "-o", "bad"]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Ambiguous enum constructor"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[cfg(unix)]
fn match_arm_separators_and_ungrouped_bitwise_or_run_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "arms.jk",
        r#"
        enum Choice { Empty, Value(Int32) }
        fn direct(value: Choice) -> Int32 {
            match value {
                Empty => 1 | 2
                Value(n) => match Choice.Empty { Empty => n | 8, Value(other) => other, },
            }
        }
        fn suspended(value: Choice) @suspends -> Int32 {
            match value { Empty => direct(Choice.Empty), Value(n) => { direct(Choice.Value(n)) } }
        }
        fn discard(value: Choice) {
            match value {
                Empty => 1;
                Value(n) => n;
            }
        }
        fn main() @suspends {
            discard(Choice.Empty)
            discard(Choice.Value(4))
            let first = direct(Choice.Empty)
            println(&first)
            let second = suspended(Choice.Value(4))
            println(&second)
        }
    "#,
    );
    work.assert_all_backends("arms.jk", &[], b"3\n12\n", 0);
    for (body, message) in [
        ("A => {} B => {}", "Expected"),
        ("A => {},, B => {}", "match arms cannot be empty"),
        ("| A => {} | B => {}", "Expected"),
    ] {
        work.write(
            "invalid.jk",
            &format!("enum E {{ A, B }} fn main() {{ match E.A {{ {body} }} }}"),
        );
        failure(&work.cli(&["build", "invalid.jk"]), message);
    }
}

#[test]
#[cfg(unix)]
fn arithmetic_assignment_and_remainder_run_on_all_backends() {
    let work = Workspace::new();
    let fixture = include_str!("../fixtures/arithmetic_assignment.jk");
    for suspends in [false, true] {
        let mut source = fixture.to_owned();
        if suspends {
            // Mark integer-returning entries; reference helpers remain synchronous.
            source = source
                .replace(") -> Int", ") @suspends -> Int")
                .replace(") -> UInt", ") @suspends -> UInt");
        }
        source.insert_str(0, "import joky/io.{println}\n");
        source.push_str(if suspends {
            "\nfn main() @suspends { let result = compute(); println(&result); let zero = constant(); println(&zero) }"
        } else {
            "\nfn main() { let result = compute(); println(&result); let zero = constant(); println(&zero) }"
        });
        work.write("arithmetic.jk", &source);
        work.assert_all_backends("arithmetic.jk", &[], b"42\n0\n", 0);
    }
    for (operation, message) in [
        ("%=", "Division by zero"),
        ("/=", "Division by zero"),
        (
            "<<=",
            "Shift count must be nonnegative and less than the integer width",
        ),
    ] {
        work.write("failure.jk", &format!("import joky/io.{{println}} fn main() {{ var x = 7; x {operation} 0{}; println(&x) }}", if operation == "<<=" { " + 32" } else { "" }));
        for args in [
            ["run", "failure.jk"].as_slice(),
            ["run", "--interpreter", "failure.jk"].as_slice(),
        ] {
            let output = work.cli(args);
            failure(&output, message);
            assert!(output.stdout.is_empty());
        }
        success(&work.cli(&["build", "failure.jk"]));
        let output = Command::new(work.path().join("failure")).output().unwrap();
        failure(&output, message);
        assert!(output.stdout.is_empty());
    }
}

#[test]
#[cfg(unix)]
fn conditional_patterns_run_on_all_backends() {
    let work = Workspace::new();
    let fixture = include_str!("../fixtures/conditional_patterns.jk")
        .replace(
            "import joky/check.{assert}",
            "import joky/check.{assert}\nimport joky/io.{println}",
        )
        .replace(
            "extern fn record(x: Int32) -> Int32",
            "fn record(x: Int32) -> Int32 { println(&x); x }",
        );
    for suspends in [false, true] {
        let mut source = fixture.clone();
        if suspends {
            source.insert_str(0, "import joky/task\n");
            source = source
                .replace("fn compute() -> Int32", "fn compute() @suspends -> Int32")
                .replace(
                    "fn choose(n: Int32) -> Option<Int32>",
                    "fn choose(n: Int32) @suspends -> Option<Int32>",
                )
                .replace(
                    "record(n); if n < 4",
                    "record(n); task.yield_now(); if n < 4",
                )
                .replace("fn owners() -> Int32", "fn owners() @suspends -> Int32")
                .replace("record(-value.id)", "task.yield_now(); record(-value.id)")
                .replace("i += 1", "i += 1; task.yield_now()");
        }
        source.push_str(if suspends {
            "\nfn main() @suspends { let result = compute(); println(&result) }"
        } else {
            "\nfn main() { let result = compute(); println(&result) }"
        });
        work.write("conditional.jk", &source);
        work.assert_all_backends(
            "conditional.jk",
            &[],
            b"0\n1\n2\n3\n4\n9\n0\n1\n-1\n1\n2\n3\n-4\n4\n5\n-6\n6\n42\n",
            0,
        );
    }
    let source = format!("import joky/task\n{fixture}\nfn main() @suspends {{ if let Some(value) = Some(owned(9)) {{ task.yield_now(); let invalid = 1 % 0 }}; record(999); }}");
    work.write("pattern_failure.jk", &source);
    for args in [
        ["run", "pattern_failure.jk"].as_slice(),
        ["run", "--interpreter", "pattern_failure.jk"].as_slice(),
    ] {
        let output = work.cli(args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Division by zero"),
            "{output:?}"
        );
        assert_eq!(output.stdout, b"9\n");
    }
    success(&work.cli(&["build", "pattern_failure.jk"]));
    let output = Command::new(work.path().join("pattern_failure"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Division by zero"),
        "{output:?}"
    );
    assert_eq!(output.stdout, b"9\n");
}

#[test]
#[cfg(unix)]
fn tuple_members_run_on_all_backends() {
    let work = Workspace::new();
    let fixture = include_str!("../fixtures/tuple_members.jk");
    for suspends in [false, true] {
        let mut source = fixture.to_string();
        if suspends {
            source.insert_str(0, "import joky/task\n");
            source = source
                .replace("fn compute() -> Int32", "fn compute() @suspends -> Int32")
                .replace(
                    "fn delta() -> Int32 { 2 }",
                    "fn delta() @suspends -> Int32 { task.yield_now(); 2 }",
                );
        }
        source.push_str(if suspends {
            "\nfn main() @suspends { let answer = compute(); println(&answer) }"
        } else {
            "\nfn main() { let answer = compute(); println(&answer) }"
        });
        work.write("tuple_members.jk", &source);
        work.assert_all_backends("tuple_members.jk", &[], b"2\n1\n3\n42\n", 0);
    }
    for (source, message) in [
        ("fn main() { let pair = (1, 2); let invalid = pair.2 }", "out of range"),
        ("fn main() { let pair = (1,); let invalid = pair.18446744073709551615 }", "out of range"),
        ("fn main() { let pair = (1, 2); pair.0 = 3 }", "immutable"),
        ("fn main() { var pair = (1, 2); let view = &pair; view.0 = 3 }", "shared reference"),
        ("fn main() { var pair = (1, 2); pair.0 = true }", "expects Int32"),
        ("fn main() { let pair = (1, 2); let invalid = pair.0x1 }", "decimal digits"),
        ("fn main() { let pair = (1, 2); let invalid = pair.1_0 }", "decimal digits"),
        ("fn main() { let pair = (1, 2); let invalid = pair.-1 }", "Expected an identifier"),
        ("fn main() { let pair = (1, 2); let invalid = pair.name }", "Tuple member"),
        ("fn main() { let n = 42; let invalid = n.0 }", "has no fields"),
        ("fn main() { let pair = (1, 2); let invalid = pair.18446744073709551616 }", "UInt64 range"),
        ("fn main() { var pair = (1, 2); let view = &pair.0; pair.0 = 9; let used = *view }", "borrow"),
        ("struct Hidden { value: Int32 } fn main() { let pair = (Hidden { value: 1 },); let invalid = pair.0.1 }", "Unknown field"),
        ("import joky/array.{type Array} fn main() { let pair = (Array.new::<Int32>(), 2); let moved = pair.0; let invalid = pair.0 }", "moved"),
        ("import joky/array.{type Array} fn main() { let pair = (Array.new::<Int32>(), 2); let view = &pair; let invalid = view.0 }", "owning field"),
        ("import joky/array.{type Array} import joky/drop.{type Drop} struct R { values: Array<Int32> } impl Drop for R { fn drop(&mut self) {} } fn main() { let pair = (R { values: Array.new::<Int32>() },); let invalid = pair.0.values }", "implementing Drop"),
    ] {
        work.write("invalid_tuple_member.jk", source);
        failure(&work.cli(&["build", "invalid_tuple_member.jk"]), message);
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn pure_enum_contract_limits_and_rejections() {
    for count in [63, 64] {
        let variants = (0..count)
            .map(|i| format!("V{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let work = Workspace::new();
        work.write("limit.jk", &format!("import joky/domain enum E{{{variants}}} struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn main() @suspends {{let owner=domain.create(make,0); let h=owner.handle(); when(h){{E.V0}}.unwrap();}}"));
        // One tag plus every variant counts, even with zero payload lanes.
        if count == 63 {
            work.assert_all_backends("limit.jk", &[], b"", 0);
        } else {
            failure(&work.cli(&["build", "limit.jk"]), "bounded value");
        }
    }
    for depth in [8, 9] {
        let mut definitions = String::new();
        let mut previous = "Int32".to_string();
        for i in 0..depth {
            definitions.push_str(&format!("enum E{i}{{Value({previous})}} "));
            previous = format!("E{i}");
        }
        let mut value = "42".to_string();
        for i in 0..depth {
            value = format!("E{i}.Value({value})");
        }
        let work = Workspace::new();
        work.write("depth.jk", &format!("import joky/domain {definitions} struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn main() @suspends {{let owner=domain.create(make,0); let h=owner.handle(); when(h){{{value}}}.unwrap();}}"));
        if depth == 8 {
            work.assert_all_backends("depth.jk", &[], b"", 0);
        } else {
            failure(&work.cli(&["build", "depth.jk"]), "bounded value");
        }
    }
    for (definitions, ty, value) in [
        (
            "import joky/option.{type Option}",
            "Option<vm.Vm>",
            "Option.None::<vm.Vm>",
        ),
        ("enum E{Empty,Value(vm.Vm)}", "E", "E.Empty"),
        (
            "enum Phantom<T>{Empty}",
            "Phantom<vm.Vm>",
            "Phantom.Empty::<vm.Vm>",
        ),
        ("enum E{Empty,Value(Ptr)}", "E", "E.Empty"),
        ("enum E{Empty,Value(fn()->Int32)}", "E", "E.Empty"),
    ] {
        let work = Workspace::new();
        work.write("reject.jk",&format!("import joky/vm {definitions} fn main() @suspends {{let h=vm.Vm.new(); h.call::<{ty},Int32>(\"echo\",{value});}}"));
        failure(
            &work.cli(&["build", "reject.jk"]),
            "Script contracts require",
        );
    }
}

#[test]
#[cfg(unix)]
fn pure_composite_contract_limits_and_rejections() {
    for element in ["Int32", "Unit"] {
        for count in [64, 65] {
            let ty = format!("({})", vec![element; count].join(","));
            let value = format!(
                "({})",
                vec![if element == "Unit" { "{}" } else { "1" }; count].join(",")
            );
            let work = Workspace::new();
            work.write("limit.jk",&format!("import joky/domain struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn main() @suspends {{let owner=domain.create(make,0); let h=owner.handle(); let result:{ty}=when(h){{{value}}}.unwrap();}}"));
            if count == 64 {
                work.assert_all_backends("limit.jk", &[], b"", 0);
            } else {
                failure(&work.cli(&["build", "limit.jk"]), "bounded value");
            }
        }
    }
    // A phantom argument still contributes its complete contract and depth.
    for depth in [8, 9] {
        let mut argument = "Bool".to_owned();
        for _ in 1..depth {
            argument = format!("Marker<{argument}>");
        }
        let work = Workspace::new();
        work.write("depth.jk", &format!("import joky/domain struct Marker<T>{{}} struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn main() @suspends {{let owner=domain.create(make,0); let h=owner.handle(); when(h){{Marker::<{argument}>{{}}}}.unwrap();}}"));
        if depth == 8 {
            work.assert_all_backends("depth.jk", &[], b"", 0);
        } else {
            failure(&work.cli(&["build", "depth.jk"]), "bounded value");
        }
    }
    for depth in [8, 9] {
        let mut value = "42".to_owned();
        for _ in 0..depth {
            value = format!("({value},)");
        }
        let work = Workspace::new();
        work.write("tuple_depth.jk", &format!("import joky/domain struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn main() @suspends {{let owner=domain.create(make,0); let h=owner.handle(); when(h){{{value}}}.unwrap();}}"));
        if depth == 8 {
            work.assert_all_backends("tuple_depth.jk", &[], b"", 0);
        } else {
            failure(&work.cli(&["build", "tuple_depth.jk"]), "bounded value");
        }
    }
    // The generated capture record consumes one member in addition to its tuple.
    for count in [63, 64] {
        let value = format!("({})", vec!["{}"; count].join(","));
        let work = Workspace::new();
        work.write("capture.jk",&format!("import joky/domain struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn main() @suspends {{let owner=domain.create(make,0); let h=owner.handle(); let captured={value}; when(h){{captured}}.unwrap();}}"));
        if count == 63 {
            work.assert_all_backends("capture.jk", &[], b"", 0);
        } else {
            failure(&work.cli(&["build", "capture.jk"]), "when captures require");
        }
    }
    for (ty, value) in [
        ("(Int32,vm.Vm)", "(1,vm.Vm.new())"),
        ("Cell<vm.Vm>", "Cell{value:vm.Vm.new()}"),
        ("Marker<vm.Vm>", "Marker::<vm.Vm>{}"),
        ("Marker<(Int32,Ptr)>", "Marker::<(Int32,Ptr)>{}"),
        ("Marker<fn()->Int32>", "Marker::<fn()->Int32>{}"),
        ("Marker<&Int32>", "Marker::<&Int32>{}"),
    ] {
        let work = Workspace::new();
        work.write("reject.jk",&format!("import joky/vm struct Cell<T>{{value:T}} struct Marker<T>{{}} fn main() @suspends {{let h=vm.Vm.new(); h.call::<{ty},Int32>(\"echo\",{value});}}"));
        failure(
            &work.cli(&["build", "reject.jk"]),
            "Script contracts require",
        );
    }
}

#[test]
#[cfg(unix)]
fn raw_and_multiline_strings_match_all_backends() {
    let work = Workspace::new();
    // Mixed physical line endings and tabs exercise source normalization in
    // generated code as well as the lexer-only tests.
    let source = include_str!("../fixtures/raw_and_multiline_strings.jk").replace(
        "    let r = 42",
        "    let mixed = \"\"\"\r\n\t a\r\n \t\r\t   b\r\n\t \"\"\"\n    expect(mixed, \"a\\n\\n  b\")\n    let r = 42",
    );
    work.write("strings.jk", &source);
    work.assert_all_backends("strings.jk", &[], b"raw-and-multiline-ok\n", 0);
}

#[test]
#[cfg(unix)]
fn nested_block_comments_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "comments.jk",
        r##"/* outer
            /* nested /* deeply nested */ */
            // quotes and braces are comment text: " ' }
        */
        import joky/check
        // /* ignored line comment
        fn main() {
            var value = 84/**// /* nested */2
            value /* before operator */ /= /* after operator */ 2
            check.assert(value == 21, "division")
            let pair = (value, 42)
            println(pair./* member */1)
            println("/* literal */")
            println(r#"/* raw */"#)
            check.assert('/' != '*', "character literals")
            println("{/* } \" /* nested */ */ pair.1}")
            var total = 1/* newline separates expressions
            */total = total + 1
            check.assert(total == 2, "comment newline")
        }/**/"##,
    );
    work.assert_all_backends("comments.jk", &[], b"42\n/* literal */\n/* raw */\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn unterminated_block_comments_preserve_file_and_line() {
    let work = Workspace::new();
    for source in [
        "// 🙂\n  /* outer\n/* inner */",
        "fn main() {\n    println(\"{/* outer /* inner */ }\")\n}",
    ] {
        work.write("invalid.jk", source);
        for args in [
            vec!["build", "invalid.jk"],
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "Unterminated block comment");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("invalid.jk:2:"), "{stderr}");
        }
    }
}

#[test]
#[cfg(unix)]
fn indexing_executes_in_all_backends() {
    let work = Workspace::new();
    work.write("indexing.jk", include_str!("../fixtures/indexing.jk"));
    work.assert_all_backends("indexing.jk", &[], b"indexing ok\n", 0);
}

#[test]
#[cfg(unix)]
fn indexing_cleanup_and_bounds_run_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/indexing_cleanup.jk").replace(
        "extern fn note(value: Int32)",
        "fn note(value: Int32) { println(value) }",
    );
    work.write("index_cleanup.jk", &source);
    work.assert_all_backends(
        "index_cleanup.jk",
        &[],
        b"1\n2\n3\n-1\n4\n-2\n-4\n-3\n42\n",
        0,
    );
    let declarations = source.split("fn main()").next().unwrap();
    for (entry, stdout, message) in [
        ("failed_index", "1\n-1\n", "outside the initialized prefix"),
        ("failed_rhs", "1\n2\n-2\n-1\n", "Division by zero"),
        ("failed_drop", "1\n2\n-1\n-2\n", "Division by zero"),
    ] {
        work.write(
            "index_failure.jk",
            &format!("{declarations}\nfn main() {{ {entry}(); }}"),
        );
        let mut outputs = vec![
            work.cli(&["run", "index_failure.jk"]),
            work.cli(&["run", "--interpreter", "index_failure.jk"]),
        ];
        success(&work.cli(&["build", "index_failure.jk"]));
        outputs.push(
            Command::new(work.path().join("index_failure"))
                .output()
                .unwrap(),
        );
        for (backend, output) in outputs.into_iter().enumerate() {
            assert_eq!(output.status.code(), Some(1), "{entry}: {output:?}");
            assert_eq!(output.stdout, stdout.as_bytes(), "{entry}: {output:?}");
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(message), "{entry}: {output:?}");
            if backend < 2 {
                assert!(error.contains("index_failure.jk:"), "{entry}: {output:?}");
                if entry == "failed_index" {
                    assert!(error.contains("values[2]"), "{output:?}");
                }
            } else {
                assert!(error.contains("bytes "), "{entry}: {output:?}");
                if entry == "failed_index" {
                    let start = declarations.find("values[2]").unwrap();
                    assert!(
                        error.contains(&format!("bytes {start}..{}", start + "values[2]".len())),
                        "{output:?}"
                    );
                }
            }
        }
    }
    for body in [
        "let a: Array<Int32> = []; a[0];",
        "let a: FixedArray<Int32, 0> = []; a[0];",
        "let a = [1]; let s = a.slice(0, 0); s[0];",
        "var a = [1]; let s = a.slice_mut(0, 0); s[0] = 2;",
        "var a: FixedArray<Int32, 1> = [1]; a[1] = 2;",
        "let a = [{}, {}]; a[2];",
    ] {
        work.write("index_bounds.jk", &format!("import joky/array.{{type Array}} import joky/fixed_array.{{type FixedArray}}\nfn main() {{ {body} }}"));
        let mut outputs = vec![
            work.cli(&["run", "index_bounds.jk"]),
            work.cli(&["run", "--interpreter", "index_bounds.jk"]),
        ];
        success(&work.cli(&["build", "index_bounds.jk"]));
        outputs.push(
            Command::new(work.path().join("index_bounds"))
                .output()
                .unwrap(),
        );
        for (backend, output) in outputs.into_iter().enumerate() {
            assert_eq!(output.status.code(), Some(1), "{body}: {output:?}");
            let error = String::from_utf8_lossy(&output.stderr);
            if backend < 2 {
                assert!(error.contains("index_bounds.jk:2:"), "{body}: {output:?}");
                assert!(error.contains("[runtime_error]"), "{body}: {output:?}");
            } else {
                assert!(error.contains("bytes "), "{body}: {output:?}");
            }
        }
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn postfix_unwrap_matches_all_backends() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/postfix_unwrap.jk");
    work.write(
        "postfix.jk",
        &format!("import joky/io.{{println}}\n{source}\nfn main() {{ println(check()) }}"),
    );
    work.assert_all_backends("postfix.jk", &["--no-prelude"], b"42\n", 0);
    for (entry, message) in [
        ("fail_option", "Postfix `!` on None"),
        ("fail_result", "Postfix `!` on Err"),
    ] {
        work.write(
            "postfix.jk",
            &format!("{source}\nfn main() {{ {entry}(); }}"),
        );
        let check = |output: std::process::Output| {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(message), "{stderr}");
            assert!(
                stderr.contains("postfix.jk:") || stderr.contains("bytes "),
                "{stderr}"
            );
        };
        check(work.cli(&["run", "postfix.jk"]));
        check(work.cli(&["run", "--interpreter", "postfix.jk"]));
        success(&work.cli(&["build", "postfix.jk"]));
        check(Command::new(work.path().join("postfix")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn postfix_unwrap_cleanup_matches_native_execution() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/postfix_cleanup.jk").replace(
        "extern fn record(n: Int32) -> Int32",
        "import joky/io.{println}\nfn record(n: Int32) -> Int32 { println(n); n }",
    );
    work.write(
        "cleanup.jk",
        &format!("{source}\nfn main() {{ record(check()); }}"),
    );
    work.assert_native(
        "cleanup.jk",
        &[],
        b"-1\n-42\n42\n1\n-1\n-7\n1\n7\n99\n42\n",
        0,
    );
    work.write(
        "cleanup.jk",
        &format!("{source}\nfn main() {{ force_failure(); }}"),
    );
    success(&work.cli(&["build", "cleanup.jk"]));
    let output = Command::new(work.path().join("cleanup")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"-1\n-7\n7\n1\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Postfix `!` on Err"));
}

#[test]
#[cfg(unix)]
fn postfix_unwrap_tasks_and_closures_match_native_execution() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/postfix_tasks.jk").replace(
        "extern fn record(n: Int32) -> Int32",
        "import joky/io.{println}\nfn record(n: Int32) -> Int32 { println(n); n }",
    );
    work.write("tasks.jk", &format!("{source}\nfn main() @suspends {{ record(check()); record(closure_check()); println(inferred(\"context\")!) }}"));
    work.assert_native("tasks.jk", &[], b"10\n20\n10\n30\n42\n42\n42\n", 0);
}

#[test]
fn suspending_borrows_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "borrows.jk",
        include_str!("../fixtures/suspending_borrows.jk"),
    );
    work.assert_native("borrows.jk", &[], b"", 0);
}

#[test]
fn suspending_borrows_cleanup_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/suspending_borrow_cleanup.jk").replace(
        "extern fn record_drop(id: Int32)",
        "fn record_drop(id: Int32) { println(id) }",
    );
    work.write("cleanup.jk", &source);
    work.assert_native(
        "cleanup.jk",
        &[],
        b"2\n42\n1\n2\n42\n2\n42\n2\n42\n2\n42\n",
        0,
    );
}

#[test]
#[cfg(unix)]
fn for_indexed_executes_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/for_indexed.jk");
    for suspends in [false, true] {
        let source = if suspends {
            source
                .replace("fn compute() ->", "fn compute() @suspends ->")
                .replace(
                    "if i == 1 { continue }",
                    "task.yield_now(); if i == 1 { continue }",
                )
        } else {
            source.to_owned()
        };
        let effect = if suspends { "@suspends" } else { "" };
        work.write("indexed.jk", &format!(
            "import joky/io.{{println}}\nimport joky/task\n{source}\nfn main() {effect} {{ println(compute()); println(boundary(128)); println(unsigned_boundary(256)); }}"
        ));
        success(&work.cli(&["build", "--no-prelude", "indexed.jk"]));
        let output = Command::new(work.path().join("indexed")).output().unwrap();
        success(&output);
        assert_eq!(output.stdout, b"42\n127\n255\n");
    }
    for body in [
        "boundary(129)",
        "unsigned_boundary(257)",
        "overflow_owned()",
    ] {
        work.write("overflow.jk", &format!("{source}\nfn main() {{ {body}; }}"));
        success(&work.cli(&["build", "--no-prelude", "overflow.jk"]));
        let output = Command::new(work.path().join("overflow")).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("For index overflow"));
    }
}

#[test]
#[cfg(unix)]
fn ranges_execute_boundaries_and_suspending_endpoints_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/ranges.jk")
        .replace(
            "extern fn record(x: Int32) -> Int32",
            "fn record(x: Int32) -> Int32 { println(x); x }",
        )
        .replace(
            "extern fn delayed_value(x: Int32) @suspends -> Int32",
            "fn delayed_value(x: Int32) @suspends -> Int32 { record(x); task.yield_now(); x + 1 }",
        );
    let source = format!("import joky/io.{{println}}\nimport joky/task\n{source}");
    work.write(
        "ranges.jk",
        &format!("{source}\nfn main() @suspends {{ println(compute()); println(pending()); }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "ranges.jk"]));
    let output = Command::new(work.path().join("ranges")).output().unwrap();
    success(&output);
    assert_eq!(
        output.stdout,
        b"1\n3\n42\n-99\n0\n0\n2\n2\n11\n11\n12\n12\n13\n13\n99\n42\n"
    );
    work.write(
        "failure.jk",
        &format!("{source}\nfn main() {{ endpoint_failure() }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "failure.jk"]));
    let output = Command::new(work.path().join("failure")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"7\n99\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Division by zero"));
}

#[test]
fn range_slices_execute_views_suspension_and_checked_closed_bounds_in_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/range_slices.jk")
        .replace(
            "extern fn record(x: Int32) -> Int32",
            "fn record(x: Int32) -> Int32 { println(x); x }",
        )
        .replace(
            "extern fn delayed_value(x: Int32) @suspends -> Int32",
            "fn delayed_value(x: Int32) @suspends -> Int32 { record(x); task.yield_now(); x + 1 }",
        );
    let source = format!("import joky/io.{{println}}\nimport joky/task\n{source}");
    work.write(
        "slices.jk",
        &format!("{source}\nfn main() @suspends {{ println(compute()); println(pending()); }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "slices.jk"]));
    let output = Command::new(work.path().join("slices")).output().unwrap();
    success(&output);
    assert_eq!(
        output.stdout,
        b"0\n1\n2\n42\n-99\n-1\n-1\n0\n0\n4\n4\n99\n42\n"
    );
    work.write(
        "failure.jk",
        &format!("{source}\nfn main() {{ bad_max() }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "failure.jk"]));
    let output = Command::new(work.path().join("failure")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"99\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid inclusive slice bounds"));
}

#[test]
fn range_patterns_execute_integer_domains_owned_matches_and_suspension_in_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/range_patterns.jk")
        .replace(
            "extern fn record(x: Int32) -> Int32",
            "fn record(x: Int32) -> Int32 { println(x); x }",
        )
        .replace(
            "extern fn delayed_value(x: Int32) @suspends -> Int32",
            "fn delayed_value(x: Int32) @suspends -> Int32 { record(x); task.yield_now(); x + 1 }",
        );
    let source = format!("import joky/io.{{println}}\nimport joky/task\n{source}");
    work.write(
        "patterns.jk",
        &format!("{source}\nfn main() @suspends {{ println(compute()); println(pending()); }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "patterns.jk"]));
    let output = Command::new(work.path().join("patterns")).output().unwrap();
    success(&output);
    assert_eq!(
        output.stdout,
        b"21\n31\n22\n23\n2\n42\n-99\n1\n1\n40\n40\n99\n42\n"
    );
    work.write(
        "failure.jk",
        &format!("{source}\nfn main() {{ failure() }}"),
    );
    success(&work.cli(&["build", "--no-prelude", "failure.jk"]));
    let output = Command::new(work.path().join("failure")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"3\n99\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("range failure"));
}

#[test]
#[cfg(unix)]
fn iterator_adapters_execute_and_clean_up_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/iterator_adapters.jk").replace(
        "extern fn record(x: Int32) -> Int32",
        "import joky/io.{println}\nfn record(x: Int32) -> Int32 { println(x); x }",
    );
    for (name, entry, expected, failure) in [
        (
            "adapters",
            "fn main() @suspends { println(compute()); println(fields()); lazy(); cleanup(); zip_cleanup(); println(early()); println(pending()) }",
            "42\n42\n1\n2\n3\n4\n13\n14\n13\n14\n13\n20\n21\n30\n31\n32\n1\n2\n3\n9\n1\n3\n2\n1\n2\n9\n1\n14\n3\n1\n16\n4\n2\n42\n",
            None,
        ),
        ("map_failure", "fn main() { fail_map() }", "1\n2\n9\n", Some("Map callback failure")),
        ("filter_failure", "fn main() { fail_filter() }", "1\n2\n9\n", Some("Filter callback failure")),
    ] {
        let file = format!("{name}.jk");
        work.write(&file, &format!("{source}\n{entry}"));
        success(&work.cli(&["build", "--no-prelude", &file]));
        let output = Command::new(work.path().join(name)).output().unwrap();
        if let Some(message) = failure {
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        } else {
            success(&output);
        }
        assert_eq!(output.stdout, expected.as_bytes(), "{name}");
    }
}

#[test]
fn service_ergonomics_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "service_clone_model.jk",
        include_str!("../fixtures/service_clone_model.jk"),
    );
    work.write(
        "ergonomics.jk",
        include_str!("../fixtures/service_ergonomics.jk"),
    );
    work.assert_all_backends("ergonomics.jk", &[], b"", 0);
}

#[test]
fn copy_projections_survive_temporary_owner_cleanup_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "temporary_projection.jk",
        r#"import joky/check
struct State { name:String, number:UInt64 }
fn make() -> State {State{name:"temporary",number:500}}
fn direct() -> UInt64 {make().number}
fn wrapped() @suspends -> Result<State,Int32> {Ok(make())}
fn project() @suspends -> UInt64 {wrapped()!.number}
fn main() @suspends {
    check.assert(direct()==500,"synchronous temporary")
    check.assert(project()==500,"suspended enum payload temporary")
}"#,
    );
    work.assert_all_backends("temporary_projection.jk", &[], b"", 0);
}

#[test]
fn mutable_parameters_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "mutable.jk",
        include_str!("../fixtures/mutable_parameters.jk"),
    );
    work.assert_native("mutable.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn contextual_calls_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "context.jk",
        include_str!("../fixtures/contextual_calls.jk"),
    );
    work.assert_native("context.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn receiver_reservations_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "receivers.jk",
        include_str!("../fixtures/receiver_reservations.jk"),
    );
    work.assert_native("receivers.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn string_equality_executes_in_standalone_aot_without_prelude() {
    let work = Workspace::new();
    work.write(
        "equality.jk",
        include_str!("../fixtures/string_equality.jk"),
    );
    work.assert_native("equality.jk", &["--no-prelude"], b"", 0);
    work.write(
        "comparison.jk",
        r#"pub fn matches(a: &String, b: &String) @noescape -> Bool { a == b }"#,
    );
    work.write(
        "imported_equality.jk",
        r#"
        import comparison
        fn main() -> Int32 { if comparison.matches("中🙂", "中🙂") { 42 } else { 0 } }
    "#,
    );
    work.assert_all_backends("imported_equality.jk", &["--no-prelude"], b"", 42);
}

#[test]
#[cfg(unix)]
fn borrowed_queries_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "queries.jk",
        include_str!("../fixtures/borrowed_queries.jk"),
    );
    work.assert_native("queries.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn shared_combinators_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "shared.jk",
        include_str!("../fixtures/shared_combinators.jk"),
    );
    work.assert_native("shared.jk", &["--no-prelude"], b"", 0);
    work.write(
        "adapters_example.jk",
        include_str!("../../examples/iterator_adapters.jk"),
    );
    for args in [
        vec!["run", "adapters_example.jk"],
        vec!["run", "--interpreter", "adapters_example.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(
            output.stdout,
            b"0: 10\n1: 12\n2: 14\nAlice: 80\nBob: 90\nAlice\nBob\n4\n"
        );
    }
    work.assert_native(
        "adapters_example.jk",
        &[],
        b"0: 10\n1: 12\n2: 14\nAlice: 80\nBob: 90\nAlice\nBob\n4\n",
        0,
    );
}

#[test]
#[cfg(unix)]
fn shared_pipeline_executes_in_standalone_aot_without_prelude() {
    let work = Workspace::new();
    work.write(
        "pipeline.jk",
        &include_str!("../fixtures/shared_pipeline.jk").replace(
            "extern fn record(value: Int32) -> Int32",
            "fn record(value: Int32) -> Int32 { value }",
        ),
    );
    work.assert_native("pipeline.jk", &["--no-prelude"], b"", 0);
}

#[test]
#[cfg(unix)]
fn exclusive_pipeline_executes_in_standalone_aot_without_prelude() {
    let work = Workspace::new();
    work.write(
        "pipeline.jk",
        &include_str!("../fixtures/exclusive_pipeline.jk").replace(
            "extern fn record(value: Int32) -> Int32",
            "fn record(value: Int32) -> Int32 { value }",
        ),
    );
    work.assert_native("pipeline.jk", &["--no-prelude"], b"", 0);
}

#[test]
#[cfg(unix)]
fn borrowed_pipelines_example_matches_all_backends() {
    let work = Workspace::new();
    work.write(
        "pipelines.jk",
        include_str!("../../examples/borrowed_pipelines.jk"),
    );
    work.assert_all_backends(
        "pipelines.jk",
        &["--no-prelude"],
        b"0: 5\n1: 3\n4\n11\n0\n12\n3\n",
        0,
    );
    work.write(
        "protocol.jk",
        include_str!("../../examples/shared_iterator_protocol.jk"),
    );
    work.assert_all_backends("protocol.jk", &["--no-prelude"], b"2\nAlice\nBob\n4\n", 0);
    work.write(
        "captured.jk",
        include_str!("../../examples/captured_pipeline.jk"),
    );
    work.assert_all_backends("captured.jk", &["--no-prelude"], b"80\n65\n85\n70\n4\n", 0);
}

#[test]
#[cfg(unix)]
fn captured_pipeline_executes_in_standalone_aot_without_prelude() {
    let work = Workspace::new();
    work.write(
        "captured.jk",
        &include_str!("../fixtures/captured_pipeline.jk").replace(
            "extern fn record(value: Int32) -> Int32",
            "fn record(value: Int32) -> Int32 { value }",
        ),
    );
    work.assert_native("captured.jk", &["--no-prelude"], b"", 0);
}

#[test]
fn borrowed_pipeline_diagnostics_explain_limits_at_user_sources() {
    let work = Workspace::new();
    for (body, message) in [
        (r#"fn f(v: &String) @noescape -> String { "new" } fn main() { let a = ["a"]; for _ in iter.shared(&a).map(f) {} }"#, "does not satisfy Copy bound"),
        ("fn f(v: &Int32) @noescape -> &Int32 { alloc(1) } fn main() { let a = [1]; for _ in iter.shared(&a).map(f) {} }", "reference-free Copy storage value"),
        ("fn main() { let a = [1]; var count = 0; for _ in iter.shared(&a).map(|n| { count += 1; *n + count }) {} }", "does not satisfy Copy bound"),
        ("fn main() { var a = [1]; let offset = 1; let keep = |n: &Int32| { *n > offset }; for _ in iter.exclusive(&mut a).filter(keep) {} }", "A capturing closure cannot be used as a function pointer"),
        ("fn accept(callback: fn(Int32) -> Bool) {} fn main() { let offset = 1; accept(|n| { n > offset }) }", "A capturing closure cannot be used as a function pointer"),
        ("fn f(v: &Int32) -> Bool { true } fn main() { var a = [1]; for _ in iter.exclusive(&mut a).filter(f) {} }", "Use an @noescape callback"),
        ("fn f(v: &Int32) @suspends -> Int32 { *v } fn main() { let a = [1]; for _ in iter.shared(&a).map(f) {} }", "synchronous callback without @suspends"),
        ("fn main() { var a = [1]; let p = iter.exclusive(&mut a); let q = p; for _ in p {}; for _ in q {} }", "Use of a moved, sealed or uninitialized exclusive reference"),
        ("fn main() @suspends { var a = [1]; for _ in iter.exclusive(&mut a) {} }", "Keep borrowed pipelines in a synchronous helper"),
    ] {
        work.write("invalid.jk", &format!("import joky/array.{{type Array}}\nimport joky/iter\n{body}\n"));
        let output = work.cli(&["build", "invalid.jk"]);
        failure(&output, message);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid.jk:3:"), "{stderr}");
        assert!(!stderr.contains("joky/iter.jk:"), "{stderr}");
        assert!(!work.path().join("invalid").exists());
    }
}

#[test]
#[cfg(unix)]
fn borrowed_pipeline_callback_failures_stop_standalone_aot() {
    let work = Workspace::new();
    for fixture in [
        include_str!("../fixtures/shared_pipeline.jk"),
        include_str!("../fixtures/exclusive_pipeline.jk"),
        include_str!("../fixtures/captured_pipeline.jk"),
    ] {
        let source = fixture
            .replace(
                "extern fn record(value: Int32) -> Int32",
                "fn record(value: Int32) -> Int32 { value }",
            )
            .replace("fn main()", "fn fixture_main()");
        work.write("failure.jk", &format!("{source}\nfn main() {{ failed() }}"));
        success(&work.cli(&["build", "--no-prelude", "failure.jk"]));
        let output = Command::new(work.path().join("failure")).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("failed"));
    }
}

#[test]
#[cfg(unix)]
fn flat_expression_chains_keep_evaluation_order_on_all_backends() {
    let work = Workspace::new();
    let mut sum = vec!["1"; 1000];
    sum[0] = "mark(&mut counter, 0)";
    sum[500] = "mark(&mut counter, 1)";
    sum[999] = "mark(&mut counter, 2)";
    let branches = (0..300)
        .map(|index| match index {
            0 => "if mark(&mut counter, 0) == needle { 0 }".to_owned(),
            298 => "if needle == 298 && mark(&mut counter, 1) == 1 { 298 }".to_owned(),
            299 => "if mark(&mut counter, 99) == needle { 299 }".to_owned(),
            _ => format!("if needle == {index} {{ {index} }}"),
        })
        .collect::<Vec<_>>()
        .join(" else ");
    let sum = sum.join(" + ");
    let source = format!(
        r#"
        fn mark(counter: &mut Int32, expected: Int32) -> Int32 {{
            assert_eq(*counter, expected)
            *counter += 1
            expected
        }}
        fn choose(needle: Int32) -> Int32 {{
            var counter = 0
            let selected = {branches} else {{ -1 }}
            assert_eq(counter, 2)
            selected
        }}
        fn main() {{
            var counter = 0
            let sum: Int32 = {sum}
            assert_eq(40 - 10 - 5 + 17, 42)
            println(sum)
            println(choose(298))
        }}
    "#
    );
    work.write("chains.jk", &source);
    success(&work.cli(&["fmt", "chains.jk"]));
    work.assert_all_backends("chains.jk", &[], b"1000\n298\n", 0);
}

#[test]
#[cfg(unix)]
fn nested_generic_calls_and_method_chains_keep_context_on_all_backends() {
    let depth = 30;
    let nested = format!("{}42{}", "id(".repeat(depth), ")".repeat(depth));
    let ors = format!("Some(value){}", ".or(None)".repeat(depth));
    let ands = format!("Some(value){}", ".and(Some(value))".repeat(depth));
    let option_type = format!("{}Int32{}", "Option<".repeat(depth), ">".repeat(depth));
    let options = format!("{}42{}", "Some(".repeat(depth), ")".repeat(depth));
    let unwraps = ".unwrap()".repeat(depth);
    let source = format!(
        r#"
        fn id<T>(value: T) -> T {{ value }}
        fn nested() -> Int32 {{ {nested} }}
        fn ors(value: Int32) -> Option<Int32> {{ {ors} }}
        fn ands(value: Int32) -> Option<Int32> {{ {ands} }}
        fn first<T, U>(value: T, other: U) -> T {{ value }}
        fn options() -> {option_type} {{ {options} }}
        fn unwraps(value: {option_type}) -> Int32 {{ value{unwraps} }}
        fn main() {{
            let wide: Int64 = id(id(3000000000))
            let wrapped: Option<Int64> = Some(id(3000000000))
            let explicit = first::<Int64, Bool>(id(3000000000), true)
            assert_eq(wide, wrapped!)
            assert_eq(explicit, wide)
            let doubled: Int64 = 6000000000
            assert_eq(id(3000000000) + wide, doubled)
            let inferred = [1, first(wide, 0)]
            assert_eq(inferred[1], wide)
            assert_eq(unwraps(options()), 42)
            assert_eq(nested(), 42)
            assert_eq(ors(42)!, 42)
            println(ands(42)!)
        }}
    "#
    );
    let work = Workspace::new();
    work.write("nested.jk", &source);
    work.assert_all_backends("nested.jk", &[], b"42\n", 0);
}

#[test]
#[cfg(unix)]
fn module_constants_supply_generic_argument_types() {
    let work = Workspace::new();
    work.write(
        "constant.jk",
        r#"
        const A: Int64 = 3000000000
        fn id<T>(x: T) -> T { x }
        fn shadow(A: Bool) -> Bool { id(A) }
        fn main() {
            println(A)
            let inferred = Some(id(A))
            println(inferred!)
            assert_eq(shadow(true), true)
        }
    "#,
    );
    work.assert_all_backends(
        "constant.jk",
        &[],
        b"3000000000
3000000000
",
        0,
    );
}
