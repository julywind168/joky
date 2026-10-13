//! End-to-end checks of the installed-style command-line interface.
#[cfg(unix)]
#[path = "fixtures/map_stress_oracle.rs"]
mod map_stress_oracle;
#[cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;
#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/cli.rs"]
mod support;

#[cfg(unix)]
use runtime::runtime_archive;
use std::process::Command;
use support::{failure, success, Workspace};

// VM/AOT probe registries record integer events without doing I/O. CLI runs the
// same fixture algorithms with a synchronous Joky stdout adapter instead.
#[cfg(unix)]
fn recording_cli_source(source: &str) -> String {
    source.replace(
        "extern fn record_int(value: Int32)",
        "import joky/io.{println} fn record_int(value: Int32) { println(&value) }",
    )
}

#[test]
#[cfg(unix)]
fn sc0_probe_owned_snapshots_replacement_and_cancellation_run_on_all_backends() {
    let work = Workspace::new();
    work.write("probe.jk", include_str!("fixtures/state_domain_probe.jk"));
    work.assert_all_backends("probe.jk", &[], b"42\n77\n99\n", 0);
}

#[test]
#[cfg(unix)]
fn string_s5_example_and_module_aliases_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "collections.jk",
        include_str!("../examples/string_collections.jk"),
    );
    work.assert_all_backends("collections.jk", &[], b"5\n11\n6\n", 0);
    work.write(
        "parts/text.jk",
        r#"
        import joky/string as text.{type StringBuilder as Writer}
        import joky/array.{type Array}
        pub type Word = String
        pub type Texts = Array<Word>
        pub type Builder = Writer
        pub fn builder(input: &Word) -> Builder { Writer.from(input) }
        pub struct Envelope<T> { value: T }
        pub fn pass<T>(value: T) -> T { value }
        pub fn unpack<T>(value: Envelope<T>) -> T { value.value }
        pub fn pack(input: &Word) -> Envelope<Texts> {
            let separator = "|"
            let parts = input.split(&separator).unwrap_or(Array.new::<Word>())
            Envelope { value: pass(parts) }
        }
        pub fn render(parts: &Texts) -> Word {
            let separator = "|"
            text.join(parts, &separator)
        }
    "#,
    );
    work.write(
        "aliases.jk",
        r#"
        import parts/text as words.{type Texts as Parts, type Builder as Writer}
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn unpack(value: words.Envelope<Parts>) -> Parts { words.unpack(value) }
        fn main() {
            let source = "中|🙂"
            let pack = words.pack
            let wrapped = pack(&source)
            source.release()
            let unpack = unpack
            let parts = unpack(wrapped)
            let render = words.render
            let text = render(&parts)
            parts.release()
            var builder: Writer = words.builder(&text)
            text.release()
            builder.push('!')
            let rendered = builder.finish()
            report_int32(int32(rendered.byte_length()))
        }
    "#,
    );
    work.assert_all_backends("aliases.jk", &[], b"9\n", 0);
}

#[test]
#[cfg(unix)]
fn string_s4_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("queries.jk", include_str!("../examples/string_queries.jk"));
    work.assert_all_backends("queries.jk", &[], b"7\n4\n20013\n128578\n101\n769\n20\n", 0);
}

#[test]
#[cfg(unix)]
fn string_builder_s3_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("builder.jk", include_str!("../examples/string_builder.jk"));
    work.assert_all_backends("builder.jk", &[], b"15\n", 0);
}

#[test]
#[cfg(unix)]
fn string_storage_probe_executes_ownership_and_failures_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "probe.jk",
        &recording_cli_source(include_str!("fixtures/string_storage_probe.jk")),
    );
    work.write(
        "success.jk",
        "import probe fn main() { probe.report(probe.success()) probe.report(probe.transfer()) }",
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
                check.require(actual.equals(&expected), &message)
                let padded = "\u{{{scalar:06x}}}"
                let message = "Unicode length mismatch"
                check.require(padded.byte_length() == {}, &message)
            }}
            "#,
            ch.len_utf8(),
        ));
    }
    source.push_str(
        r#"
        let nul = "\u{0}Z" let expected = "\0Z"
        let message = "NUL must preserve following bytes"
        check.require(nul.equals(&expected), &message)
        let slash = "\u{5c}u{41}" let expected = "\\u{41}"
        let message = "Escape must decode once"
        check.require(slash.equals(&expected), &message)
        let newline = "\u{a}" let expected = "\n"
        let message = "Encoded newline is allowed"
        check.require(newline.equals(&expected), &message)
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
    source.push_str(include_str!("fixtures/uint8.jk"));
    source.push_str("\nfn expect(condition: Bool) { if !condition { let message = \"Byte arithmetic mismatch\" require(false, &message) } }\nfn report_int32(value: Int32) { println(&value) }\nfn add(a: UInt8, b: UInt8) -> UInt8 { a + b }\nfn sub(a: UInt8, b: UInt8) -> UInt8 { a - b }\nfn mul(a: UInt8, b: UInt8) -> UInt8 { a * b }\nfn div(a: UInt8, b: UInt8) -> UInt8 { a / b }\nfn shl(a: UInt8, b: UInt8) -> UInt8 { a << b }\nfn shr(a: UInt8, b: UInt8) -> UInt8 { a >> b }\nfn main() { { let assertion_condition = byte_check() == 42 let assertion_message = \"Byte fixture\" require(assertion_condition, &assertion_message) }\n");
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
    work.write("floats.jk", include_str!("fixtures/float.jk"));
    // `main` runs the semantic and aggregate composites and returns 42.
    work.assert_all_backends("floats.jk", &[], b"", 42);
    for (source, message) in [
        (
            "fn main() -> Int32 { let x: Float32 = 1.0e39 0 }",
            "finite range",
        ),
        (
            "fn main() -> Int32 { let x: Float64 = 1.0e309 0 }",
            "finite range",
        ),
        (
            "fn main() { let x = float32(1.0) + float64(2.0) }",
            "same type",
        ),
        (
            "fn main() { let x: Int32 = 1 let y: Float64 = 1.0 + x }",
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
    work.write("float_math.jk", include_str!("fixtures/float_math.jk"));
    // `main` runs math, checked-conversion, bit and formatting composites.
    work.assert_all_backends("float_math.jk", &[], b"", 42);
    for (source, message) in [
        (
            "fn main() -> Int32 { let x = 1.to_bits() 0 }",
            "has no method 'to_bits'",
        ),
        (
            "fn main() -> Int32 { let x = Float64.from_bits(1.5) 0 }",
            "Argument type mismatch",
        ),
        (
            "fn main() -> Int32 { let x = 1.5.trunc_to_i32() + 1 0 }",
            "same supported numeric type",
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
fn float_hosts_and_vm_float_calls_match_all_backends() {
    let work = Workspace::new();
    // Exact binary fractions keep Rust's shortest spelling and C's %.17g
    // identical across the JIT, interpreter and built artifacts.
    work.write(
        "floats.jk",
        "import joky/io.{println} fn report_float64(value: Float64) { println(&value) }\nfn main() { report_float64(2.5) report_float64(-0.75) }",
    );
    work.assert_all_backends("floats.jk", &[], b"2.5\n-0.75\n", 0);
    // vm.call_float64_1 reaches an interpreter VM from every host backend.
    work.write(
        "vmfloat.jk",
        r#"
        import joky/vm as host
        fn main() -> Int32 {
            let h = host.create_interpreter()
            { let vm_text = "fn twice(v: Float64) -> Float64 {{ v * 2.0 }}" host.load_source(h, &vm_text) }
            let result = { let vm_text = "twice" host.call_float64_1(h, &vm_text, 2.5) }
            host.close(h)
            if result != 5.0 { 1 } else { 0 }
        }
    "#,
    );
    work.assert_all_backends("vmfloat.jk", &[], b"", 0);
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn narrow_integers_match_all_backends_and_reject_bad_literals() {
    let work = Workspace::new();
    work.write(
        "narrow_integers.jk",
        include_str!("fixtures/narrow_integers.jk"),
    );
    // `main` runs the arithmetic, extension, bridge and array composites.
    work.assert_all_backends("narrow_integers.jk", &[], b"", 42);
    for (source, message) in [
        (
            "fn main() -> Int32 { let x: Int8 = 128 0 }",
            "outside Int8 range",
        ),
        (
            "fn main() -> Int32 { let x: Int8 = -129 0 }",
            "outside Int8 range",
        ),
        (
            "fn main() -> Int32 { let x: Int16 = -32769 0 }",
            "outside Int16 range",
        ),
        (
            "fn main() -> Int32 { let x: UInt16 = 65536 0 }",
            "outside UInt16 range",
        ),
        (
            "fn main() -> Int32 { let x: UInt16 = -1 0 }",
            "Unary `-` is not supported for UInt16",
        ),
        (
            "fn main() -> Int32 { let x: Int8 = 1 let y: Int16 = 2 if x < y { 0 } else { 1 } }",
            "the same type",
        ),
        (
            "fn main() -> Int32 { let x: UInt16 = 1 if -x == 0 { 0 } else { 1 } }",
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
        "fn main() -> Int32 { let a: Int8 = 1 let b: Int8 = 0 int32(a / b) }",
    );
    for args in [
        vec!["run", "traps.jk"],
        vec!["run", "--interpreter", "traps.jk"],
    ] {
        failure(&work.cli(&args), "Division by zero");
    }
    work.write(
        "shift.jk",
        "fn main() -> Int32 { let a: Int16 = 1 let n: Int16 = 16 int32(a << n) }",
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
            include_str!("fixtures/rune.jk")
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
#[cfg(unix)]
fn array_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("arrays.jk", include_str!("../examples/arrays.jk"));
    for args in [
        vec!["run", "arrays.jk"],
        vec!["run", "--interpreter", "arrays.jk"],
        vec!["run", "--no-prelude", "arrays.jk"],
        vec!["run", "--interpreter", "--no-prelude", "arrays.jk"],
    ] {
        let output = work.cli(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
    }
    let built = work.cli(&["build", "--no-prelude", "arrays.jk", "-o", "arrays"]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let output = Command::new(work.path().join("arrays")).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
}

#[test]
#[cfg(unix)]
fn user_file_modules_run_and_build_from_the_entry_directory() {
    let work = Workspace::new();
    work.write(
        "project/app/value.jk",
        r#"
        pub enum Choice { Empty, Value(Int32) }
        pub type Number = Int32
        fn offset(n: Number) -> Number { n + 1 }
        pub fn make(n: Number) -> Choice { Value(offset(n)) }
        pub fn copy<T>(n: T) -> T { n }
    "#,
    );
    work.write(
        "project/app/left.jk",
        r#"
        import app/value as values.{type Choice, make}
        pub fn value() -> Choice { make(20) }
    "#,
    );
    work.write(
        "project/app/right.jk",
        r#"
        import app/value as values.{type Choice, Value, copy}
        pub fn value() -> Choice { Value(copy(21)) }
    "#,
    );
    work.write(
        "project/main.jk",
        r#"
        import app/left as l
        import app/right as r
        import app/value.{type Choice, Value}
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn main() -> Int32 {
            let x = match l.value() { Value(n) => n, Choice.Empty => 0 }
            let y = match r.value() { Value(n) => n, Choice.Empty => 0 }
            report_int32(x + y)
            x + y
        }
    "#,
    );
    for args in [
        vec!["run", "project/main.jk"],
        vec!["run", "--interpreter", "project/main.jk"],
    ] {
        let output = work.cli(&args);
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
    }
    let output = work.cli(&["build", "project/main.jk", "-o", "program"]);
    success(&output);
    let output = Command::new(work.path().join("program")).output().unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"42\n");
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn user_file_module_constants_match_across_all_backends() {
    let work = Workspace::new();
    work.write(
        "project/app/config.jk",
        r#"
        pub const LIMIT: Int32 = 100
        const OFFSET: Int32 = 7
        pub const TOTAL: Int32 = LIMIT + OFFSET
    "#,
    );
    work.write(
        "project/app/derived.jk",
        r#"
        import app/config as cfg.{TOTAL}
        pub const SCALED: Int32 = TOTAL * 2
        pub fn scaled() -> Int32 { SCALED }
    "#,
    );
    work.write(
        "project/main.jk",
        r#"
        import app/config as cfg
        import app/derived as d
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn main() {
            report_int32(cfg.LIMIT + cfg.TOTAL + d.SCALED + d.scaled())
        }
    "#,
    );
    work.assert_all_backends("project/main.jk", &[], b"635\n", 0);
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
        for args in [
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
            vec!["build", "invalid.jk"],
        ] {
            failure(&work.cli(&args), message);
        }
    }
}

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
fn user_file_runtime_trace_and_output_protection_cover_dependencies() {
    let work = Workspace::new();
    let dependency = "pub fn divide(n: Int32) -> Int32 { 42 / n }";
    work.write("app/math.jk", dependency);
    work.write(
        "main.jk",
        "import app/math fn main() -> Int32 { math.divide(0) }",
    );
    let native_error = work.cli(&["run", "main.jk"]);
    failure(&native_error, "app/math.jk:1");
    failure(&native_error, "42 / n");
    let output = work.cli(&["run", "--interpreter", "main.jk"]);
    failure(&output, "app/math.jk:1");
    failure(&output, "at app/math.divide");
    failure(&output, "at main");
    failure(
        &work.cli(&["build", "main.jk", "-o", "app/math.jk"]),
        "must not replace the source file",
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("app/math.jk")).unwrap(),
        dependency
    );
}

#[test]
#[cfg(unix)]
fn user_files_share_host_imports_including_aot() {
    let work = Workspace::new();
    work.write(
        "app/engine.jk",
        r#"
        import joky/vm as host
        pub fn value() -> Int32 {
            let h = host.create_interpreter()
            { let vm_text = "fn answer() -> Int32 {{ 42 }}" host.load_source(h, &vm_text) }
            let result = { let vm_text = "answer" host.call_int32(h, &vm_text) }
            host.close(h)
            result
        }
    "#,
    );
    work.write(
        "main.jk",
        "import app/engine import joky/vm fn main() -> Int32 { engine.value() }",
    );
    for args in [
        vec!["run", "main.jk"],
        vec!["run", "--interpreter", "main.jk"],
    ] {
        let output = work.cli(&args);
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    success(&work.cli(&["build", "main.jk", "-o", "program"]));
    let output = Command::new(work.path().join("program")).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn unused_dependency_failure_still_links_its_runtime() {
    // A private module function is emitted even when no export reaches it, so
    // its failing body must still select the native runtime library.
    let work = Workspace::new();
    work.write(
        "app/math.jk",
        r#"
        fn divide(n: Int32) -> Int32 { 42 / n }
        pub fn double(n: Int32) -> Int32 { n * 2 }
        pub fn fails(n: Int32) -> Int32 { divide(n) }
    "#,
    );
    work.write(
        "main.jk",
        "import app/math fn main() -> Int32 { math.double(21) }",
    );
    for args in [
        vec!["run", "main.jk"],
        vec!["run", "--interpreter", "main.jk"],
    ] {
        let output = work.cli(&args);
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    success(&work.cli(&["build", "main.jk", "-o", "program"]));
    let output = Command::new(work.path().join("program")).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Reaching the failing body still names the dependency, not the entry file.
    work.write(
        "boom.jk",
        "import app/math.{fails} fn main() -> Int32 { fails(0) }",
    );
    failure(&work.cli(&["run", "boom.jk"]), "app/math.jk:2");
    failure(&work.cli(&["run", "boom.jk"]), "42 / n");
    success(&work.cli(&["build", "boom.jk", "-o", "boom"]));
    let output = Command::new(work.path().join("boom")).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Division by zero"));
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn alias_entry_points_and_host_signatures_work_in_all_execution_modes() {
    let work = Workspace::new();
    for (source, expected_output, expected_status) in [
        (include_str!("../examples/aliases.jk"), "43\n", 0),
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
        type Handle = vm.Handle
        fn report_int32(value: Count) { println(&value) }
        fn next(value: Count) -> Count { value + 1 }
        fn main() {
            let engine: Handle = vm.create()
            { let vm_text = "next" vm.register_int32(engine, &vm_text, next) }
            { let vm_text = "type Count = Int32 extern fn next(x: Count) -> Count fn value() -> Count {{ next(41) }}" vm.load_source(engine, &vm_text) }
            report_int32({ let vm_text = "value" vm.call_int32(engine, &vm_text) })
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
    work.write("wrapping.jk", "import joky/io.{println} fn report_int32(value: Int32) { println(&value) } fn divide(a: Int32, b: Int32) -> Int32 { a / b } fn main() { report_int32(divide(-2147483648, -1)) report_int32(divide(-7, 3)) report_int32(divide(7, -3)) }");
    work.assert_all_backends("wrapping.jk", &[], b"-2147483648\n-2\n-2\n", 0);
}

#[test]
#[cfg(unix)]
fn loop_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("loops.jk", include_str!("../examples/loops.jk"));
    work.assert_all_backends("loops.jk", &[], b"45\n", 0);
}

#[test]
#[cfg(unix)]
fn sequence_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("sequences.jk", include_str!("../examples/sequences.jk"));
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
        .map(|name| format!("{{ let value = {name}() println(&value) }}\n"))
        .collect::<String>();
    let output = b"42\n-42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n42\n";
    for effect in ["", " @suspends"] {
        let work = Workspace::new();
        let source = format!(
            "import joky/io.{{println}}\n{}\n{}\nfn main(){effect} {{\n{calls} newline_discard()\n}}",
            include_str!("fixtures/newlines.jk"),
            include_str!("fixtures/newline_dispatch.jk")
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
        include_str!("../examples/conditionals.jk"),
    );
    work.assert_all_backends("conditionals.jk", &[], b"-1\n0\n1\n0\n4\n", 0);
}

#[test]
#[cfg(unix)]
fn jump_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("jumps.jk", include_str!("../examples/jumps.jk"));
    work.assert_all_backends("jumps.jk", &[], b"0\n4\n13\n", 0);
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
fn help_version_and_argument_errors_are_user_facing() {
    let work = Workspace::new();
    for args in [
        vec![],
        vec!["--help"],
        vec!["run", "--help"],
        vec!["build", "-h"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("joky run"));
    }
    let output = work.cli(&["--version"]);
    success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("joky {}\n", env!("CARGO_PKG_VERSION"))
    );
    for (args, message) in [
        (vec!["unknown"], "Unknown command"),
        (vec!["run"], "Missing source file"),
        (vec!["build", "hello.jk", "-o"], "Missing path"),
        (vec!["run", "hello.jk", "-o", "hello"], "only available"),
        (vec!["build", "hello.jk", "--interpreter"], "only available"),
        (
            vec!["run", "hello.jk", "--interpreter", "--interpreter"],
            "more than once",
        ),
        (vec!["run", "a.jk", "b.jk"], "exactly one source"),
        (vec!["run", "--unknown"], "Unknown option"),
        (vec!["run", "hello.txt"], ".jk extension"),
        (vec!["run", "missing.jk"], "Cannot read 'missing.jk'"),
        (
            vec!["build", "hello.jk", "-o", "a", "-o", "b"],
            "more than once",
        ),
    ] {
        failure(&work.cli(&args), message);
    }
}

#[test]
fn run_can_select_the_interpreter_without_a_c_compiler() {
    let work = Workspace::new();
    work.write("hello.jk", include_str!("../examples/hello.jk"));
    let output = work
        .command()
        .args(["run", "--interpreter", "hello.jk"])
        .env("PATH", "")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn run_needs_no_c_compiler_and_accepts_paths_after_double_dash() {
    let work = Workspace::new();
    work.write("-hello world.jk", include_str!("../examples/hello.jk"));
    let output = work
        .command()
        .args(["run", "--", "-hello world.jk"])
        .env("PATH", "")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn run_and_build_report_source_and_entry_errors_consistently() {
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
        for command in ["run", "build"] {
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
    work.write("bad.jk", "fn main() {\n?\n}\n");
    let output = work.cli(&["run", "bad.jk"]);
    failure(&output, "Unexpected character");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("bad.jk:2:1"), "{error}");
    assert!(error.contains("2 │ ?"), "{error}");
    assert!(error.contains("Unexpected character '?'"), "{error}");
}

#[test]
fn parser_diagnostics_use_utf8_byte_spans() {
    let work = Workspace::new();
    work.write("unicode.jk", "fn main() { let x = \"🙂\" ? }\n");
    let output = work.cli(&["run", "unicode.jk"]);
    failure(&output, "Unexpected character");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("unicode.jk:1:25"), "{error}");
    assert!(
        error.contains("1 │ fn main() { let x = \"🙂\" ? }"),
        "{error}"
    );
}

#[test]
fn parser_diagnostics_at_eof_render_without_panicking() {
    let work = Workspace::new();
    work.write("eof.jk", "fn main() {\n");
    let output = work.cli(&["run", "eof.jk"]);
    failure(&output, "Expected RBrace");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("eof.jk:1:13"), "{error}");
    assert!(error.contains("Line 2: Expected RBrace"), "{error}");
}

#[test]
fn renamed_scope_constructs_report_parallel_migration() {
    let work = Workspace::new();
    work.write(
        "old_scope.jk",
        "fn wait() @suspends {}\nfn main() @suspends { scope { | wait() } }\n",
    );
    let output = work.cli(&["run", "old_scope.jk"]);
    failure(&output, "Use parallel for pipe-separated branches");

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
fn task_scopes_and_branches_match_all_backends() {
    let work = Workspace::new();
    work.write(
        "task_scope.jk",
        r#"import joky/task
import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
fn worker(id: Int32) @suspends {
    task.yield_now()
    report_int32(100 + id)
}
fn early() @suspends {
    scope {
        branch { worker(1) }
        task.yield_now()
        return
    }
}
fn main() @suspends {
    early()
    scope {
        branch { worker(1) }
        branch { worker(2) }
        task.yield_now()
        report_int32(9)
    }
    report_int32(42)
}
"#,
    );
    // Registration returns immediately; the body's yield gives the children
    // their round; the join completes them in registration order; the early
    // return joins its scope before leaving.
    work.assert_all_backends("task_scope.jk", &[], b"101\n9\n101\n102\n42\n", 0);
}

#[test]
fn task_scope_rounds_follow_calls_and_static_branches() {
    let work = Workspace::new();
    work.write(
        "rounds.jk",
        r#"import joky/task.{yield_now}
import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
fn helper(id: Int32) @suspends {
    scope {
        branch { report_int32(id) yield_now() report_int32(id + 1) }
        yield_now()
        report_int32(id + 2)
        yield_now()
        report_int32(id + 3)
    }
}
fn relay(id: Int32) @suspends { helper(id) }
fn main() @suspends {
    relay(10)
    parallel { | relay(20) | relay(30) }
}
"#,
    );
    work.assert_all_backends(
        "rounds.jk",
        &[],
        b"10\n12\n11\n13\n20\n30\n22\n32\n21\n31\n23\n33\n",
        0,
    );
}

#[test]
fn task_scope_implicit_branch_scopes_join_on_return() {
    let work = Workspace::new();
    work.write(
        "nested.jk",
        r#"import joky/task.{yield_now}
import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
fn main() @suspends {
    scope {
        branch {
            branch { branch { report_int32(3) } yield_now() report_int32(2) }
            report_int32(1)
            return
        }
    }
    report_int32(4)
}
"#,
    );
    work.assert_all_backends("nested.jk", &[], b"1\n3\n2\n4\n", 0);
}

#[test]
fn task_scope_captures_callees_and_method_receivers() {
    let work = Workspace::new();
    work.write(
        "captures.jk",
        r#"import joky/task.{yield_now}
import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
fn say() @suspends { yield_now() report_int32(1) }
fn other() @suspends { report_int32(2) }
fn main() @suspends {
    let f = say
    let text = "hello"
    scope {
        branch { f() }
        branch { report_int32(int32(text.byte_length())) }
        branch { let f = other f() }
    }
}
"#,
    );
    work.assert_all_backends("captures.jk", &[], b"5\n2\n1\n", 0);
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
        "import joky/vm fn main() { vm.call_int32() }\n",
    );
    let output = work.cli(&["run", "resolve.jk"]);
    failure(&output, "vm.call_int32 expects");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("resolve.jk:1:28"), "{error}");
    assert!(
        error.contains("1 │ import joky/vm fn main() { vm.call_int32() }"),
        "{error}"
    );
}

#[test]
#[cfg(unix)]
fn built_program_matches_jit_output_and_avoids_native_symbol_collisions() {
    let work = Workspace::new();
    work.write(
        "hello world.jk",
        r#"
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn printf(x: Int32) -> Int32 { x + 1 }
        fn pick(unit: Unit, value: Int32) -> Int32 { unit value }
        fn main() { report_int32(pick({}, printf(41))) }
    "#,
    );
    let run = work.cli(&["run", "hello world.jk"]);
    success(&run);
    assert_eq!(run.stdout, b"42\n");
    let build = work.cli(&["build", "hello world.jk"]);
    success(&build);
    assert!(build.stdout.is_empty());
    assert!(String::from_utf8_lossy(&build.stderr).contains("Built hello world"));
    // The artifact remains runnable without either its source or the CLI.
    std::fs::remove_file(work.path().join("hello world.jk")).unwrap();
    let native = Command::new(work.path().join("hello world"))
        .env("PATH", "")
        .output()
        .unwrap();
    success(&native);
    assert_eq!(native.stdout, run.stdout);
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn explicit_output_and_main_exit_codes_match_run() {
    let work = Workspace::new();
    for (source, code) in [
        ("fn main() {}", 0),
        ("fn main() -> Int32 { 7 }", 7),
        ("fn main() -> Int32 { 263 }", 7),
        ("fn main() -> Int32 { 0 - 1 }", 255),
        (
            "fn report_int32(x: Int32) -> Int32 { x } fn main() -> Int32 { report_int32(8) }",
            8,
        ),
    ] {
        work.write("exit.jk", source);
        let run = work.cli(&["run", "exit.jk"]);
        assert_eq!(run.status.code(), Some(code));
        assert!(run.stdout.is_empty() && run.stderr.is_empty());
        success(&work.cli(&["build", "--output", "custom program", "exit.jk"]));
        let native = Command::new(work.path().join("custom program"))
            .output()
            .unwrap();
        assert_eq!(native.status.code(), Some(code));
        assert!(native.stdout.is_empty() && native.stderr.is_empty());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn failed_build_preserves_source_and_existing_output() {
    use std::os::unix::{fs::symlink, fs::PermissionsExt};
    let work = Workspace::new();
    let source = "fn main() {}";
    work.write("safe.jk", source);
    work.write("existing", "keep this output");
    failure(
        &work.cli(&["build", "safe.jk", "-o", "./safe.jk"]),
        "must not replace the source",
    );
    symlink(work.path().join("safe.jk"), work.path().join("alias")).unwrap();
    failure(
        &work.cli(&["build", "safe.jk", "-o", "alias"]),
        "must not replace the source",
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("safe.jk")).unwrap(),
        source
    );
    let output = work
        .command()
        .args(["build", "safe.jk", "-o", "existing"])
        .env("PATH", "")
        .output()
        .unwrap();
    failure(&output, "Cannot start C compiler 'cc'");
    work.assert_no_build_files();
    work.write("cc", "#!/bin/sh\necho 'test linker failure' >&2\nexit 1\n");
    std::fs::set_permissions(
        work.path().join("cc"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let output = work
        .command()
        .args(["build", "safe.jk", "-o", "existing"])
        .env("PATH", work.path())
        .output()
        .unwrap();
    failure(&output, "test linker failure");
    work.write("safe.jk", "fn main() { unknown() }");
    failure(
        &work.cli(&["build", "safe.jk", "-o", "existing"]),
        "Undeclared function",
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn vm_standard_library_runs_in_jit_and_standalone_executable() {
    let work = Workspace::new();
    work.write("vm.jk", include_str!("../examples/vm.jk"));
    let jit = work.cli(&["run", "vm.jk"]);
    success(&jit);
    assert_eq!(jit.stdout, b"42\n100\n");
    let built = work
        .command()
        .args(["build", "vm.jk"])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&built);
    std::fs::remove_file(work.path().join("vm.jk")).unwrap();
    let native = Command::new(work.path().join("vm"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&native);
    assert_eq!(native.stdout, jit.stdout);
    assert!(native.stderr.is_empty());
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn standard_library_reports_failures_and_keeps_the_previous_module() {
    let work = Workspace::new();
    work.write("errors.jk", r#"
        import joky/vm
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn source() -> String { "fn value() -> Int32 {{ 42 }}" }
        fn identity(text: String) -> String { text }
        fn main() -> Int32 {
            let engine = vm.create()
            report_int32({ let vm_text = identity(source()) vm.load_source(engine, &vm_text) })
            report_int32({ let vm_text = "value" vm.call_int32(engine, &vm_text) })
            report_int32({ let vm_text = "fn value() -> Bool {{ 42 }}" vm.reload_source(engine, &vm_text) })
            report_int32(vm.error_code())
            vm.print_error()
            report_int32({ let vm_text = "value" vm.call_int32(engine, &vm_text) })
            report_int32({ let vm_text = "missing" vm.call_int32(engine, &vm_text) })
            report_int32(vm.error_code())
            vm.print_error()
            report_int32({ let vm_text = "import joky/vm fn nested() -> Int32 {{ let child = vm.create() vm.close(child) }}" vm.load_source(engine, &vm_text) })
            report_int32({ let vm_text = "nested" vm.call_int32(engine, &vm_text) })
            report_int32(vm.close(engine))
            report_int32(vm.close(engine))
            vm.error_code()
        }
    "#);
    let jit = work.cli(&["run", "errors.jk"]);
    assert_eq!(jit.status.code(), Some(1));
    assert_eq!(jit.stdout, b"0\n42\n1\n1\n42\n0\n1\n0\n0\n0\n1\n");
    let stderr = String::from_utf8_lossy(&jit.stderr);
    assert!(
        stderr.contains("Return type mismatch") && stderr.contains("not found"),
        "{stderr}"
    );
    success(
        &work
            .command()
            .args(["build", "errors.jk"])
            .env("JOKY_RUNTIME", runtime_archive())
            .output()
            .unwrap(),
    );
    let native = Command::new(work.path().join("errors")).output().unwrap();
    assert_eq!(native.status.code(), jit.status.code());
    assert_eq!(native.stdout, jit.stdout);
    assert_eq!(native.stderr, jit.stderr);
}

#[test]
fn imports_and_standard_library_calls_are_checked_before_execution() {
    let work = Workspace::new();
    for (source, message) in [
        (
            "import joky/missing fn main() {}",
            "Unknown built-in module",
        ),
        (
            "import joky/vm import joky/vm fn main() {}",
            "Duplicate import",
        ),
        ("fn main() { let h = vm.create() }", "Undeclared function"),
        (
            "import joky/vm fn main() { vm.unknown() }",
            "Undeclared function",
        ),
        (
            "import joky/vm fn main() { let h = vm.create() vm.load_source(h, 42) }",
            "Argument type mismatch",
        ),
        (
            "import joky/vm fn main() { vm.close(1) }",
            "Argument type mismatch",
        ),
        (
            "import joky/vm fn main() { vm.call_int32() }",
            "zero to two Int32 arguments",
        ),
        (
            "import joky/vm fn main() { let h = vm.create() { let vm_text = \"missing\" vm.register_int32(h, &vm_text, missing) } }",
            "not defined in this module",
        ),
        (
            "import joky/vm fn host(value: String) -> Int32 { 1 } fn main() { let h = vm.create() { let vm_text = \"host\" vm.register_int32(h, &vm_text, host) } }",
            "must be a synchronous Int32 function",
        ),
        (
            "fn joky_vm_create() {} fn main() {}",
            "Reserved runtime function",
        ),
    ] {
        work.write("bad.jk", source);
        for command in ["run", "build"] {
            failure(&work.cli(&[command, "bad.jk"]), message);
        }
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn missing_standard_runtime_has_an_actionable_error_and_preserves_output() {
    let work = Workspace::new();
    work.write("vm.jk", include_str!("../examples/vm.jk"));
    work.write("existing", "keep this output");
    let output = work
        .command()
        .args(["build", "vm.jk", "-o", "existing"])
        .env("JOKY_RUNTIME", work.path().join("missing.a"))
        .output()
        .unwrap();
    failure(&output, "JOKY_RUNTIME does not name a runtime archive");
    assert_eq!(
        std::fs::read_to_string(work.path().join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn installed_compiler_finds_its_runtime_without_repository_paths() {
    let work = Workspace::new();
    let bin = work.path().join("installation/bin");
    let lib = work.path().join("installation/lib/joky");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_joky"), bin.join("joky")).unwrap();
    std::fs::copy(runtime_archive(), lib.join("libjoky.a")).unwrap();
    work.write("vm.jk", include_str!("../examples/vm.jk"));
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "vm.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    work.write(
        "division.jk",
        "fn divide() -> Int32 { 7 / 0 } fn main() -> Int32 { divide() }",
    );
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "division.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    work.write(
        "shift.jk",
        "fn shift(n: UInt64) -> UInt64 { 1 << n } fn main() { shift(64); }",
    );
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "shift.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    work.write("async_main.jk", include_str!("../examples/async_main.jk"));
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "async_main.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    std::fs::remove_dir_all(work.path().join("installation")).unwrap();
    std::fs::remove_file(work.path().join("async_main.jk")).unwrap();
    let program = Command::new(work.path().join("async_main"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&program);
    assert_eq!(program.stdout, b"42\n");
    std::fs::remove_file(work.path().join("vm.jk")).unwrap();
    let program = Command::new(work.path().join("vm"))
        .env("PATH", "")
        .output()
        .unwrap();
    success(&program);
    assert_eq!(program.stdout, b"42\n100\n");
    std::fs::remove_file(work.path().join("shift.jk")).unwrap();
    let program = Command::new(work.path().join("shift"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    failure(&program, "Shift count");
    assert!(program.stdout.is_empty());
    std::fs::remove_file(work.path().join("division.jk")).unwrap();
    let program = Command::new(work.path().join("division"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    failure(&program, "Division by zero");
    assert_eq!(program.status.code(), Some(1));
}

#[test]
#[cfg(unix)]
fn records_run_and_build_with_nominal_type_diagnostics() {
    let work = Workspace::new();
    work.write("records.jk", include_str!("../examples/records.jk"));
    work.assert_all_backends("records.jk", &[], b"1\n42\n", 0);
    for (source, message) in [
        ("struct P { next: P } fn main() {}", "recursive_record"),
        (
            "struct P { x: Int32 } fn main() { let p = P {} }",
            "Missing field `x`",
        ),
        (
            "struct P { x: Int32 } fn main() { let p = P { x: 1 } p.x = 2 }",
            "invalid_assignment",
        ),
        ("struct P {} fn main() -> P { P {} }", "Entry point must be"),
        (
            "struct P {} extern fn report_int32(value: P) fn main() {}",
            "host ABI",
        ),
    ] {
        work.write("invalid_record.jk", &format!("// 🙂\n{source}"));
        for args in [
            vec!["run", "invalid_record.jk"],
            vec!["run", "--interpreter", "invalid_record.jk"],
            vec!["build", "invalid_record.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_record.jk:2:"));
        }
        assert!(!work.path().join("invalid_record").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn enums_run_and_build_with_nominal_type_diagnostics() {
    let work = Workspace::new();
    work.write("enums.jk", include_str!("../examples/enums.jk"));
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
        for args in [
            vec!["run", "invalid_enum.jk"],
            vec!["run", "--interpreter", "invalid_enum.jk"],
            vec!["build", "invalid_enum.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_enum.jk:2:"));
        }
        assert!(!work.path().join("invalid_enum").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn tuples_run_and_build_with_pattern_diagnostics() {
    let work = Workspace::new();
    work.write("tuples.jk", include_str!("../examples/tuples.jk"));
    work.assert_all_backends("tuples.jk", &[], b"42\n13\n7\n", 0);
    for (source, message) in [
        ("type T = (T,) fn main() {}", "cyclic_type_alias"),
        ("fn main() { let (x, y) = (1,) }", "type_mismatch"),
        ("fn main() { let (x, x) = (1, 2) }", "duplicate_definition"),
        (
            "fn main() { let (x, y) = (1, 2) x = 3 }",
            "invalid_assignment",
        ),
        ("fn main() -> (Int32,) { (1,) }", "Entry point must be"),
        (
            "extern fn report_int32(value: (Int32,)) fn main() {}",
            "host ABI",
        ),
        ("fn main() { let (x y) = (1, 2) }", "Expected RParen"),
    ] {
        work.write("invalid_tuple.jk", &format!("// 🙂\n{source}"));
        for args in [
            vec!["run", "invalid_tuple.jk"],
            vec!["run", "--interpreter", "invalid_tuple.jk"],
            vec!["build", "invalid_tuple.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_tuple.jk:2:"));
        }
        assert!(!work.path().join("invalid_tuple").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn int64_literals_output_and_failures_match_all_frontends() {
    let work = Workspace::new();
    work.write("int64.jk", include_str!("../examples/int64.jk"));
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
            "fn main() { let n = 1 let wide: Int64 = n }",
            "annotation mismatch",
        ),
        (
            "extern fn report_int64(n: Int32) fn main() {}",
            "Unknown CLI host function",
        ),
        ("fn main() -> Int64 { 1 }", "Entry point must be"),
    ] {
        work.write("invalid64.jk", &format!("// 🙂\n{source}"));
        for args in [
            vec!["run", "invalid64.jk"],
            vec!["run", "--interpreter", "invalid64.jk"],
            vec!["build", "invalid64.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid64.jk:2:"));
        }
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
    work.write("unsigned.jk", include_str!("../examples/unsigned.jk"));
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
            "fn main() { let n = 1 let u: UInt32 = n }",
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
        for args in [
            vec!["run", "invalid_unsigned.jk"],
            vec!["run", "--interpreter", "invalid_unsigned.jk"],
            vec!["build", "invalid_unsigned.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_unsigned.jk:2:"));
        }
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
#[cfg(unix)]
fn bitwise_radix_output_diagnostics_and_shift_failures_match_frontends() {
    let work = Workspace::new();
    work.write("bitwise.jk", include_str!("../examples/bitwise.jk"));
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
        for args in [
            vec!["run", "invalid_bits.jk"],
            vec!["run", "--interpreter", "invalid_bits.jk"],
            vec!["build", "invalid_bits.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_bits.jk:2:"));
        }
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
    work.write("conversions.jk", include_str!("../examples/conversions.jk"));
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
        for args in [
            vec!["run", "invalid_conversion.jk"],
            vec!["run", "--interpreter", "invalid_conversion.jk"],
            vec!["build", "invalid_conversion.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_conversion.jk:2:"));
        }
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
fn generic_functions_run_build_and_reject_invalid_templates_in_all_frontends() {
    let work = Workspace::new();
    work.write("generics.jk", include_str!("../examples/generics.jk"));
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
            "Return type mismatch",
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
            "fn id<T>(n: T) -> T { n } fn main() { let n: Int64 = id(1) }",
            "Type annotation mismatch",
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
        for args in [
            vec!["run", "invalid_generic.jk"],
            vec!["run", "--interpreter", "invalid_generic.jk"],
            vec!["build", "invalid_generic.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_generic.jk:2:"));
        }
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
        include_str!("../examples/generic_enums.jk"),
    );
    let expected = b"42\n7\n18446744073709551615\n42\n-1\n";
    work.assert_all_backends("generic_enums.jk", &[], expected, 0);
    let source = include_str!("fixtures/generic_enums.jk");
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
            "enum Bad<T> { Value(vm.Handle) } fn main() {}",
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
        for args in [
            vec!["run", "invalid_enum.jk"],
            vec!["run", "--interpreter", "invalid_enum.jk"],
            vec!["build", "invalid_enum.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_enum.jk:2:"));
        }
        assert!(!work.path().join("invalid_enum").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_aliases_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_aliases.jk",
        include_str!("../examples/generic_aliases.jk"),
    );
    work.write(
        "model.jk",
        "pub type PairOf<T> = (T, T) pub fn duplicate<T>(n: T) -> PairOf<T> { (n, n) }",
    );
    work.write(
        "imported_alias.jk",
        r#"
        import model as m.{type PairOf as Duo}
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        type PairOf<T> = m.PairOf<T>
        fn first<T>(p: PairOf<T>) -> T { let (a, b) = p a }
        fn show() { let p: Duo<Int32> = m.duplicate(42) report_int32(first(p)) }
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
            "type Drop<T> = Int32 type Bad = Drop<vm.Handle> fn main() {}",
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
        for args in [
            vec!["run", "invalid_alias.jk"],
            vec!["run", "--interpreter", "invalid_alias.jk"],
            vec!["build", "invalid_alias.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_alias.jk:2:"));
        }
        assert!(!work.path().join("invalid_alias").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn option_result_methods_run_and_build_with_explicit_or_default_imports() {
    let work = Workspace::new();
    work.write(
        "option_result.jk",
        include_str!("../examples/option_result.jk"),
    );
    work.write("std_values.jk", &format!("import joky/io.{{println}}\n{}\n fn report_int32(value: Int32) {{ println(&value) }} fn report_uint64(value: UInt64) {{ println(&value) }} fn main() {{ report_int32(check()) report_uint64(wide()) }}", include_str!("fixtures/option_result_methods.jk")));
    work.write("result_only.jk", "import joky/result as r import joky/io.{println} fn report_int32(value: Int32) { println(&value) } fn main() { report_int32(r.Result.Ok::<Int32, Bool>(42).ok().unwrap_or(0)) }");
    for (file, program, expected, no_prelude) in [
        (
            "option_result.jk",
            "option_result",
            "42\n7\n4294967295\n42\n9\n7\n1\n",
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
        for args in [
            vec!["run", "invalid_std.jk"],
            vec!["run", "--interpreter", "invalid_std.jk"],
            vec!["build", "invalid_std.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_std.jk:2:"));
        }
        assert!(!work.path().join("invalid_std").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_methods_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_methods.jk",
        include_str!("../examples/generic_methods.jk"),
    );
    for (file, fixture) in [
        (
            "shared_methods.jk",
            include_str!("fixtures/generic_methods.jk"),
        ),
        (
            "library_methods.jk",
            include_str!("fixtures/generic_library_methods.jk"),
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
        ("fn main() { Some(1).zip::<String>(Some(\"text\")) }", "Return type mismatch"),
        ("fn main() { Result.Ok::<Int32, Bool>(1).and(Result.Ok(\"text\")) }", "Return type mismatch"),
    ] {
        work.write("invalid_method.jk", &format!("// 🙂\n{source}"));
        for args in [vec!["run", "invalid_method.jk"], vec!["run", "--interpreter", "invalid_method.jk"], vec!["build", "invalid_method.jk"]] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_method.jk:2:"));
        }
        assert!(!work.path().join("invalid_method").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_impls_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_impls.jk",
        include_str!("../examples/generic_impls.jk"),
    );
    work.write("model.jk", "pub struct Pair<T> { left: T, right: T } impl<T> Pair<T> { pub fn make(left: T, right: T) -> Self { Self { left: left, right: right } } pub fn first(self) -> T { self.left } }");
    work.write("imported_impl.jk", "import model as m.{type Pair as Renamed} import joky/io.{println} fn report_int64(value: Int64) { println(&value) } fn main() { report_int64(m.Pair::<Int64>.make(4294967295, 0).first()) report_int64(Renamed.make::<Int64>(42, 0).first()) }");
    work.write(
        "shared_impl.jk",
        &format!(
            "import joky/io.{{println}}\n{}\n fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(check()) }}",
            include_str!("fixtures/generic_impls.jk")
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
        ("struct Marker<T> {} impl<T> Marker<T> { fn make() -> Self { Self {} } } fn main() { Marker.make() }", "Cannot infer type parameter"),
        ("struct Marker<T> {} impl<T> Marker<T> { fn make() -> Self { Self {} } } fn main() { Marker.make::<String>() }", "Return type mismatch"),
    ] {
        work.write("invalid_impl.jk", &format!("// 🙂\n{source}"));
        for args in [vec!["run", "invalid_impl.jk"], vec!["run", "--interpreter", "invalid_impl.jk"], vec!["build", "invalid_impl.jk"]] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_impl.jk:2:"));
        }
        assert!(!work.path().join("invalid_impl").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn generic_records_run_build_and_report_template_errors() {
    let work = Workspace::new();
    work.write(
        "generic_records.jk",
        include_str!("../examples/generic_records.jk"),
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
            "struct Bad<T> { value: vm.Handle } fn main() {}",
            "Copy value",
        ),
    ] {
        work.write("invalid_record.jk", &format!("// 🙂\n{source}"));
        for args in [
            vec!["run", "invalid_record.jk"],
            vec!["run", "--interpreter", "invalid_record.jk"],
            vec!["build", "invalid_record.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_record.jk:2:"));
        }
        assert!(!work.path().join("invalid_record").exists());
    }
}

#[test]
fn integer_methods_run_build_and_report_invalid_members() {
    let work = Workspace::new();
    for (file, source, expected) in [
        (
            "integer_methods.jk",
            include_str!("../examples/integer_methods.jk").to_owned(),
            "42\n1\n18446744073709551615\n2147483647\n-1\n",
        ),
        (
            "methods.jk",
            format!(
                "import joky/io.{{println}}\n{}\n fn report_int32(value: Int32) {{ println(&value) }}\nfn main() {{ report_int32(check()) }}",
                include_str!("fixtures/methods.jk")
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
        for args in [
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
            vec!["build", "invalid.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("invalid.jk:2:"));
        }
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn prelude_module_aliases_and_opt_out_work_in_run_and_build() {
    let work = Workspace::new();
    for (file, source, no_prelude) in [
        ("prelude", "fn main() -> Int32 { match (42).to_u64() { Some(n) => int32(n), None => 0 } }".to_owned(), false),
        ("imports", format!("{}\nfn main() -> Int32 {{ check() }}", include_str!("fixtures/imports.jk")), true),
        ("plain", "enum Option { Present(Int32) } fn main() -> Int32 { match Option.Present(42) { Present(n) => n } }".into(), true),
        ("explicit", "import joky/prelude fn main() -> Int32 { match Some(42) { Some(n) => n, None => 0 } }".into(), true),
    ] {
        let source_file = format!("{file}.jk");
        work.write(&source_file, &source);
        for interpreter in [false, true] {
            let mut args = vec!["run"];
            if interpreter { args.push("--interpreter"); }
            if no_prelude { args.push("--no-prelude"); }
            args.push(&source_file);
            let output = work.cli(&args);
            assert_eq!(output.status.code(), Some(42), "{}", String::from_utf8_lossy(&output.stderr));
        }
        let mut args = vec!["build"];
        if no_prelude { args.push("--no-prelude"); }
        args.push(&source_file);
        success(&work.cli(&args));
        let output = Command::new(work.path().join(file)).output().unwrap();
        assert_eq!(output.status.code(), Some(42), "{output:?}");
    }
    for command in ["run", "build"] {
        failure(
            &work.cli(&[command, "--no-prelude", "prelude.jk"]),
            "has no method",
        );
        failure(
            &work.cli(&[command, "--no-prelude", "--no-prelude", "plain.jk"]),
            "more than once",
        );
        failure(&work.cli(&[command, "plain.jk"]), "already defined");
    }
    // Native registration remains supported by JIT/AOT; ordinary aliased VM calls
    // also run in the interpreter.
    for register in [false, true] {
        let initialization = if register {
            "let name = \"next\" register(h, &name, next) let code = \"extern fn next(n: Int32) -> Int32 fn answer() -> Int32 {{ next(41) }}\" v.load_source(h, &code)"
        } else {
            "let code = \"fn answer() -> Int32 {{ 42 }}\" v.load_source(h, &code)"
        };
        let source = format!("import joky/vm as v.{{type Handle as Engine, create as new, register_int32 as register, call_int32 as invoke, close}} fn next(n: Int32) -> Int32 {{ n + 1 }} fn main() -> Int32 {{ let vm = 7 let h: Engine = new() {initialization} let entry = \"answer\" let answer = invoke(h, &entry) close(h) answer }}");
        work.write("host_alias.jk", &source);
        let output = work.cli(&["run", "host_alias.jk"]);
        assert_eq!(output.status.code(), Some(42), "{output:?}");
        if !register {
            let output = work.cli(&["run", "--interpreter", "host_alias.jk"]);
            assert_eq!(output.status.code(), Some(42), "{output:?}");
        }
        success(&work.cli(&["build", "host_alias.jk"]));
        let output = Command::new(work.path().join("host_alias"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(42), "{output:?}");
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn borrowed_receiver_example_matches_all_frontends_and_reports_conflicts() {
    let work = Workspace::new();
    work.write(
        "methods.jk",
        include_str!("../examples/borrowed_methods.jk"),
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
    work.write("bad.jk", "struct Counter { value: Int32 } impl Counter { fn set(&mut self) { self.value = 1 } } fn main() { let c = Counter { value: 0 } c.set() }");
    for args in [
        vec!["run", "bad.jk"],
        vec!["run", "--interpreter", "bad.jk"],
        vec!["build", "bad.jk", "-o", "bad"],
    ] {
        let output = work.cli(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("var binding"));
    }
    assert!(!work.path().join("bad").exists());
}

#[test]
#[cfg(unix)]
fn static_trait_example_matches_all_frontends_and_reports_bound_errors() {
    let work = Workspace::new();
    work.write("traits.jk", include_str!("../examples/static_traits.jk"));
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
    for args in [
        vec!["run", "bad.jk"],
        vec!["run", "--interpreter", "bad.jk"],
        vec!["build", "bad.jk", "-o", "bad"],
    ] {
        let output = work.cli(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("does not satisfy trait bound"));
    }
    assert!(!work.path().join("bad").exists());
}

#[test]
#[cfg(unix)]
fn function_value_example_matches_all_frontends_and_rejects_signature_mismatches() {
    let work = Workspace::new();
    work.write(
        "functions.jk",
        include_str!("../examples/function_values.jk"),
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
        "fn id(x: Int32) -> Int32 { x } fn main() { let f = id f(true); }",
    );
    for args in [
        vec!["run", "bad.jk"],
        vec!["run", "--interpreter", "bad.jk"],
        vec!["build", "bad.jk", "-o", "bad"],
    ] {
        let output = work.cli(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Argument type mismatch"));
    }
    assert!(!work.path().join("bad").exists());
}

#[test]
#[cfg(unix)]
fn suspending_cli_entries_match_jit_interpreter_and_standalone() {
    let work = Workspace::new();
    for (source, code, stdout) in [
        ("fn main() @suspends {}", 0, ""),
        ("fn unused() @suspends {} impl Int32 { fn f(self) @suspends {} } fn main() {}", 0, ""),
        ("fn callback() @suspends -> Int32 { 42 } fn apply(f: fn() @suspends -> Int32) @suspends -> Int32 { f() } fn main() @suspends -> Int32 { apply(callback) }", 42, ""),
        (include_str!("../examples/async_main.jk"), 0, "42\n"),
        (r#"
            import joky/task.{yield_now, sleep_ms}
            fn main() @suspends -> Int32 {
                let first = 100
                let second = 150
                yield_now()
                sleep_ms(1)
                first + second + 49
            }
        "#, 43, ""),
        (r#"
            import joky/task as task
            import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
            impl Int32 {
                fn worker(self) @suspends {
                    report_int32(self)
                    task.yield_now()
                    task.yield_now()
                    report_int32(self + 10)
                }
            }
            fn worker(n: Int32) @suspends { n.worker() }
            fn winner() @suspends { task.yield_now(); report_int32(99) }
            fn loser() @suspends { task.sleep_ms(1); report_int32(-1) }
            fn main() @suspends {
                let f: fn(Int32) @suspends -> Unit = worker
                parallel { | f(1) | f(2) }
                race { | winner() | loser() }
                task.sleep_ms(1)
                report_int32(42)
            }
        "#, 0, "1\n2\n11\n12\n99\n42\n"),
    ] {
        work.write("task.jk", source);
        for flags in [vec![], vec!["--no-prelude"]] {
            for interpreter in [false, true] {
                let mut args = vec!["run"];
                if interpreter { args.push("--interpreter"); }
                args.extend(flags.iter().copied());
                args.push("task.jk");
                let output = work.cli(&args);
                assert_eq!(output.status.code(), Some(code), "{}", String::from_utf8_lossy(&output.stderr));
                assert_eq!(output.stdout, stdout.as_bytes());
                assert!(output.stderr.is_empty());
            }
            let mut args = vec!["build"];
            args.extend(flags.iter().copied());
            args.push("task.jk");
            success(&work.cli(&args));
            let output = Command::new(work.path().join("task")).output().unwrap();
            assert_eq!(output.status.code(), Some(code), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.stdout, stdout.as_bytes());
            assert!(output.stderr.is_empty());
        }
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn suspending_cli_file_imports_preserve_pending_and_failures() {
    let work = Workspace::new();
    work.write(
        "app/work.jk",
        r#"
        import joky/task.{yield_now}
        pub fn divide(n: Int32) @suspends -> Int32 { yield_now(); 42 / n }
    "#,
    );
    for (argument, code) in [(1, 42), (0, 1)] {
        work.write(
            "main.jk",
            &format!("import app/work fn main() @suspends -> Int32 {{ work.divide({argument}) }}"),
        );
        for args in [
            vec!["run", "main.jk"],
            vec!["run", "--interpreter", "main.jk"],
        ] {
            let output = work.cli(&args);
            assert_eq!(output.status.code(), Some(code));
            if argument == 0 {
                failure(&output, "Division by zero");
            }
        }
        success(&work.cli(&["build", "main.jk"]));
        let output = Command::new(work.path().join("main")).output().unwrap();
        assert_eq!(output.status.code(), Some(code));
        if argument == 0 {
            failure(&output, "Division by zero");
        }
    }
    work.write("bad.jk", "import joky/task fn main() { task.yield_now() }");
    for args in [
        vec!["run", "bad.jk"],
        vec!["run", "--interpreter", "bad.jk"],
        vec!["build", "bad.jk"],
    ] {
        let output = work.cli(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("suspends"));
    }
}

#[test]
#[cfg(unix)]
fn owning_array_methods_match_all_backends_and_private_module_boundaries() {
    let work = Workspace::new();
    work.write(
        "arrays.jk",
        &format!(
            "import joky/io.{{println}}\n{}  fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(methods_answer()) }}",
            include_str!("fixtures/owned_arrays.jk")
        ),
    );
    for flags in [&[][..], &["--no-prelude"][..]] {
        work.assert_all_backends("arrays.jk", flags, b"42\n", 0);
    }
    work.write(
        "mixed.jk",
        &format!(
            "import joky/io.{{println}}\n{}  fn report_int32(value: Int32) {{ println(&value) }} fn main() {{ report_int32(mixed_all()) }}",
            include_str!("fixtures/mixed_owners.jk")
        ),
    );
    for flags in [&[][..], &["--no-prelude"][..]] {
        work.assert_all_backends("mixed.jk", flags, b"42\n", 0);
    }
    work.write(
        "collections.jk",
        r#"
        import joky/array as array.{type Array}
        import joky/option.{type Option}
        pub struct Batch<T> { pub version: UInt64, values: Array<T>, pub sorted: Bool }
        pub struct Secret { value: Int32 }
        impl Secret {
            pub fn new(value: Int32) -> Self { Self { value: value } }
            pub fn get(&self) -> Int32 { self.value }
        }
        pub struct Node { next: Option<&Node> }
        pub fn empty_node() -> Node { Node { next: None } }
        impl<T> Batch<T> {
            pub fn new() -> Self { Self { version: 0, values: Array.new::<T>(), sorted: true } }
            pub fn push(&mut self, value: T) { self.values.push(value) self.version = self.version + 1 self.sorted = false }
            pub fn get(&self, index: UInt64) -> Option<T> { self.values.get(index) }
            pub fn release(self) { self.values.release() }
        }
    "#,
    );
    work.write(
        "bridge.jk",
        r#"
        import collections.{type Batch}
        pub type BatchAlias<T> = Batch<T>
        pub fn make<T>(value: T) -> BatchAlias<T> {
            var b = Batch.new::<T>()
            b.push(value)
            b
        }
    "#,
    );
    work.write("main.jk", r#"
        import collections.{type Batch, type Secret}
        import bridge as bridge
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        struct Point { value: Int32 }
        fn main() {
            var b = bridge.make(Point { value: 20 })
            b.push(Point { value: 22 })
            let answer = b.get(0).unwrap_or(Point { value: 0 }).value + b.get(1).unwrap_or(Point { value: 0 }).value
            if b.version != 2 || b.sorted { report_int32(0) return }
            b.release()
            let secret = Secret.new(answer)
            report_int32(secret.get())
        }
    "#);
    work.assert_all_backends("main.jk", &[], b"42\n", 0);
    for body in [
        "fn main() { let b = Batch.new::<Int32>() b.values; }",
        "fn main() { var b = Batch.new::<Int32>() b.values.push(42) }",
        "fn main() { let b = Batch { values: array.new::<Int32>() } }",
        "fn hidden<T>(b: Batch<T>) { b.values; } fn main() {}",
        "fn hidden<T>(b: &Batch<T>) { let r = &b.values } fn main() {}",
        "fn hidden<T>(b: &mut Batch<T>) { let r = &mut b.values } fn main() {}",
        "fn main() { let s = Secret.new(42) s.value; }",
        "fn main() { var s = Secret.new(42) s.value = 0 }",
        "fn main() { var s = Secret.new(42) let r = &mut s.value }",
        "fn main() { let s = alloc(Secret.new(42)) s.value; }",
        "fn main() { let s = alloc(Secret.new(42)) s.value = 0 }",
        "fn main() { let s = Secret { value: 42 } }",
        "fn hidden<T>(s: Secret) -> Int32 { s.value } fn main() {}",
        "fn main() { graph { let a = graph.alloc(collections.empty_node()) graph.link(a.next, a) a }; }",
    ] {
        work.write(
            "invalid.jk",
            &format!("import collections.{{type Batch, type Secret}} import joky/array as array {body}"),
        );
        for command in ["run", "build"] {
            failure(&work.cli(&[command, "invalid.jk"]), "private");
        }
        failure(
            &work.cli(&["run", "--interpreter", "invalid.jk"]),
            "private",
        );
    }
}

#[test]
fn owning_arrays_run_and_build_with_exactly_once_cleanup() {
    let work = Workspace::new();
    work.write(
        "owning_arrays.jk",
        include_str!("../examples/owning_arrays.jk"),
    );
    work.assert_all_backends("owning_arrays.jk", &[], b"42\n20\n22\n", 0);
    work.write(
        "owning_enums.jk",
        include_str!("../examples/owning_enums.jk"),
    );
    work.assert_all_backends("owning_enums.jk", &[], b"42\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn owning_tuples_run_and_build_with_drop_and_zip() {
    let work = Workspace::new();
    work.write(
        "owning_tuples.jk",
        include_str!("../examples/owning_tuples.jk"),
    );
    work.assert_all_backends("owning_tuples.jk", &[], b"3\n2\n1\n4\n42\n5\n5\n", 0);
}

#[test]
#[cfg(unix)]
fn borrowed_matches_run_and_build_without_consuming_the_enum() {
    let work = Workspace::new();
    work.write(
        "borrowed_matches.jk",
        include_str!("../examples/borrowed_matches.jk"),
    );
    work.assert_all_backends("borrowed_matches.jk", &[], b"42\n42\n22\n20\n", 0);
}

#[test]
#[cfg(unix)]
fn mutable_borrows_run_and_build_with_in_place_updates() {
    let work = Workspace::new();
    work.write(
        "mutable_borrows.jk",
        include_str!("../examples/mutable_borrows.jk"),
    );
    work.assert_all_backends("mutable_borrows.jk", &[], b"42\n42\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn memory_replace_run_and_build_transfer_old_values() {
    let work = Workspace::new();
    work.write(
        "memory_replace.jk",
        include_str!("../examples/memory_replace.jk"),
    );
    work.assert_all_backends("memory_replace.jk", &[], b"42\n1\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn owning_slices_run_and_build_with_resource_replacement() {
    let work = Workspace::new();
    work.write(
        "owning_slices.jk",
        include_str!("../examples/owning_slices.jk"),
    );
    work.assert_all_backends("owning_slices.jk", &[], b"21\n42\n3\n", 0);
}

#[test]
#[cfg(unix)]
fn swap_remove_runs_and_builds_with_owned_elements() {
    let work = Workspace::new();
    work.write("swap_remove.jk", include_str!("../examples/swap_remove.jk"));
    work.assert_all_backends("swap_remove.jk", &[], b"20\n22\n2\n1\n", 0);
}

#[test]
#[cfg(unix)]
fn ordered_arrays_run_and_build_with_owned_elements() {
    let work = Workspace::new();
    work.write(
        "ordered_arrays.jk",
        include_str!("../examples/ordered_arrays.jk"),
    );
    work.assert_all_backends("ordered_arrays.jk", &[], b"21\n22\n20\n0\n", 0);
}

#[test]
#[cfg(unix)]
fn array_management_runs_and_builds_with_owned_elements() {
    let work = Workspace::new();
    work.write(
        "array_management.jk",
        include_str!("../examples/array_management.jk"),
    );
    work.assert_all_backends("array_management.jk", &[], b"22\n1\n4\n42\n0\n", 0);
}

#[test]
#[cfg(unix)]
fn array_replace_runs_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "array_replace.jk",
        &recording_cli_source(include_str!("fixtures/array_replace.jk")),
    );
    work.assert_all_backends(
        "array_replace.jk",
        &[],
        b"42\n42\n42\n-1\n-2\n-3\n-4\n3\n4\n1\n2\n42\n-1\n-2\n2\n1\n42\n",
        0,
    );
}

#[test]
#[cfg(unix)]
fn checked_failure_runs_and_reports_owned_messages_on_all_backends() {
    let work = Workspace::new();
    let source = &recording_cli_source(include_str!("fixtures/checked_failure.jk"));
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
                stderr.contains("require") || stderr.contains("bytes"),
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
fn int_map_public_operations_match_all_backends() {
    let work = Workspace::new();
    // Internal index checks run in VM and native test registries.
    // CLI validates the same production algorithm through public exports.
    let source: String = recording_cli_source(include_str!("fixtures/int_map.jk"))
        .lines()
        .filter(|line| !line.contains("maps.test_bucket_count"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("maps.test_invariant(&m)", "true")
        .replace("maps.test_deleted_count(&m)", "0");
    work.write("int_map.jk", &source);
    work.assert_all_backends("int_map.jk", &[], b"42\n42\n42\n", 0);
    {
        let name = "capacity_failure";
        work.write(
            "failure.jk",
            &(source.replace("fn main()", "fn successful_main()")
                + &format!("\nfn main() {{ {name}(); }}\n")),
        );
        let assert_failure = |output: std::process::Output| {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("Map capacity exceeds the supported limit"));
        };
        assert_failure(work.cli(&["run", "failure.jk"]));
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(Command::new(work.path().join("failure")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn owned_map_values_and_failures_match_all_backends() {
    check_owned_map_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check in Release"]
fn exhaustive_owned_map_cli_value_matrix() {
    check_owned_map_cli_cases(true);
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the full Release check.
#[cfg(unix)]
fn check_owned_map_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = recording_cli_source(include_str!("fixtures/owned_map.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .replace("maps.test_invariant(&m)", "true");
    // Share specializations within the same groups as the VM tests.
    let groups: &[(&[&str], &str)] = if additional_cases {
        &[
            (&["nested_values"], "-6\n-7\n7\n6\n42\n"),
            (&["arrays", "reuse_budget"], "42\n42\n"),
        ]
    } else {
        &[(
            &["transitions", "return_move"],
            "-1\n-2\n101\n102\n-3\n-4\n3\n4\n-5\n-90\n5\n2\n1\n42\n-8\n8\n42\n",
        )]
    };
    for (names, expected) in groups {
        let body: String = names
            .iter()
            .map(|name| format!("record_int({name}::<Unit>());"))
            .collect();
        work.write("owned_map.jk", &format!("{prefix} fn main() {{ {body} }}"));
        work.assert_all_backends("owned_map.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    for (name, expected, message) in [
        ("clear_failure", "3\n2\n1\n", "Map Drop failure"),
        ("replacement_drop_failure", "2\n4\n", "Map Drop failure"),
        ("runtime_failure", "-1\n1\n", "Map operation failure"),
    ] {
        work.write(
            "failure.jk",
            &format!("{prefix} fn main() {{ {name}::<Unit>(); }}"),
        );
        let assert_failure = |output: std::process::Output| {
            assert!(!output.status.success());
            assert_eq!(output.stdout, expected.as_bytes());
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        };
        assert_failure(work.cli(&["run", "failure.jk"]));
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(Command::new(work.path().join("failure")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn standard_key_protocols_match_all_backends() {
    let work = Workspace::new();
    work.write(
        "key_protocols.jk",
        &recording_cli_source(include_str!("fixtures/key_protocols.jk")),
    );
    work.assert_all_backends("key_protocols.jk", &[], b"42\n", 0);
}

#[test]
#[cfg(unix)]
fn generic_map_public_keys_values_and_examples_match_all_backends() {
    check_generic_map_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check in Release"]
fn exhaustive_generic_map_cli_type_matrix() {
    check_generic_map_cli_cases(true);
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the full Release check.
#[cfg(unix)]
fn check_generic_map_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = &recording_cli_source(include_str!("fixtures/generic_map.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    let groups: &[(&[&str], &str)] = if additional_cases {
        &[
            (&["integer32"], "42\n"),
            (&["integer64"], "42\n"),
            (&["unsigned32"], "42\n"),
            (&["unsigned64"], "42\n"),
            (&["booleans"], "42\n"),
            (&["units"], "42\n"),
            (&["collisions"], "42\n"),
            (&["enums"], "42\n"),
            (&["lifecycle"], "42\n"),
            (&["array_of_maps"], "42\n"),
            (&["map_of_arrays"], "42\n"),
            (&["map_of_maps"], "42\n"),
        ]
    } else {
        // These cases share specializations, as in the VM test group.
        &[(
            &["retained_key", "cached_rehash"],
            "11\n11\n11\n42\n101\n102\n201\n900\n901\n101\n201\n102\n201\n202\n42\n",
        )]
    };
    for (names, expected) in groups {
        let body: String = names
            .iter()
            .map(|name| format!("record_int({name}::<Unit>());"))
            .collect();
        work.write(
            "generic_map.jk",
            &format!("{prefix} fn main() {{ {body} }}"),
        );
        work.assert_all_backends("generic_map.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    work.write("maps.jk", include_str!("../examples/maps.jk"));
    work.assert_all_backends("maps.jk", &[], b"42\n42\n42\n42\n42\n", 0);
    work.write("map_batches.jk", include_str!("../examples/map_batches.jk"));
    work.assert_all_backends("map_batches.jk", &[], b"42\n", 0);
    for (name, message) in [
        ("hash_failure", "Map Hash failure"),
        ("eq_failure", "Map Eq failure"),
    ] {
        work.write(
            "failure.jk",
            &format!("{prefix} fn main() {{ {name}::<Unit>(); }}"),
        );
        let assert_failure = |output: std::process::Output| {
            assert!(!output.status.success());
            assert_eq!(output.stdout, b"2\n1\n");
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        };
        assert_failure(work.cli(&["run", "failure.jk"]));
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(Command::new(work.path().join("failure")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn map_borrowing_and_shared_values_match_all_backends() {
    check_map_borrowing_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check in Release"]
fn exhaustive_map_borrowing_cli_value_matrix() {
    check_map_borrowing_cli_cases(true);
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the full Release check.
#[cfg(unix)]
fn check_map_borrowing_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = recording_cli_source(include_str!("fixtures/map_borrowing.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    for (name, expected) in [
        ("copy_views", "42\n"),
        ("owned_views", "-1\n-2\n2\n1\n42\n"),
        ("nested_views", "42\n"),
        ("reference_values", "42\n"),
        ("shared_aggregate", "42\n"),
        ("copied_reference", "42\n"),
        ("returned_reference", "42\n"),
        ("aggregate_references", "42\n"),
    ] {
        if additional_cases == matches!(name, "owned_views" | "shared_aggregate") {
            continue;
        }
        work.write(
            "map_borrowing.jk",
            &format!("{prefix} fn main() {{ record_int({name}::<Unit>()) }}"),
        );
        work.assert_all_backends("map_borrowing.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    for (name, expression) in [
        ("missing_copy", "m.borrow(2)"),
        ("missing_mut", "m.borrow_mut(2)"),
        ("missing_owner", "m.borrow_ref(2)"),
        ("missing_owner_mut", "m.borrow_mut_ref(2)"),
    ] {
        let source = format!("{prefix} fn main() {{ {name}::<Unit>(); }}");
        let start = source.rfind(expression).unwrap();
        let end = start + expression.len();
        let line = source[..start].bytes().filter(|&b| b == b'\n').count() + 1;
        work.write("failure.jk", &source);
        let assert_failure = |output: std::process::Output, native: bool| {
            assert!(!output.status.success(), "{output:?}");
            assert_eq!(
                output.stdout,
                if name.contains("owner") {
                    b"-1\n1\n".as_slice()
                } else {
                    b""
                }
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("Map key not found"), "{stderr}");
            let call = if native {
                format!("(bytes {start}..{end})")
            } else {
                format!("failure.jk:{line}")
            };
            assert!(stderr.contains(&call), "missing caller {call}: {stderr}");
        };
        assert_failure(work.cli(&["run", "failure.jk"]), false);
        assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]), false);
        success(&work.cli(&["build", "failure.jk"]));
        assert_failure(
            Command::new(work.path().join("failure")).output().unwrap(),
            true,
        );
    }
}

#[test]
#[cfg(unix)]
fn map_borrowing_cross_module_aliases_match_all_backends() {
    let work = Workspace::new();
    work.write(
        "map_views_library.jk",
        include_str!("fixtures/map_views_library.jk"),
    );
    let prefix = recording_cli_source(include_str!("fixtures/map_views_modules.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    for name in ["copies", "owners", "references", "iteration"] {
        work.write(
            "map_views_modules.jk",
            &format!("{prefix} fn main() {{ record_int({name}::<Unit>()) }}"),
        );
        work.assert_all_backends("map_views_modules.jk", &[], b"42\n", 0);
    }
}

#[test]
#[cfg(unix)]
fn map_iteration_and_batches_match_all_backends() {
    check_map_iteration_cli_cases(false);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: additional CLI Map matrix; run by the stress check in Release"]
fn exhaustive_map_iteration_cli_value_matrix() {
    check_map_iteration_cli_cases(true);
}

// VM tests cover the complete matrix daily. Keep representative CLI cases and
// diagnostics here; additional CLI specializations run in the full Release check.
#[cfg(unix)]
fn check_map_iteration_cli_cases(additional_cases: bool) {
    let work = Workspace::new();
    let prefix = recording_cli_source(include_str!("fixtures/map_iteration.jk"))
        .split("fn main()")
        .next()
        .unwrap()
        .to_owned();
    for (name, expected) in [
        ("copy_views", "42\n"),
        ("consuming", "42\n"),
        ("batches", "42\n"),
        (
            "owned_pipeline",
            "-1\n-2\n-3\n1\n-4\n2\n3\n900\n901\n4\n42\n",
        ),
        ("references", "42\n"),
        ("unbounded", "42\n"),
        ("protocol_free_export", "101\n102\n42\n"),
    ] {
        if additional_cases == matches!(name, "owned_pipeline" | "batches") {
            continue;
        }
        work.write(
            "map_iteration.jk",
            &format!("{prefix} fn main() {{ record_int({name}::<Unit>()) }}"),
        );
        work.assert_all_backends("map_iteration.jk", &[], expected.as_bytes(), 0);
    }
    if additional_cases {
        return;
    }
    work.write(
        "failure.jk",
        &format!("{prefix} fn main() {{ drop_failure::<Unit>(); }}"),
    );
    let assert_failure = |output: std::process::Output| {
        assert!(!output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Batch drop failure"));
        let events: Vec<i32> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(&events[..7], &[-1, 101, -2, -3, -4, 101, 1]);
        let mut remaining = events[7..].to_vec();
        remaining.sort();
        assert_eq!(remaining, vec![2, 3, 4]);
    };
    assert_failure(work.cli(&["run", "failure.jk"]));
    assert_failure(work.cli(&["run", "--interpreter", "failure.jk"]));
    success(&work.cli(&["build", "failure.jk"]));
    assert_failure(Command::new(work.path().join("failure")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn map_stress_matches_the_owner_ledger_in_standalone_aot() {
    // One cycle covers growth, dense deletion, rehash, random operations and clear.
    check_map_stress_in_standalone_aot(256);
}

#[test]
#[cfg(unix)]
#[ignore = "exhaustive: long AOT Map model matrix; run by the stress check in Release"]
fn exhaustive_map_stress_matches_the_owner_ledger_in_standalone_aot() {
    check_map_stress_in_standalone_aot(map_stress_oracle::STEPS);
}

#[cfg(unix)]
fn check_map_stress_in_standalone_aot(steps: i32) {
    use map_stress_oracle as oracle;
    let work = Workspace::new();
    let mut source = recording_cli_source(&oracle::source(oracle::SEEDS[0], 0, 0, false));
    source.push_str("\nfn main() {");
    for distribution in 0..3 {
        for seed in oracle::SEEDS {
            source.push_str(&format!(
                " stress({seed},{distribution},{steps}) record_int(-2147483647) "
            ));
        }
    }
    source.push('}');
    work.write("stress.jk", &source);
    success(&work.cli(&["build", "stress.jk"]));
    let output = Command::new(work.path().join("stress")).output().unwrap();
    success(&output);
    let events: Vec<i32> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|v| v.parse().unwrap())
        .collect();
    let chunks: Vec<_> = events.split(|&v| v == -2147483647).collect();
    assert_eq!(chunks.len(), 7);
    assert!(chunks[6].is_empty());
    for (index, chunk) in chunks[..6].iter().enumerate() {
        oracle::verify(chunk, oracle::SEEDS[index % 2], steps, true);
    }
}

#[test]
#[cfg(unix)]
fn show_print_and_interpolation_match_all_backends() {
    let work = Workspace::new();
    work.write("example.jk", include_str!("../examples/show.jk"));
    work.assert_all_backends(
        "example.jk",
        &[],
        b"player=Joky:42, next=43\nscores=[41, 42]\nJoky:42\n",
        0,
    );
    work.write("show.jk", include_str!("fixtures/show.jk"));
    work.write(
        "main.jk",
        r#"
        import show as samples
        import joky/io.{print, println}
        fn main() {
            let scalar = samples.scalar_text() println(&scalar)
            let custom = samples.custom_text() println(&custom)
            let array = samples.array_text() println(&array)
            let option = samples.option_text() println(&option)
            let result = samples.result_text() println(&result)
            let order = samples.evaluation_text() println(&order)
            let empty = "" print(&empty)
            let prefix = "tail=" print(&prefix)
            let rune = '\0' print(&rune)
            let flag = false println(&flag)
        }
    "#,
    );
    let expected = "-128|-32768|-2147483648|-9223372036854775808|255|65535|4294967295|18446744073709551615|1.5|-0|true|界|()|NaN|inf\n<Item(42:hé\0🙂)>|Item(42:hé\0🙂)|hé\0🙂\n[Item(1:one), Item(2:two)]|[Item(1:one), Item(2:two)]\nSome(Item(42:yes))|None|Some(Item(42:yes))\nOk(Item(42:yes))|Err(no)|Ok(Item(42:yes))\n{x}=1:2:[2]:kept:block:2:kept\ntail=\0false\n";
    work.assert_all_backends("main.jk", &[], expected.as_bytes(), 0);
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
            include_str!("fixtures/cps_local_borrows.jk")
        ),
    );
    work.assert_all_backends("locals.jk", &[], b"42\n42\n43\n42\n1.5\ntrue\n()\n42\n", 0);
}

#[test]
fn cps_local_borrows_reject_live_loans_at_suspension() {
    let work = Workspace::new();
    for body in [
        "let x = 42 let r = &x yield_now() println(r)",
        "let x = 42 let r = &x let h = Holder { value: r } yield_now() println(h.value)",
        "let x = 42 read(x, later());",
        "let x = 42 read(&x, later());",
        "let x = 42 consume(&x)",
        "let x = 42 parallel { | consume(&x) }",
        "let x = 42 let r = &x scope { branch { yield_now() } } println(r)",
        "let x = 42 let r = &x scope { branch { println(r) } }",
    ] {
        work.write(
            "invalid.jk",
            &format!(
                r#"
            import joky/task.{{yield_now}}
            struct Holder {{ value: &Int32 }}
            fn consume(value: &Int32) @suspends {{}}
            fn later() @suspends -> Int32 {{ yield_now() 1 }}
            fn read(value: &Int32, other: Int32) @noescape -> Int32 {{ (*value) + other }}
            fn main() @suspends {{ {body} }}
        "#
            ),
        );
        for args in [
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
            vec!["build", "invalid.jk"],
        ] {
            failure(
                &work.cli(&args),
                "local borrow cannot remain live across suspension",
            );
        }
    }
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
        fn make() -> Item { println("make") Item { text: "temporary" } }
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
            println(read(n, { n = 2 n }))
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
        fn main() @suspends { println("before") yield_now() println("after") }
    "#,
    );
    work.assert_all_backends("async_auto.jk", &[], b"before\nafter\n", 0);
    work.write(
        "explicit_auto.jk",
        "import joky/io.{print, println} fn main() { print(42) println(43) }",
    );
    work.assert_all_backends("explicit_auto.jk", &["--no-prelude"], b"4243\n", 0);
    work.assert_all_backends("explicit_auto.jk", &[], b"4243\n", 0);
    work.write(
        "qualified_io.jk",
        "import joky/prelude as p fn main() { p.print(42) p.println(43) }",
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
        fn make() -> Resource { println("make") Resource { text: "owned" } }
        fn inspect(value: &Resource) @noescape -> String { value.text.clone() }
        fn explode(value: &Resource) @noescape { 1 / 0; }
        fn pair(value: &Resource, ignored: Int32) @noescape {}
        fn fail() -> Int32 { 1 / 0 }
    "#;
    work.write(
        "lifetime.jk",
        &format!("{definitions} fn main() {{ let result = inspect(make()) println(result) }}"),
    );
    work.assert_all_backends("lifetime.jk", &[], b"make\ndrop\nowned\n", 0);
    let failing_drop = definitions.replace("println(\"drop\") }", "println(\"drop\") 1 / 0; }");
    for (definitions, entry) in [
        (definitions, "explode(make())"),
        (definitions, "pair(make(), fail())"),
        (
            failing_drop.as_str(),
            "let result = inspect(make()) println(result)",
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
        "fn edit(value: &mut Int32) @noescape {} fn main() { var n = 42 edit(n) }",
        "fn read(value: &Int32) @suspends {} fn main() @suspends { read(42) }",
        "fn read(value: &Int32) @noescape {} fn main() { let f = read f(42) }",
        "fn read(value: &Int32) @noescape -> &Int32 { value } fn main() { read(42); }",
        "fn consume(value: String) {} fn main() { let text = \"owned\" consume(text) println(text) }",
        "fn both(value: &String, other: &mut String) @noescape {} fn main() { var text = \"owned\" both(text, &mut text) }",
        "fn pair(value: &Int32, n: Int32) @noescape {} fn later() @suspends -> Int32 { 42 } fn main() @suspends { pair(7, later()) }",
    ] {
        work.write("contract.jk", source);
        for args in [vec!["run", "contract.jk"], vec!["run", "--interpreter", "contract.jk"], vec!["build", "contract.jk"]] {
            let output = work.cli(&args);
            assert!(!output.status.success(), "{source}: {output:?}");
            assert!(output.stdout.is_empty());
            assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"), "{source}: {output:?}");
        }
    }
}

#[test]
#[cfg(unix)]
fn show_map_and_explicit_io_work_without_prelude_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "map.jk",
        r#"
        import joky/io.{print, println}
        import joky/map.{type Map}
        fn main() {
            var values = Map.new::<Int32, String>()
            values.insert(42, "hello")
            let message = "map={values}" println(message)
            println(values)
            values.remove(42)
            let empty = "{values}" println(empty)
        }
    "#,
    );
    work.assert_all_backends(
        "map.jk",
        &["--no-prelude"],
        b"map={42: hello}\n{42: hello}\n{}\n",
        0,
    );
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
            r#"struct Missing {} fn main() { let value = Missing {} "{value}" }"#,
            "Show",
        ),
        (r#"fn main() { f"{42}" }"#, "remove the `f` prefix"),
    ] {
        work.write("invalid.jk", source);
        for args in [
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
            vec!["build", "invalid.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(message), "{error}");
            assert!(error.contains("invalid.jk"), "{error}");
        }
    }
    work.write("failure.jk", r#"
        import joky/show.{type Show}
        import joky/string.{type StringBuilder}
        import joky/drop.{type Drop}
        import joky/io.{println}
        struct Resource { text: String }
        impl Drop for Resource { fn drop(&mut self) { let event = "drop" println(&event) } }
        impl Show for Resource { fn show(&self, output: &mut StringBuilder) @noescape { output.push_str(&self.text) 1 / 0; } }
        fn make() -> Resource { Resource { text: "owned" } }
        fn main() { let message = "{make()}" println(&message) }
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
fn show_print_from_sync_segment_in_cps_main_matches_all_backends() {
    let work = Workspace::new();
    work.write(
        "async.jk",
        r#"
        import joky/io.{println}
        import joky/task
        fn report(value: Int32) { let message = "value={value}" println(&message) }
        fn main() @suspends { report(41) task.yield_now() report(42) }
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
fn host_executor_drives_standalone_aot_with_the_same_runtime() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("fixtures/host_executor.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("host_executor").unwrap();
    compiler.compile_module(&ast).unwrap();
    work.write(
        "driver.rs",
        &format!(
            "const WORK_SLOTS: usize = {};\nconst BRANCH_SLOTS: usize = {};\n{}",
            compiler.cps_frame_slots("work").unwrap(),
            compiler.cps_frame_slots("branches").unwrap(),
            include_str!("fixtures/host_executor.rs")
        ),
    );
    compiler.write_object(&work.path().join("tasks.o")).unwrap();
    // Link generated AOT code into a Rust embedding host, with exactly one copy
    // of the runtime (including its clock/waker TLS), just like production.
    let library = runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!(
            "link-arg={}",
            work.path().join("tasks.o").display()
        ))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn text_state_hosts_execute_in_standalone_aot() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("fixtures/text_state_hosts.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("text_state_hosts").unwrap();
    compiler.compile_module(&ast).unwrap();
    work.write(
        "driver.rs",
        &format!(
            "const ASYNC_SLOTS: usize = {};\n{}",
            compiler.cps_frame_slots("check_text_hosts_async").unwrap(),
            include_str!("fixtures/text_state_hosts.rs")
        ),
    );
    compiler.write_object(&work.path().join("text.o")).unwrap();
    let library = runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!("link-arg={}", work.path().join("text.o").display()))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn scalar_wait_requests_execute_in_standalone_aot() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("fixtures/scalar_wait_requests.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("scalar_wait_requests").unwrap();
    compiler.compile_module(&ast).unwrap();
    work.write(
        "driver.rs",
        &format!(
            "const WAIT_SLOTS: usize = {};\n{}",
            compiler.cps_frame_slots("wait_loop").unwrap(),
            include_str!("fixtures/scalar_wait_requests.rs")
        ),
    );
    compiler.write_object(&work.path().join("waits.o")).unwrap();
    let library = runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!(
            "link-arg={}",
            work.path().join("waits.o").display()
        ))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn record_patterns_run_and_reject_invalid_patterns_on_all_frontends() {
    let work = Workspace::new();
    work.write(
        "record_patterns.jk",
        include_str!("../examples/record_patterns.jk"),
    );
    work.assert_all_backends(
        "record_patterns.jk",
        &[],
        b"34\n13\n13\n2\nAda\n42\n11\nrest\n30\n31\n",
        0,
    );
    for (source, message) in [
        (
            "struct A { x: Int32 } struct B { x: Int32 } fn unused<T>(v: A) -> Int32 { let B { x } = v x } fn main() {}",
            "requires B",
        ),
        (
            "struct Pair<T> { x: T } fn unused<T>(v: Pair<T>) -> T { let Pair::<Int32> { x } = v x } fn main() {}",
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
            "struct User { name: String, score: Int32 } fn main() -> Int32 { let u = User { name: \"a\", score: 1 } let User { score, .. } = u u.score }",
            "moved or uninitialized",
        ),
    ] {
        work.write("invalid_pattern.jk", &format!("// 🙂\n{source}"));
        for args in [
            vec!["run", "invalid_pattern.jk"],
            vec!["run", "--interpreter", "invalid_pattern.jk"],
            vec!["build", "invalid_pattern.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
        }
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
    for args in [
        vec!["run", "moved.jk"],
        vec!["run", "--interpreter", "moved.jk"],
        vec!["build", "moved.jk"],
    ] {
        failure(&work.cli(&args), "implementing Drop");
    }
}

#[test]
#[cfg(unix)]
fn trait_sources_example_matches_all_backends() {
    let work = Workspace::new();
    work.write(
        "trait_sources.jk",
        include_str!("../examples/trait_sources.jk"),
    );
    work.assert_all_backends("trait_sources.jk", &[], b"42\n", 0);
}

#[test]
#[cfg(unix)]
fn borrowed_dyn_example_matches_all_backends() {
    let work = Workspace::new();
    work.write(
        "borrowed_dyn.jk",
        include_str!("../examples/borrowed_dyn.jk"),
    );
    work.assert_all_backends("borrowed_dyn.jk", &[], b"21\n42\n22\n42\n", 0);
}

#[test]
#[cfg(unix)]
fn list_example_and_cross_module_aliases_run_in_all_backends() {
    let work = Workspace::new();
    work.write("lists.jk", include_str!("../examples/lists.jk"));
    work.assert_all_backends(
        "lists.jk",
        &["--no-prelude"],
        b"last\n0\nlast\nbefore\nextra\ntail\n",
        0,
    );
    work.write("views.jk", include_str!("fixtures/list_views.jk"));
    work.write(
        "aliased.jk",
        r#"
        import views as chains
        import joky/array.{type Array}
        import joky/element as element
        import joky/io.{println}
        fn main() {
            var input=Array.new::<Int32>() input.push(20) input.push(22)
            var list=chains.import_array(input)
            element.write(chains.edit(&mut list,0),20)
            let sum=element.read(chains.view(&list,0))+chains.copied(&list,1).unwrap_or(0)
            list.reverse()
            let output=chains.export(list)
            println(&sum) println(output.get(0).unwrap_or(0))
        }
    "#,
    );
    work.assert_all_backends("aliased.jk", &["--no-prelude"], b"42\n22\n", 0);
}

#[test]
#[cfg(unix)]
fn set_example_and_cross_module_aliases_run_in_all_backends() {
    let work = Workspace::new();
    work.write("sets.jk", include_str!("../examples/sets.jk"));
    work.assert_all_backends(
        "sets.jk",
        &["--no-prelude"],
        b"2\ntrue\n3\ntrue\ntrue\n3\n",
        0,
    );
    work.write("views.jk", include_str!("fixtures/set_views.jk"));
    work.write(
        "aliased.jk",
        r#"
        import views as groups
        import joky/array.{type Array}
        import joky/io.{println}
        import joky/string
        fn main() {
            var input=Array.new::<String>() input.push("apple") input.push("apple")
            var set=groups.import_array(input)
            var query="apple"
            println(groups.contains(&set,query))
            println(groups.remove(&mut set,query))
            query="pear"
            groups.insert(&mut set,query)
            let output=groups.export(set)
            println(output.borrow_ref(0))
        }
    "#,
    );
    work.assert_all_backends("aliased.jk", &["--no-prelude"], b"true\ntrue\npear\n", 0);
}

#[test]
#[cfg(unix)]
fn net0_loopback_probe_executes_in_standalone_aot() {
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("fixtures/net0.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("net0").unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    compiler.compile_module(&ast).unwrap();
    let mut driver = include_str!("fixtures/net0_host.rs").to_owned();
    let entries = [
        "data",
        "eof",
        "domain_error",
        "startup_failure",
        "pending_failure",
        "timeout",
        "sibling_failure",
        "unstarted",
    ];
    driver.push_str("\nextern \"C\" {\n");
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name = \"{entry}\"] fn entry_{entry}(frame: *mut CpsFrame) -> i32;\n"
        ));
    }
    driver.push_str("}\nfn main() { check_suite(|name| { let (entry, slots): (joky::runtime::cps::CpsEntry,usize) = match name {\n");
    for entry in entries {
        driver.push_str(&format!(
            "\"{entry}\" => (entry_{entry}, {}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_ => unreachable!() }; unsafe { Continuation::from_native(entry,CpsFrame::new(slots),Rc::new(())) } }); }\n");
    work.write("driver.rs", &driver);
    compiler.write_object(&work.path().join("net0.o")).unwrap();
    let library = runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!("link-arg={}", work.path().join("net0.o").display()))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn suspending_owners_execute_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("fixtures/suspending_owners.jk").replace(
        "extern fn owner_event(id: Int32)",
        "import joky/io.{println} fn owner_event(id: Int32) { println(id) }",
    );
    for (entry, ids, fails) in [
        ("record", vec![1], false),
        ("result_ok", vec![1], false),
        ("result_err", vec![2], false),
        ("tuple", vec![1, 2], false),
        ("pair", vec![1, 2], false),
        ("array_result", vec![], false),
        ("builder", vec![], false),
        ("map", vec![1], false),
        ("array_owners", vec![1], false),
        ("race_unstarted", vec![1], false),
        ("race_started", vec![1], false),
        ("dynamic", vec![1, 2, 3], false),
        ("dynamic_failure", vec![1, 2, 3], true),
        ("failed_return", vec![1, 9], true),
        ("failure", vec![1, 2], true),
    ] {
        work.write(
            "owners.jk",
            &format!("{source}\nfn main() @suspends {{ println({entry}()) }}"),
        );
        success(&work.cli(&["build", "owners.jk"]));
        let output = Command::new(work.path().join("owners")).output().unwrap();
        assert_eq!(
            output.status.success(),
            !fails,
            "{entry}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events: Vec<i32> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|n| n.parse().unwrap())
            .collect();
        for id in ids {
            assert_eq!(
                events.iter().filter(|&&n| n == -id).count(),
                1,
                "{entry}: {events:?}"
            );
            assert_eq!(
                events.iter().filter(|&&n| n == id).count(),
                1,
                "{entry}: {events:?}"
            );
        }
        assert_eq!(events.contains(&42), !fails, "{entry}: {events:?}");
    }
}

#[test]
#[cfg(unix)]
fn resource_owners_execute_in_standalone_aot() {
    resource_program_in_standalone_aot(
        include_str!("fixtures/resource_owners.jk"),
        &[
            "result",
            "pair",
            "array_task",
            "overwrite",
            "partial",
            "unstarted",
            "dynamic",
            "borrow",
        ],
    );
}
#[test]
#[cfg(unix)]
fn resource_handoff_execute_in_standalone_aot() {
    resource_program_in_standalone_aot(
        include_str!("fixtures/resource_handoff.jk"),
        &[
            "success",
            "startup_failure",
            "output_failure",
            "invalid_pending",
            "empty_success",
            "runtime_failure",
            "race_cancel",
            "race_unstarted",
        ],
    );
}
#[cfg(unix)]
fn resource_program_in_standalone_aot(source: &str, entries: &[&str]) {
    let work = Workspace::new();
    let ast = joky::parser::parse(source).unwrap();
    let mut compiler = joky::aot::AotCompiler::new("resource_owners").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("make", "Socket")
        .unwrap();
    compiler
        .register_resource_factory_async("acquire", "Socket")
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let mut driver = include_str!("fixtures/resource_factory_host.rs").to_owned();
    driver.push_str(
        "\nuse joky::runtime::suspension::Continuation; use std::rc::Rc;\nextern \"C\" {\n",
    );
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name = \"{entry}\"] fn entry_{entry}(frame: *mut CpsFrame) -> i32;\n"
        ));
    }
    driver.push_str("}\nfn main() {\n");
    for entry in entries {
        driver.push_str(&format!("for cancel in [false,true] {{ reset(); reset_stops(); let mut task=unsafe {{ Continuation::from_native(entry_{entry}, CpsFrame::new({}), Rc::new(())) }}; let mut failed=false; for _ in 0..20 {{ if task.is_finished() {{ break; }} if task.resume().is_err() {{ failed=true; break; }} if cancel {{ break; }} }} if !cancel {{ assert_eq!(failed, {}); if !failed {{ assert_eq!(task.cps_result(),Some(42)); }} }} drop(task); check(); assert!(stops().len()<=1); }}\n",compiler.cps_frame_slots(entry).unwrap(),*entry=="partial" || entry.ends_with("failure") || *entry=="invalid_pending" || *entry=="empty_success"));
    }
    driver.push_str("}\n");
    work.write("driver.rs", &driver);
    compiler
        .write_object(&work.path().join("resources.o"))
        .unwrap();
    let library = runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!(
            "link-arg={}",
            work.path().join("resources.o").display()
        ))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn resource_bytes_execute_in_standalone_aot() {
    use joky::runtime::byte_request::ByteIoKind;
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("fixtures/resource_bytes.jk")).unwrap();
    let mut compiler =
        joky::aot::AotCompiler::with_symbol_prefix("resource_bytes", "net1a_bytes_").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("byte_make", "Socket")
        .unwrap();
    compiler
        .register_resource_io("read", "Socket", ByteIoKind::Read)
        .unwrap();
    compiler
        .register_resource_io("write", "Socket", ByteIoKind::Write)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let entries = [
        "receive",
        "eof",
        "send",
        "empty_write",
        "zero_read",
        "oversized",
        "bad_count",
        "zero_progress",
        "cancel_reuse",
        "unstarted",
        "field",
        "indirect",
        "independent",
        "dynamic",
    ];
    let mut driver = include_str!("fixtures/resource_bytes_host.rs")
        .replace(
            "__joky_resource_factory_v1_",
            "__joky_resource_factory_v1_net1a_bytes_",
        )
        .replace(
            "__joky_resource_io_v1_",
            "__joky_resource_io_v1_net1a_bytes_",
        );
    driver.push_str(
        "\nuse joky::runtime::suspension::Continuation; use std::rc::Rc;\nextern \"C\" {\n",
    );
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name=\"net1a_bytes_{entry}\"] fn entry_{entry}(frame:*mut CpsFrame)->i32;\n"
        ));
    }
    driver.push_str("}\nfn main(){\n");
    for entry in entries {
        let fails = ["zero_read", "oversized", "bad_count", "zero_progress"].contains(&entry);
        driver.push_str(&format!("for cancel in [false,true] {{ reset(); let mut task=unsafe {{Continuation::from_native(entry_{entry},CpsFrame::new({}),Rc::new(()))}}; let mut failed=false; for _ in 0..50 {{if task.is_finished() {{break;}} if task.resume().is_err() {{failed=true;break;}} if cancel {{break;}} }} if !cancel {{assert_eq!(failed,{fails},\"{entry}\");if !failed {{assert_eq!(task.cps_result(),Some(42),\"{entry}\");}}}} drop(task);check(); if !cancel && \"{entry}\"==\"send\" {{assert_eq!(sent(),vec![0,255]);}} }}\n",compiler.cps_frame_slots(entry).unwrap()));
    }
    driver.push_str("}\n");
    work.write("driver.rs", &driver);
    compiler
        .write_object(&work.path().join("resource_bytes.o"))
        .unwrap();
    let library = runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!(
            "link-arg={}",
            work.path().join("resource_bytes.o").display()
        ))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn resource_bytes_c_header_matches_descriptor_layouts() {
    use joky::runtime::{
        byte_request::ResourceIoV1,
        resource::{ResourceFactoryCpsV1, ResourceFactoryV1},
    };
    let work = Workspace::new();
    let mut compiler = joky::aot::AotCompiler::with_symbol_prefix("headers", "net_").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("make", "Socket")
        .unwrap();
    compiler
        .register_resource_factory_async("acquire", "Socket")
        .unwrap();
    compiler
        .register_resource_io(
            "read",
            "Socket",
            joky::runtime::byte_request::ByteIoKind::Read,
        )
        .unwrap();
    let header = compiler.resource_header_v1();
    assert!(header.contains("JokyResourceFactoryV1 __joky_resource_factory_v1_net_make"));
    assert!(header.contains("JokyResourceFactoryCpsV1 __joky_resource_factory_cps_v1_net_acquire"));
    assert!(header.contains("JokyResourceIoV1 __joky_resource_io_v1_net_read"));
    work.write("bindings.h", &header);
    work.write("layout.c",&format!("#include \"bindings.h\"\n_Static_assert(sizeof(JokyResourceFactoryV1)=={},\"factory size\");\n_Static_assert(offsetof(JokyResourceFactoryV1,release)=={},\"release offset\");\n_Static_assert(sizeof(JokyResourceFactoryCpsV1)=={},\"CPS size\");\n_Static_assert(offsetof(JokyResourceFactoryCpsV1,stop)=={},\"stop offset\");\n_Static_assert(sizeof(JokyResourceIoV1)=={},\"I/O size\");\n_Static_assert(offsetof(JokyResourceIoV1,poll)=={},\"poll offset\");\nint main(void) {{ return JOKY_BYTE_REQUEST_MAX_V1==1048576 ? 0 : 1; }}\n",std::mem::size_of::<ResourceFactoryV1>(),std::mem::offset_of!(ResourceFactoryV1,release),std::mem::size_of::<ResourceFactoryCpsV1>(),std::mem::offset_of!(ResourceFactoryCpsV1,stop),std::mem::size_of::<ResourceIoV1>(),std::mem::offset_of!(ResourceIoV1,poll)));
    let output = Command::new("cc")
        .arg("-std=c11")
        .arg("-Werror")
        .arg("-I")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
        .arg(work.path().join("layout.c"))
        .arg("-o")
        .arg(work.path().join("layout"))
        .output()
        .unwrap();
    success(&output);
    success(&Command::new(work.path().join("layout")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn net1b_reactor_executes_in_standalone_aot() {
    use joky::runtime::byte_request::ByteIoKind;
    let work = Workspace::new();
    let ast = joky::parser::parse(include_str!("fixtures/net1b.jk")).unwrap();
    let mut compiler = joky::aot::AotCompiler::with_symbol_prefix("net1b", "net1b_").unwrap();
    compiler.register_resource_type("Socket").unwrap();
    compiler
        .register_resource_factory("net1b_make", "Socket")
        .unwrap();
    compiler
        .register_resource_io("net1b_read", "Socket", ByteIoKind::Read)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let mut driver = include_str!("fixtures/net1b_host.rs")
        .replace(
            "__joky_resource_factory_v1_net1b_make",
            "__joky_resource_factory_v1_net1b_net1b_make",
        )
        .replace(
            "__joky_resource_io_v1_net1b_read",
            "__joky_resource_io_v1_net1b_net1b_read",
        );
    driver.push_str("\nextern \"C\" {\n");
    let entries = [
        "receive",
        "eof",
        "timeout",
        "sibling_failure",
        "reuse",
        "unstarted",
        "dynamic",
    ];
    for entry in entries {
        driver.push_str(&format!(
            "#[link_name=\"net1b_{entry}\"] fn entry_{entry}(frame:*mut CpsFrame)->i32;\n"
        ));
    }
    driver.push_str("}\nfn main(){suite(|name| { let (entry,slots)=match name {\n");
    for entry in entries {
        driver.push_str(&format!(
            "\"{entry}\" => (entry_{entry} as joky::runtime::cps::CpsEntry,{}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_=>unreachable!()}; unsafe {Continuation::from_native(entry,CpsFrame::new(slots),Rc::new(()))}}); blocking_suite(|name| { let (entry,slots)=match name {\n");
    for entry in entries {
        driver.push_str(&format!(
            "\"{entry}\" => (entry_{entry} as joky::runtime::cps::CpsEntry,{}),\n",
            compiler.cps_frame_slots(entry).unwrap()
        ));
    }
    driver.push_str("_=>unreachable!()}; let mut frame=CpsFrame::new(slots); joky::runtime::error::joky_runtime_error_clear(); let status=unsafe {joky::runtime::driver::joky_cps_block_on(entry,&mut frame)}; assert_eq!(status,if name==\"sibling_failure\" {3} else {0}); if status==0 {assert_eq!(joky::runtime::error::joky_runtime_error_code(),0); Ok(frame.result)} else {assert_ne!(joky::runtime::error::joky_runtime_error_code(),0); joky::runtime::error::joky_runtime_error_clear(); Err(joky::Error::new(joky::ErrorPhase::Runtime,\"Native driver failure\"))}});}\n");
    work.write("driver.rs", &driver);
    compiler.write_object(&work.path().join("net1b.o")).unwrap();
    let library = runtime_archive().with_extension("rlib");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(work.path().join("driver.rs"))
        .arg("--extern")
        .arg(format!("joky={}", library.display()))
        .arg("-L")
        .arg(format!(
            "dependency={}",
            library.parent().unwrap().display()
        ))
        .arg("-C")
        .arg(format!(
            "link-arg={}",
            work.path().join("net1b.o").display()
        ))
        .arg("-o")
        .arg(work.path().join("driver"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    success(&Command::new(work.path().join("driver")).output().unwrap());
}

#[test]
#[cfg(unix)]
fn net2_tcp_public_module_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("tcp.jk", include_str!("fixtures/tcp.jk"));
    work.assert_all_backends("tcp.jk", &[], b"", 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net2_tcp_connection_refused_matches_all_backends() {
    mod endpoint {
        include!("support/tcp.rs");
    }
    let (_reserved, port) = endpoint::refused_endpoint();
    let work = Workspace::new();
    work.write(
        "refused.jk",
        &include_str!("fixtures/tcp_refused.jk").replace("PORT", &port.to_string()),
    );
    work.assert_all_backends("refused.jk", &[], b"", 0);
}

#[test]
fn net3_tcp_composition_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "tcp_composition.jk",
        include_str!("fixtures/tcp_composition.jk"),
    );
    work.assert_all_backends("tcp_composition.jk", &[], b"", 0);
}

#[test]
fn async_owner_borrows_short_segments_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "async_owner_borrows.jk",
        include_str!("fixtures/async_owner_borrows.jk"),
    );
    work.assert_all_backends("async_owner_borrows.jk", &[], b"", 0);
}

#[test]
fn async_owner_views_execute_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "async_owner_views.jk",
        include_str!("fixtures/async_owner_views.jk"),
    );
    work.assert_all_backends("async_owner_views.jk", &[], b"", 0);
}

#[test]
fn async_owner_tcp_buffers_execute_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "async_owner_tcp.jk",
        include_str!("fixtures/async_owner_tcp.jk"),
    );
    work.assert_all_backends("async_owner_tcp.jk", &[], b"", 0);
}

#[test]
fn async_owner_views_drop_in_order_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("fixtures/async_owner_drop.jk").replace(
        "extern fn record_drop(id: Int32)",
        "fn record_drop(id: Int32) { println(id) }",
    );
    work.write("async_owner_drop.jk", &source);
    work.assert_all_backends("async_owner_drop.jk", &[], b"2\n42\n1\n2\n3\n1\n", 0);
}

#[test]
fn net4a_buffered_tcp_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("buffered_tcp.jk", include_str!("fixtures/buffered_tcp.jk"));
    work.assert_all_backends("buffered_tcp.jk", &[], b"", 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net4a_buffered_tcp_reset_runs_in_all_backends() {
    mod endpoint {
        include!("support/buffered_tcp.rs");
    }
    let (port, peer) = endpoint::reset_endpoint(3);
    let work = Workspace::new();
    work.write(
        "buffered_tcp_reset.jk",
        &include_str!("fixtures/buffered_tcp_reset.jk").replace("PORT", &port.to_string()),
    );
    work.assert_all_backends("buffered_tcp_reset.jk", &[], b"", 0);
    peer.join().unwrap();
}

#[test]
fn net4b_buffered_tcp_frames_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "buffered_tcp_frames.jk",
        include_str!("fixtures/buffered_tcp_frames.jk"),
    );
    work.assert_all_backends("buffered_tcp_frames.jk", &[], b"", 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net4b_buffered_tcp_frame_resets_run_in_all_backends() {
    mod endpoint {
        include!("support/buffered_tcp.rs");
    }
    for method in ["read_exact(4)", "read_until(10, 4)", "read_line(4)"] {
        let (port, peer) = endpoint::reset_endpoint(3);
        let work = Workspace::new();
        let source = include_str!("fixtures/buffered_tcp_reset.jk")
            .replace("PORT", &port.to_string())
            .replace(".read(4)", &format!(".{method}"));
        work.write("frame_reset.jk", &source);
        work.assert_all_backends("frame_reset.jk", &[], b"", 0);
        peer.join().unwrap();
    }
}

#[test]
fn net4c_buffered_tcp_messages_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "buffered_tcp_messages.jk",
        include_str!("fixtures/buffered_tcp_messages.jk"),
    );
    work.assert_all_backends("buffered_tcp_messages.jk", &[], b"", 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net4c_buffered_tcp_messages_reset_runs_in_all_backends() {
    mod endpoint {
        include!("support/buffered_tcp.rs");
    }
    let (port, peer) = endpoint::reset_endpoint(3);
    let work = Workspace::new();
    let source = include_str!("fixtures/buffered_tcp_reset.jk")
        .replace("PORT", &port.to_string())
        .replace(".read(4)", ".read_message(4)");
    work.write("message_reset.jk", &source);
    work.assert_all_backends("message_reset.jk", &[], b"", 0);
    peer.join().unwrap();
}

#[test]
fn net5d_duplex_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("tcp_duplex.jk", include_str!("../examples/tcp_duplex.jk"));
    work.assert_all_backends("tcp_duplex.jk", &[], b"1\n2\n3\n", 0);
}

#[test]
fn net4d_buffered_tcp_echo_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "tcp_messages.jk",
        include_str!("../examples/tcp_messages.jk"),
    );
    work.assert_all_backends("tcp_messages.jk", &[], "hello\n你好\n\n".as_bytes(), 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net4d_buffered_tcp_native_peer_and_cancelled_prefix_match_all_backends() {
    message_native_peer(false);
}
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net5c_buffered_writer_native_peer_and_cancelled_prefix_match_all_backends() {
    message_native_peer(true);
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn message_native_peer(split: bool) {
    mod endpoint {
        include!("support/tcp_messages.rs");
    }
    for cancel in [false, true] {
        for backend in 0..3 {
            let mut peer = endpoint::peer();
            let work = Workspace::new();
            let source = include_str!("fixtures/buffered_tcp_pressure.jk")
                .replace("PORT", &peer.port.to_string())
                .replace("COUNT", "8")
                .replace("extern fn sent(count: Int32)", "fn sent(count: Int32) {}")
                .replace("fn main()", "fn send_all()")
                .replace("fn cancel()", "fn stop_prefix()");
            let source = if split {
                source
                    .replace("type BufferedTcp}", "type BufferedTcp, type TcpWriter}")
                    .replace("connection: &mut BufferedTcp", "connection: &mut TcpWriter")
                    .replace(
                        "var connection = BufferedTcp.new(stream)",
                        "var (reader, connection) = BufferedTcp.new(stream).split()",
                    )
            } else {
                source
            };
            let entry = if cancel { "stop_prefix" } else { "send_all" };
            work.write(
                "pressure.jk",
                &format!("{source}\nfn main() @suspends -> Int32 {{ {entry}() }}"),
            );
            peer.release();
            let output = if backend < 2 {
                let args = if backend == 0 {
                    vec!["run", "pressure.jk"]
                } else {
                    vec!["run", "pressure.jk", "--interpreter"]
                };
                work.cli(&args)
            } else {
                success(&work.cli(&["build", "pressure.jk"]));
                std::process::Command::new(work.path().join("pressure"))
                    .output()
                    .unwrap()
            };
            success(&output);
            assert!(output.stdout.is_empty() && output.stderr.is_empty());
            work.assert_no_build_files();
            assert!(peer.accepted.load(std::sync::atomic::Ordering::Acquire));
            let received = peer.finish();
            if cancel {
                assert_eq!(received.complete, 0);
                assert!(received.partial > 4 && received.partial < 65540);
            } else {
                assert_eq!(received.complete, 8);
                assert_eq!(received.partial, 0);
            }
        }
    }
}

#[test]
fn net5a_detached_owners_execute_in_all_backends() {
    let work = Workspace::new();
    for (name, source) in [
        (
            "owners.jk",
            include_str!("fixtures/net5_owner_destructure.jk"),
        ),
        ("tcp.jk", include_str!("fixtures/net5_owner_tcp.jk")),
    ] {
        work.write(name, source);
        work.assert_all_backends(name, &[], b"", 0);
    }
}

#[test]
fn net5b_split_tcp_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("tcp_split.jk", include_str!("fixtures/tcp_split.jk"));
    work.assert_all_backends("tcp_split.jk", &[], b"", 0);
}

#[test]
fn net5c_buffered_split_runs_in_all_backends() {
    mod matrix {
        include!("support/buffered_tcp_split.rs");
    }
    let work = Workspace::new();
    work.write(
        "buffered_split.jk",
        include_str!("fixtures/buffered_tcp_split.jk"),
    );
    work.assert_all_backends("buffered_split.jk", &[], b"", 0);
    work.write("split_frames.jk", &matrix::frames_source());
    work.assert_all_backends("split_frames.jk", &[], b"", 0);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        mod endpoint {
            include!("support/buffered_tcp.rs");
        }
        for method in [
            "read(4)",
            "read_exact(4)",
            "read_line(4)",
            "read_message(4)",
        ] {
            let (port, peer) = endpoint::reset_endpoint(3);
            work.write("split_reset.jk", &matrix::reset_source(port, method));
            work.assert_all_backends("split_reset.jk", &[], b"", 0);
            peer.join().unwrap();
        }
    }
}

#[test]
#[cfg(unix)]
fn json0_existing_primitives_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "primitives.jk",
        include_str!("fixtures/json0_primitives.jk"),
    );
    work.assert_all_backends("primitives.jk", &[], b"42\n", 0);
}

#[test]
#[cfg(unix)]
fn json1a_recursive_owners_run_in_all_backends() {
    let work = Workspace::new();
    work.write("recursive.jk", include_str!("fixtures/json1a_recursive.jk"));
    work.assert_all_backends("recursive.jk", &[], b"1\n2\n", 0);
    let source = include_str!("fixtures/json1a_ownership.jk").replace(
        "extern fn record_drop(value: Int32)",
        "fn record_drop(value: Int32) { println(value) }",
    );
    work.write(
        "transfers.jk",
        &format!("{source}\nfn main() {{ probe(); }}"),
    );
    work.assert_all_backends(
        "transfers.jk",
        &[],
        b"-1\n-2\n-3\n-4\n3\n2\n-5\n5\n-100\n4\n1\n",
        0,
    );
}

#[test]
fn json1a_inline_recursion_remains_rejected_in_all_backends() {
    let work = Workspace::new();
    for source in [
        "enum Value { Next(Value) }",
        "import joky/option.{type Option} enum Value { Next(Option<Value>) }",
        "type Next = Value enum Value { More((Int32, Next)) }",
        "struct First { next: Second } struct Second { next: First }",
    ] {
        work.write("recursive.jk", &format!("{source}\nfn main() {{}}"));
        for args in [
            vec!["run", "recursive.jk"],
            vec!["run", "recursive.jk", "--interpreter"],
            vec!["build", "recursive.jk"],
        ] {
            failure(&work.cli(&args), "recursive_");
        }
        assert!(!work.path().join("recursive").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn json1a_expanding_types_and_live_owner_loans_remain_rejected() {
    let work = Workspace::new();
    for (source, code) in [
        ("import joky/array.{type Array} enum Bad<T> { Items(Array<Bad<(T,)>>) } fn main() {}", "resource_limit"),
        ("import joky/array.{type Array} enum Value { Nil, Items(Array<Value>) } fn main() { var values = Array.new::<Value>() values.push(Value.Nil) let view = values.borrow_ref(0) values.push(Value.Nil) match view { Nil => {}, _ => {} } }", "invalid_assignment"),
        ("import joky/array.{type Array} import joky/mem enum Value { Nil, Items(Array<Value>) } fn main() { var value = Value.Items(Array.new::<Value>()) let view = &value let old = mem.replace(&mut value, Value.Nil) match view { Nil => {}, _ => {} } }", "invalid_assignment"),
        ("import joky/array.{type Array} enum Value { Nil, Items(Array<Value>) } fn main() { let values = Array.new::<Value>() values.get(0) }", "type_mismatch"),
        ("import joky/array.{type Array} struct Shared { reference: &Int32 } enum Value { Nil, Items(Array<Value>), Hidden(Array<Shared>) } fn main() {}", "type_mismatch"),
        ("struct First<T, U> { value: T } enum Bad { Pair((First<Int32, Bad>, First<Bad, Int32>)) } fn main() {}", "recursive_type"),
        ("import joky/array.{type Array} struct Outer { values: Array<Holder> } struct Holder { reference: &Int32 } fn bad(value: Outer) @suspends {} fn main() {}", "invalid_call"),
        ("import joky/array.{type Array} struct Borrowed { target: &Owner } struct Owner { values: Array<Int32> } fn bad(values: Array<Borrowed>) {} fn main() {}", "type_mismatch"),
    ] {
        work.write("invalid.jk", source);
        for args in [vec!["run", "invalid.jk"], vec!["run", "invalid.jk", "--interpreter"], vec!["build", "invalid.jk"]] {
            failure(&work.cli(&args), code);
        }
        assert!(!work.path().join("invalid").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn json1b_general_numeric_from_text_remains_outside_public_api() {
    let work = Workspace::new();
    for source in [
        "fn main() { let text = \"42\"; Int64.from_text(&text); }",
        "fn main() { let text = \"18446744073709551615\"; UInt64.from_text(&text); }",
        "fn main() { let text = \"1.25e2\"; Float64.from_text(&text); }",
    ] {
        work.write("number.jk", source);
        for args in [
            vec!["run", "number.jk"],
            vec!["run", "number.jk", "--interpreter"],
            vec!["build", "number.jk"],
        ] {
            failure(&work.cli(&args), "from_text");
        }
        assert!(!work.path().join("number").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn json1b_values_and_numbers_run_in_all_backends() {
    let work = Workspace::new();
    work.write("values.jk", include_str!("fixtures/json1b_values.jk"));
    work.assert_all_backends("values.jk", &[], b"", 0);
}

#[test]
fn json1b_number_invariants_and_unique_value_ownership_are_enforced() {
    let work = Workspace::new();
    for (source, diagnostic) in [
        ("import joky/json.{type Number} fn main() { let number = Number { text: \"NaN\" } }", "private"),
        ("import joky/json as json fn main() { let text = \"1\" json.parse_float64(&text) }", "unknown_name"),
        ("import joky/json.{type Number} fn main() { let number = Number.from_i64(1) number.text }", "private"),
        ("import joky/json.{type Value} import joky/array.{type Array} fn main() { let values = Array.new::<Value>() values.get(0) }", "type_mismatch"),
    ] {
        work.write("invalid.jk", source);
        for args in [vec!["run", "invalid.jk"], vec!["run", "invalid.jk", "--interpreter"], vec!["build", "invalid.jk"]] {
            failure(&work.cli(&args), diagnostic);
        }
        assert!(!work.path().join("invalid").exists());
    }
    work.assert_no_build_files();
}

#[test]
fn json2a_strict_decoding_and_limits_run_in_all_backends() {
    let work = Workspace::new();
    work.write("decode.jk", include_str!("fixtures/json2a_decode.jk"));
    work.assert_all_backends("decode.jk", &[], b"", 0);
}

#[test]
fn json2b_roundtrips_match_independent_semantic_models_in_all_backends() {
    let work = Workspace::new();
    work.write("models.jk", include_str!("fixtures/json2a_model.jk"));
    work.assert_all_backends("models.jk", &[], b"", 0);
}

#[test]
fn json2b_compact_encoding_limits_and_borrowed_inputs_run_in_all_backends() {
    let work = Workspace::new();
    work.write("encode.jk", include_str!("fixtures/json2b_encode.jk"));
    work.assert_all_backends("encode.jk", &[], b"", 0);
}

#[test]
fn json3_loopback_example_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("json_tcp.jk", include_str!("../examples/tcp_json.jk"));
    work.assert_all_backends("json_tcp.jk", &[], b"push\nerror\nreply\nerror\nreply\n", 0);
}

#[test]
fn json3_native_peer_fragmentation_errors_and_terminal_frames_run_in_all_backends() {
    mod endpoint {
        include!("support/json_tcp.rs");
    }
    for case in 0..=3 {
        for backend in 0..3 {
            let peer = endpoint::peer(case);
            let work = Workspace::new();
            let entry = if case == 3 {
                format!("pressure({})", peer.port)
            } else {
                format!("native({}, {})", peer.port, if case == 0 { 2 } else { -2 })
            };
            let source = endpoint::source().replace("extern fn sent()", "fn sent() {}");
            work.write(
                "session.jk",
                &format!("{source}\nfn main() @suspends -> Int32 {{ ensure({entry} == 42) 0 }}"),
            );
            // The VM test holds this gate until writes really stall; CLI checks wire parity.
            let mut peer = peer;
            peer.release();
            let output = match backend {
                0 => work.cli(&["run", "session.jk"]),
                1 => work.cli(&["run", "session.jk", "--interpreter"]),
                _ => {
                    success(&work.cli(&["build", "session.jk"]));
                    std::process::Command::new(work.path().join("session"))
                        .output()
                        .unwrap()
                }
            };
            success(&output);
            assert!(
                output.stdout.is_empty() && output.stderr.is_empty(),
                "{output:?}"
            );
            assert_eq!(peer.finish(), if case == 3 { 512 } else { 0 });
            work.assert_no_build_files();
        }
    }
}

#[test]
fn json_typed_structs_and_example_run_in_all_backends() {
    let work = Workspace::new();
    work.write("typed.jk", include_str!("fixtures/json_typed.jk"));
    work.assert_all_backends("typed.jk", &[], b"", 0);
    work.write("struct.jk", include_str!("../examples/json_struct.jk"));
    work.assert_all_backends("struct.jk", &[], b"Ada\n37\n/age\n", 0);
}

#[test]
fn json_typed_scalars_containers_and_limits_run_in_all_backends() {
    // Separate type families keep failures focused. Composed roundtrips have their own test.
    let work = Workspace::new();
    for (file, source) in [
        ("numbers.jk", include_str!("fixtures/json_typed_numbers.jk")),
        (
            "unsigned.jk",
            include_str!("fixtures/json_typed_unsigned.jk"),
        ),
        ("floats.jk", include_str!("fixtures/json_typed_floats.jk")),
        (
            "containers.jk",
            include_str!("fixtures/json_typed_containers.jk"),
        ),
        ("errors.jk", include_str!("fixtures/json_typed_errors.jk")),
        (
            "map_encode.jk",
            include_str!("fixtures/json_typed_map_encode.jk"),
        ),
        (
            "dispatch.jk",
            include_str!("fixtures/json_typed_dispatch.jk"),
        ),
        ("limits.jk", include_str!("fixtures/json_typed_limits.jk")),
        ("cancel.jk", include_str!("fixtures/json_typed_cancel.jk")),
    ] {
        work.write(file, source);
        work.assert_all_backends(file, &[], b"", 0);
    }
}

#[test]
fn json_typed_custom_keys_reject_collisions_in_all_backends() {
    let work = Workspace::new();
    work.write("keys.jk", include_str!("fixtures/json_typed_keys.jk"));
    work.assert_all_backends("keys.jk", &[], b"", 0);
}

#[test]
fn json_typed_nested_map_roundtrip_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("map.jk", include_str!("fixtures/json_typed_map.jk"));
    work.assert_all_backends("map.jk", &[], b"", 0);
}

#[test]
fn json_derive_composition_and_example_run_in_all_backends() {
    let work = Workspace::new();
    work.write("derive.jk", include_str!("fixtures/json_derive.jk"));
    work.assert_all_backends("derive.jk", &[], b"", 0);
    work.write(
        "derive_example.jk",
        include_str!("../examples/json_derive.jk"),
    );
    work.assert_all_backends("derive_example.jk", &[], b"Ada\n37\n/age\n", 0);
    work.write(
        "derive_lifecycle.jk",
        include_str!("fixtures/json_derive_lifecycle.jk"),
    );
    work.assert_all_backends("derive_lifecycle.jk", &[], b"", 0);
    work.write(
        "derive_cancel.jk",
        include_str!("fixtures/json_derive_cancel.jk"),
    );
    work.assert_all_backends("derive_cancel.jk", &[], b"", 0);
}

#[test]
fn json_derive_dependencies_keep_private_fields_and_generic_definition_scope() {
    let work = Workspace::new();
    work.write(
        "model.jk",
        r#"
        #derive(ToJson, FromJson)
        pub struct Packet<T> { data: T, label: String }
        pub fn make() -> Packet<String> { Packet { data: "payload", label: "private" } }
        pub fn valid(value: &Packet<String>) -> Bool {
            let expected = "payload"
            let label = "private"
            value.data.equals(&expected) && value.label.equals(&label)
        }
    "#,
    );
    work.write(
        "entry.jk",
        r#"
        import model.{type Packet as Wire}
        import joky/json/typed as codec
        import joky/check.{require}
        trait ToJson { fn unrelated(&self) }
        struct Context {}
        fn main() {
            let value = model.make()
            let message = "derived dependency"
            match codec.encode(&value) {
                Ok(text) => match codec.decode::<Wire<String>>(&text) {
                    Ok(result) => { require(model.valid(&result), &message) }
                    Err(_) => { require(false, &message) }
                }
                Err(_) => { require(false, &message) }
            }
        }
    "#,
    );
    work.assert_all_backends("entry.jk", &[], b"", 0);
    work.write(
        "private.jk",
        "import model fn main() { let value = model.make() value.data; }",
    );
    for args in [
        vec!["run", "private.jk"],
        vec!["run", "--interpreter", "private.jk"],
        vec!["build", "private.jk"],
    ] {
        failure(&work.cli(&args), "private");
    }
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
        for args in [
            vec!["run", "bad.jk"],
            vec!["run", "--interpreter", "bad.jk"],
            vec!["build", "bad.jk", "-o", "bad"],
        ] {
            let output = work.cli(&args);
            assert!(!output.status.success(), "{source}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(message),
                "{source}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    for body in ["Empty::<Int32>", "Item(1)"] {
        work.write(
            "model.jk",
            &format!("{declarations} pub fn bad() {{ {body} }}"),
        );
        work.write("bad.jk", "import model fn main() { model.bad() }");
        for args in [
            vec!["run", "bad.jk"],
            vec!["run", "--interpreter", "bad.jk"],
            vec!["build", "bad.jk", "-o", "bad"],
        ] {
            let output = work.cli(&args);
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Ambiguous enum constructor"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn net6_dns_public_module_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("dns.jk", include_str!("fixtures/dns.jk"));
    work.assert_all_backends("dns.jk", &[], b"", 0);
    work.write("dns_example.jk", include_str!("../examples/dns_lookup.jk"));
    work.assert_all_backends("dns_example.jk", &[], b"localhost resolved\n", 0);
}

#[cfg(unix)]
#[test]
fn state_domains_require_supervised_requests_in_standalone_programs() {
    let work = Workspace::new();
    work.write(
        "domain.jk",
        r#"
        import joky/domain
        struct State { count: Int32 }
        fn read(state: &State) -> Int32 { state.count }
        fn main() @suspends { domain.read(read); }
    "#,
    );
    let check = |output: std::process::Output| {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("No active domain request"), "{error}");
        assert!(!error.contains("panicked"), "{error}");
    };
    check(work.cli(&["run", "domain.jk"]));
    check(work.cli(&["run", "domain.jk", "--interpreter"]));
    success(&work.cli(&["build", "domain.jk"]));
    check(Command::new(work.path().join("domain")).output().unwrap());
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn state_domains_source_lifecycle_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "domain_source.jk",
        include_str!("fixtures/state_domain_source.jk"),
    );
    work.assert_all_backends("domain_source.jk", &[], b"source domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_scalar_bits_and_unit_inputs_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "scalars.jk",
        include_str!("fixtures/state_domain_scalars.jk"),
    );
    work.assert_all_backends("scalars.jk", &[], b"scalar domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_sc1_acceptance_runs_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "acceptance.jk",
        include_str!("fixtures/state_domain_acceptance.jk"),
    );
    work.assert_all_backends("acceptance.jk", &[], b"SC1 acceptance ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_versioned_snapshots_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "snapshots.jk",
        include_str!("fixtures/state_domain_snapshots.jk"),
    );
    work.assert_all_backends("snapshots.jk", &[], b"snapshot domains ok\n", 0);
}

#[test]
fn state_domains_snapshots_reject_forgery_and_unchecked_projections() {
    let work = Workspace::new();
    let prefix = r#"
        import joky/domain
        struct State { count: Int32 }
        fn make(value: Int32) -> State { State { count: value } }
        fn read(state: &State) -> Int32 { state.count }
    "#;
    for case in [
        "fn main() @suspends { let owner=domain.create(make,1) let snapshot=domain.snapshot_from(owner.read_handle(),read) let raw=snapshot.storage }",
        "fn main() @suspends { let owner=domain.create(make,1) let snapshot=domain.snapshot_from(owner.read_handle(),read) let moved=snapshot snapshot.value(); moved.value(); }",
        "fn project(state: &State) -> String { \"owned\" } fn main() @suspends { let owner=domain.create(make,1) domain.snapshot_from(owner.read_handle(),project); }",
        "fn project(state: &mut State) -> Int32 { state.count=99 state.count } fn main() @suspends { let owner=domain.create(make,1) domain.snapshot_from(owner.read_handle(),project); }",
        "fn main() @suspends { let owner=domain.create(make,1) let operation=read domain.snapshot_from(owner.read_handle(),operation); }",
        "fn main() @suspends { let factory=domain.snapshot_from::<State,Int32> }",
    ] {
        work.write("rejected.jk", &format!("{prefix}\n{case}"));
        for args in [vec!["run", "rejected.jk"], vec!["run", "--interpreter", "rejected.jk"], vec!["build", "rejected.jk", "-o", "rejected"]] {
            let output = work.cli(&args);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "accepted {case}");
            assert!(!error.contains("panicked"), "{error}");
            assert!(!error.contains("[parse"), "{error}");
        }
    }
}

#[test]
fn state_domains_sc1_rejects_unproved_and_generic_owned_calls_in_all_frontends() {
    let work = Workspace::new();
    let prefix = r#"
        import joky/domain
        import joky/io
        struct State { count: Int32 }
        fn make(value: Int32) -> State { State { count: value } }
        fn identity(value: Int32) -> Int32 { value }
        fn echo<T>(value: T) @suspends -> T { value }
        fn relay<T>(handle: domain.WriteHandle<State>, value: T) @suspends -> Result<T, domain.RequestError> {
            domain.request::<State, T, T>(handle, echo::<T>, value)
        }
    "#;
    for (case, diagnostic) in [
        ("fn bad(s: &State) -> Int32 { let f = identity f(s.count) } fn main() @suspends { let o = domain.create(make, 1) domain.read_from(o.read_handle(), bad); }", "Domain operation must be a proved"),
        ("fn helper(n: Int32) { io.println(&n) } fn bad(s: &State) -> Int32 { helper(s.count) s.count } fn main() @suspends { let o = domain.create(make, 1) domain.read_from(o.read_handle(), bad); }", "Domain operation must be a proved"),
        ("fn main() @suspends { let o = domain.create(make, 1) relay(o.handle(), \"owned\"); }", "Copy type arguments"),
        ("fn main() @suspends { let o = domain.create(make, 1) relay(o.handle(), (1, 2)); }", "scalar"),
    ] {
        work.write("rejected.jk", &format!("{prefix}\n{case}"));
        for args in [
            vec!["run", "rejected.jk"],
            vec!["run", "--interpreter", "rejected.jk"],
            vec!["build", "rejected.jk", "-o", "rejected"],
        ] {
            let output = work.cli(&args);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "accepted {case}");
            assert!(error.contains(diagnostic), "{args:?}: {error}");
            assert!(!error.contains("panicked"), "{error}");
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_imported_entries_and_language_example_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "room.jk",
        r#"
        import joky/domain
        import joky/task
        pub struct State { count: Int32 }
        pub fn make(count: Int32) -> State { State { count: count } }
        fn add(state: &mut State, delta: Int32) -> Int32 {
            state.count = state.count + delta
            state.count
        }
        pub fn read(state: &State) -> Int32 { state.count }
        pub fn work(delta: Int32) @suspends -> Int32 {
            task.sleep_ms(1)
            domain.write(add, delta)
        }
        "#,
    );
    work.write(
        "imported.jk",
        r#"
        import room
        import joky/domain
        import joky/io
        fn main() @suspends {
            let owner = domain.create(room.make, 10)
            let result = domain.request(owner.handle(), room.work, 2).unwrap_or(-1)
            io.println(result)
            let value = domain.read_from(owner.read_handle(), room.read)
            io.println(value)
            owner.close()
        }
        "#,
    );
    work.assert_all_backends("imported.jk", &[], b"12\n12\n", 0);
    let reference = include_str!("../docs/language.md");
    let section = reference
        .split("## 内置域访问 `joky/domain`")
        .nth(1)
        .unwrap();
    let example = section
        .split("```jk\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    work.write("documented.jk", example);
    work.assert_all_backends("documented.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_candidates_commit_atomically_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "commits.jk",
        include_str!("fixtures/state_domain_commits.jk"),
    );
    work.assert_all_backends("commits.jk", &[], b"domain commits ok\n", 0);
}

#[test]
fn state_domains_candidates_reject_forgery_reuse_and_unproved_constructors() {
    let work = Workspace::new();
    let prefix = r#"
        import joky/domain
        struct State { count: Int32 }
        struct Other { count: Int32 }
        fn make(value: Int32) -> State { State { count: value } }
        fn read(state: &State) -> Int32 { state.count }
    "#;
    let start = "let owner = domain.create(make, 1) let handle = owner.handle() let snapshot = domain.snapshot_from(handle.read_only(), read)";
    for (extra, body) in [
        ("", "let candidate = domain.candidate_from(snapshot, make, 2) snapshot.value();"),
        ("", "let candidate = domain.candidate_from(snapshot, make, 2) domain.commit_to(handle, candidate); domain.commit_to(handle, candidate);"),
        ("", "let candidate = domain.candidate_from(snapshot, make, 2) let storage = candidate.storage"),
        ("", "let candidate = domain.candidate_from(snapshot, make, 2) domain.commit_to(handle.read_only(), candidate);"),
        ("", "let constructor = make domain.candidate_from(snapshot, constructor, 2);"),
        ("fn other(value: Int32) -> Other { Other { count: value } }", "domain.candidate_from(snapshot, other, 2);"),
        ("fn owns(value: String) -> State { State { count: 1 } }", "domain.candidate_from(snapshot, owns, \"owned\");"),
        ("fn noisy(value: Int32) -> State { println(value) make(value) }", "domain.candidate_from(snapshot, noisy, 2);"),
        ("fn pending(value: Int32) @suspends -> State { make(value) }", "domain.candidate_from(snapshot, pending, 2);"),
        ("", "let factory = domain.candidate_from::<State, Int32, Int32>"),
        ("", "let candidate = domain.candidate_from(snapshot, make, 2) let receipt = domain.commit_to(handle, candidate) let storage = receipt.storage"),
    ] {
        work.write("rejected.jk", &format!("{prefix}\n{extra}\nfn main() @suspends {{ {start} {body} }}"));
        for args in [vec!["run", "rejected.jk"], vec!["run", "--interpreter", "rejected.jk"], vec!["build", "rejected.jk", "-o", "rejected"]] {
            let output = work.cli(&args);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "accepted {body}");
            assert!(!error.contains("panicked"), "{error}");
            assert!(!error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_owned_graphs_run_in_all_backends() {
    let work = Workspace::new();
    work.write("owned.jk", include_str!("fixtures/state_domain_owned.jk"));
    work.assert_all_backends("owned.jk", &[], b"owned domains ok\n", 0);
}

#[test]
fn state_domains_owned_roots_reject_hooks_and_unsafe_graphs() {
    let work = Workspace::new();
    for definitions in [
        "import joky/drop.{type Drop} struct State { title: String } impl Drop for State { fn drop(&mut self) {} } fn make(n: Int32) -> State { State { title: \"owned\" } }",
        "import joky/drop.{type Drop} struct Temp { title: String } impl Drop for Temp { fn drop(&mut self) {} } struct State { title: String } fn make(n: Int32) -> State { let temporary = Temp { title: \"temp\" } State { title: \"owned\" } }",
        "import joky/array.{type Array} import joky/drop.{type Drop} struct Item { name: String } impl Drop for Item { fn drop(&mut self) {} } struct State { items: Array<Item> } fn make(n: Int32) -> State { State { items: Array.new::<Item>() } }",
        "import joky/map.{type Map} import joky/drop.{type Drop} struct Item { name: String } impl Drop for Item { fn drop(&mut self) {} } struct State { items: Map<Int32,Item> } fn make(n: Int32) -> State { State { items: Map.new::<Int32,Item>() } }",
        "import joky/array.{type Array} struct Item { pointer: Ptr } struct State { items: Array<Item> } fn make(n: Int32) -> State { State { items: Array.new::<Item>() } }",
        "import joky/array.{type Array} struct Node { children: Array<Node> } struct State { root: Node } fn make(n: Int32) -> State { State { root: Node { children: Array.new::<Node>() } } }",
    ] {
        work.write("rejected.jk", &format!("import joky/domain\n{definitions}\nfn main() @suspends {{ let owner = domain.create(make, 7) }}"));
        for args in [vec!["run", "rejected.jk"], vec!["run", "--interpreter", "rejected.jk"], vec!["build", "rejected.jk", "-o", "rejected"]] {
            let output = work.cli(&args);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "accepted {definitions}");
            assert!(!error.contains("panicked"), "{error}");
            assert!(!error.contains("[parse"), "{error}");
            assert!(error.contains("proved synchronous"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_deadlines_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "deadlines.jk",
        include_str!("fixtures/state_domain_deadlines.jk"),
    );
    work.assert_all_backends("deadlines.jk", &[], b"domain deadlines ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_deadlines_reject_negative_durations_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "negative.jk",
        "import joky/domain fn main() @suspends { let deadline = domain.deadline_after(-1) }",
    );
    let rejected = |output: std::process::Output| {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("Commit deadline duration must be nonnegative"));
    };
    rejected(work.cli(&["run", "negative.jk"]));
    rejected(work.cli(&["run", "--interpreter", "negative.jk"]));
    success(&work.cli(&["build", "negative.jk"]));
    rejected(Command::new(work.path().join("negative")).output().unwrap());
}

#[test]
fn state_domains_deadlines_reject_forgery_in_all_frontends() {
    let work = Workspace::new();
    for source in [
        "import joky/domain fn main() @suspends { let d = domain.CommitDeadline { storage: 1 } }",
        "extern fn __domain_deadline_after(n: UInt64) @suspends -> domain.DeadlineOwner fn main() @suspends { let d = __domain_deadline_after(1) }",
        "import joky/domain struct State { n: Int32 } fn make(n: Int32) -> State { State { n:n } } fn read(s: &State) -> Int32 { s.n } fn main() @suspends { let o=domain.create(make,1) let c=domain.candidate_from(domain.snapshot_from(o.read_handle(),read),make,2) let d=0 let r=domain.commit_before(o.handle(),c,&d) }",
    ] {
        work.write("forged.jk", source);
        for args in [vec!["run", "forged.jk"], vec!["run", "--interpreter", "forged.jk"], vec!["build", "forged.jk"]] {
            let output = work.cli(&args);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{source}");
            assert!(!error.contains("panicked"), "{error}");
            assert!(!error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_sc2_acceptance_and_scalar_arrays_run_in_all_backends() {
    let work = Workspace::new();
    for (source, expected) in [
        (
            include_str!("fixtures/state_domain_sc2_acceptance.jk"),
            b"SC2 acceptance ok\n".as_slice(),
        ),
        (
            include_str!("fixtures/state_domain_sc2_arrays.jk"),
            b"SC2 scalar Arrays ok\n".as_slice(),
        ),
    ] {
        work.write("acceptance.jk", source);
        work.assert_all_backends("acceptance.jk", &[], expected, 0);
    }
}

#[test]
#[cfg(unix)]
fn state_domains_pairs_run_in_all_backends() {
    let work = Workspace::new();
    work.write("pairs.jk", include_str!("fixtures/state_domain_pairs.jk"));
    work.assert_all_backends("pairs.jk", &[], b"domain pairs ok\n", 0);
}

#[test]
fn state_domains_pairs_reject_unproved_entries_and_capability_mismatches() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S { n:Int32 } fn make(n:Int32)->S { S { n:n } } fn read(a:&S,b:&S)->Int32 { a.n+b.n } fn write(a:&mut S,b:&mut S,n:Int32)->Int32 { a.n=n b.n=n n }";
    for (extra, body) in [
        ("", "domain.write_pair(a.read_handle(),b.handle(),write,1);"),
        ("", "domain.read_pair(a.read_handle(),b.read_handle(),write);"),
        ("", "let f=read domain.read_pair(a.read_handle(),b.read_handle(),f);"),
        ("", "let f=domain.read_pair::<S,S,Int32>"),
        ("fn noisy(a:&S,b:&S)->Int32 { println(a.n) b.n }", "domain.read_pair(a.read_handle(),b.read_handle(),noisy);"),
        ("fn pending(a:&S,b:&S) @suspends -> Int32 { a.n+b.n }", "domain.read_pair(a.read_handle(),b.read_handle(),pending);"),
        ("fn escape(a:&S,b:&S)->&S { a }", "domain.read_pair(a.read_handle(),b.read_handle(),escape);"),
        ("struct Owned { s:String } fn owned(n:Int32)->Owned { Owned { s:\"text\" } } fn owning(a:&Owned,b:&Owned)->Int32 { 1 }", "let x=domain.create(owned,1) domain.read_pair(x.read_handle(),x.read_handle(),owning);"),
        ("import joky/drop.{type Drop} struct Hook { n:Int32 } impl Drop for Hook { fn drop(&mut self) {} } fn hook(a:&S,b:&S)->Int32 { let x=Hook{n:1} a.n+b.n }", "domain.read_pair(a.read_handle(),b.read_handle(),hook);"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1) let b=domain.create(make,2) {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_pair_aliases_fail_before_execution_in_all_backends() {
    let work = Workspace::new();
    for (operation, call) in [
        (
            "fn mutate(a:&mut S,b:&mut S,n:Int32)->Int32 { a.n=1 b.n=2 n }",
            "domain.write_pair(a.handle(),a.handle(),mutate,0)",
        ),
        (
            "fn mutate(a:&S,b:&mut S,n:Int32)->Int32 { b.n=a.n+n n }",
            "domain.update_pair(a.read_handle(),a.handle(),mutate,0)",
        ),
    ] {
        work.write("alias.jk", &format!("import joky/domain struct S {{ n:Int32 }} fn make(n:Int32)->S {{ S{{n:n}} }} {operation} fn main() @suspends {{ let a=domain.create(make,3) {call}; }}"));
        let rejected = |output: std::process::Output| {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("cannot alias a writable state reference"));
        };
        rejected(work.cli(&["run", "alias.jk"]));
        rejected(work.cli(&["run", "--interpreter", "alias.jk"]));
        success(&work.cli(&["build", "alias.jk"]));
        rejected(Command::new(work.path().join("alias")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn state_domains_joint_snapshots_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "joint.jk",
        include_str!("fixtures/state_domain_joint_snapshots.jk"),
    );
    work.assert_all_backends("joint.jk", &[], b"joint snapshots ok\n", 0);
}

#[test]
fn state_domains_joint_snapshots_reject_dependency_extraction_and_invalid_projections() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S { n:Int32 } fn make(n:Int32)->S { S{n:n} } fn project(a:&S,b:&S)->Int32 { a.n+b.n }";
    for (extra, body) in [
        ("", "let f=project let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),f)"),
        ("", "let f=domain.snapshot_pair::<S,S,Int32>"),
        ("fn mutating(a:&S,b:&mut S)->Int32 { b.n=3 a.n }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),mutating)"),
        ("fn owning(a:&S,b:&S)->String { \"value\" }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),owning)"),
        ("fn noisy(a:&S,b:&S)->Int32 { println(a.n) b.n }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),noisy)"),
        ("fn pending(a:&S,b:&S) @suspends ->Int32 { a.n+b.n }", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),pending)"),
        ("", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),project) let raw=s.storage"),
        ("", "let s=domain.snapshot_pair(a.read_handle(),b.read_handle(),project) let c=domain.candidate_from(s,make,2) s.value();"),
        ("", "let s=domain.Snapshot::<S,Int32>{storage:1}"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1) let b=domain.create(make,2) {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_pair_commits_run_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "pair_commits.jk",
        include_str!("fixtures/state_domain_pair_commits.jk"),
    );
    work.assert_all_backends("pair_commits.jk", &[], b"pair commits ok\n", 0);
}
#[test]
fn state_domains_pair_commits_reject_forgery_reuse_and_unproved_constructors() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S{n:Int32} fn make(n:Int32)->S{S{n:n}} fn total(a:&S,b:&S)->Int32{a.n+b.n}";
    for (extra, body) in [
        ("", "let p=domain.CandidatePair::<S,S>{storage:1}"),
        ("", "let f=domain.candidate_pair_from::<S,S,Int32>"),
        ("", "let f=make let p=domain.candidate_pair_from(c,f,2)"),
        ("fn noisy(n:Int32)->S { println(n) make(n) }", "let p=domain.candidate_pair_from(c,noisy,2)"),
        ("fn pending(n:Int32) @suspends ->S { make(n) }", "let p=domain.candidate_pair_from(c,pending,2)"),
        ("", "let p=domain.candidate_pair_from(c,make,2) let p2=domain.candidate_pair_from(c,make,3)"),
        ("", "let p=domain.candidate_pair_from(c,make,2) let r=domain.commit_pair_to(a.read_handle(),b.handle(),p)"),
        ("", "let p=domain.candidate_pair_from(c,make,2) let r=domain.commit_pair_to(a.handle(),b.handle(),p) let r2=domain.commit_pair_to(a.handle(),b.handle(),p)"),
        ("", "let p=domain.candidate_pair_from(c,make,2) let raw=p.storage"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1) let b=domain.create(make,2) let c=domain.candidate_from(domain.snapshot_pair(a.read_handle(),b.read_handle(),total),make,3) {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
}
#[test]
#[cfg(unix)]
fn state_domains_pair_commits_reject_single_or_duplicate_snapshot_dependencies() {
    let work = Workspace::new();
    for snapshot in [
        "domain.snapshot_from(a.read_handle(),read)",
        "domain.snapshot_pair(a.read_handle(),a.read_handle(),total)",
    ] {
        work.write("invalid.jk", &format!("import joky/domain struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn read(a:&S)->Int32{{a.n}} fn total(a:&S,b:&S)->Int32{{a.n+b.n}} fn main() @suspends {{ let a=domain.create(make,1) let c=domain.candidate_from({snapshot},make,2) let p=domain.candidate_pair_from(c,make,3) }}"));
        let rejected = |output: std::process::Output| {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("requires a joint snapshot of two distinct domains"));
        };
        rejected(work.cli(&["run", "invalid.jk"]));
        rejected(work.cli(&["run", "--interpreter", "invalid.jk"]));
        success(&work.cli(&["build", "invalid.jk"]));
        rejected(Command::new(work.path().join("invalid")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn state_domains_many_transfer_and_readonly_fourth_conflict_run_in_all_backends() {
    let work = Workspace::new();
    work.write("many.jk", include_str!("fixtures/state_domain_many.jk"));
    work.assert_all_backends("many.jk", &[], b"multi-domain transfer ok\n", 0);
}

#[test]
fn state_domains_many_reject_forgery_reuse_and_unproved_entries() {
    let work = Workspace::new();
    let prefix = "import joky/domain struct S{n:Int32} fn make(n:Int32)->S{S{n:n}} fn read(s:&S)->Int32{s.n}";
    for (extra, body) in [
        ("", "let raw=s.storage"),
        ("", "let x=domain.SnapshotSet{storage:1}"),
        ("", "let x=domain.CandidateSet{storage:1}"),
        ("", "let f=domain.read_snapshot::<S,Int32>"),
        ("", "let f=read domain.read_snapshot(&s,a.read_handle(),f);"),
        ("fn noisy(s:&S)->Int32{println(s.n) s.n}", "domain.read_snapshot(&s,a.read_handle(),noisy);"),
        ("fn mutating(s:&mut S)->Int32{s.n=3 s.n}", "domain.read_snapshot(&s,a.read_handle(),mutating);"),
        ("fn pending(s:&S) @suspends ->Int32{s.n}", "domain.read_snapshot(&s,a.read_handle(),pending);"),
        ("", "let c=domain.candidate_set_from(s,a.read_handle(),make,3)"),
        ("", "let f=make let c=domain.candidate_set_from(s,a.handle(),f,3)"),
        ("fn noisy(n:Int32)->S{println(n) make(n)}", "let c=domain.candidate_set_from(s,a.handle(),noisy,3)"),
        ("", "let c=domain.candidate_set_from(s,a.handle(),make,3) s.check();"),
        ("", "let c=domain.candidate_set_from(s,a.handle(),make,3) let d=domain.candidate_set_add(c,b.handle(),make,4) let e=domain.commit_set(c)"),
        ("", "let c=domain.candidate_set_from(s,a.handle(),make,3) let r=domain.commit_set(c) let r2=domain.commit_set(c)"),
    ] {
        work.write("invalid.jk", &format!("{prefix} {extra} fn main() @suspends {{ let a=domain.create(make,1) let b=domain.create(make,2) var access=domain.access_set() access.include(a.read_handle()) access.include(b.read_handle()) let s=domain.snapshot_set(access) {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn state_domains_many_reject_missing_or_duplicate_members_in_all_backends() {
    let work = Workspace::new();
    for (body, diagnostic) in [
        ("let s=domain.snapshot_set(domain.access_set())", "Empty domain access set"),
        ("var access=domain.access_set() access.include(a.read_handle()) let s=domain.snapshot_set(access) domain.read_snapshot(&s,b.read_handle(),read);", "Projection domain is absent"),
        ("var access=domain.access_set() access.include(a.read_handle()) let s=domain.snapshot_set(access) let c=domain.candidate_set_from(s,b.handle(),make,3)", "Candidate target is absent"),
        ("var access=domain.access_set() access.include(a.read_handle()) let s=domain.snapshot_set(access) let c=domain.candidate_set_from(s,a.handle(),make,3) let d=domain.candidate_set_add(c,a.handle(),make,4)", "cannot repeat a writable domain"),
        ("let owned=domain.create(make_owned,1) var access=domain.access_set() access.include(owned.read_handle())", "Access sets require scalar record roots"),
    ] {
        work.write("invalid.jk", &format!("import joky/domain struct S{{n:Int32}} fn make(n:Int32)->S{{S{{n:n}}}} fn read(s:&S)->Int32{{s.n}} struct Owned{{s:String}} fn make_owned(n:Int32)->Owned{{Owned{{s:\"text\"}}}} fn main() @suspends {{ let a=domain.create(make,1) let b=domain.create(make,2) {body} }}"));
        let rejected = |output: std::process::Output| {
            assert!(!output.status.success(), "{body}");
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(diagnostic), "{error}");
        };
        rejected(work.cli(&["run", "invalid.jk"]));
        rejected(work.cli(&["run", "--interpreter", "invalid.jk"]));
        success(&work.cli(&["build", "invalid.jk"]));
        rejected(Command::new(work.path().join("invalid")).output().unwrap());
    }
}

#[test]
#[cfg(unix)]
fn admission_fifo_close_and_cancellation_run_in_all_backends() {
    let work = Workspace::new();
    work.write("admission.jk", include_str!("fixtures/admission.jk"));
    work.assert_all_backends("admission.jk", &[], b"admission ok\n", 0);
}

#[test]
fn admission_rejects_forgery_copy_and_hidden_storage() {
    let work = Workspace::new();
    for body in [
        "let pool=admission.Pool{storage:1}",
        "let permit=admission.Permit{storage:1}",
        "let handle=admission.Handle{token:1}",
        "let pool=admission.create(1,1) let raw=pool.storage",
        "let pool=admission.create(1,1) let h=pool.handle() let raw=h.token",
        "let pool=admission.create(1,1) let p=pool.handle().acquire().unwrap() p.release() p.release()",
        "let pool=admission.create(1,1) let another=pool pool.close()",
        "let pool=admission.create(1,1) let p=pool.handle().acquire().unwrap() let raw=p.storage",
    ] {
        work.write("invalid.jk",&format!("import joky/admission fn main() @suspends {{ {body} }}"));
        for args in [vec!["run","invalid.jk"],vec!["run","--interpreter","invalid.jk"],vec!["build","invalid.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(),"{body}");
            assert!(!error.contains("panicked") && !error.contains("[parse"),"{error}");
        }
    }
}
#[test]
#[cfg(unix)]
fn admission_invalid_capacity_and_queue_size_fail_in_all_backends() {
    let work = Workspace::new();
    for (capacity, waiters, message) in [
        ("0", "0", "capacity must be positive"),
        ("1", "18446744073709551615", "Admission queue size overflow"),
    ] {
        work.write("invalid.jk",&format!("import joky/admission fn main() @suspends {{ let pool=admission.create({capacity},{waiters}) }}"));
        let rejected = |output: std::process::Output| {
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(message), "{error}");
        };
        rejected(work.cli(&["run", "invalid.jk"]));
        rejected(work.cli(&["run", "--interpreter", "invalid.jk"]));
        success(&work.cli(&["build", "invalid.jk"]));
        rejected(Command::new(work.path().join("invalid")).output().unwrap());
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
        for args in [
            vec!["run", "invalid.jk"],
            vec!["run", "--interpreter", "invalid.jk"],
            vec!["build", "invalid.jk"],
        ] {
            failure(&work.cli(&args), message);
        }
    }
}

#[test]
#[cfg(unix)]
fn arithmetic_assignment_and_remainder_run_on_all_backends() {
    let work = Workspace::new();
    let fixture = include_str!("fixtures/arithmetic_assignment.jk");
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
            "\nfn main() @suspends { let result = compute(); println(&result) let zero = constant(); println(&zero) }"
        } else {
            "\nfn main() { let result = compute(); println(&result) let zero = constant(); println(&zero) }"
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
    let fixture = include_str!("fixtures/conditional_patterns.jk")
        .replace(
            "import joky/check.{require}",
            "import joky/check.{require}\nimport joky/io.{println}",
        )
        .replace(
            "extern fn record(x: Int32) -> Int32",
            "fn record(x: Int32) -> Int32 { println(&x); x }",
        );
    for suspends in [false, true] {
        let mut source = fixture.clone();
        if suspends {
            source.insert_str(0, "import joky/task as task\n");
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
    let source = format!("import joky/task as task\n{fixture}\nfn main() @suspends {{ if let Some(value) = Some(owned(9)) {{ task.yield_now(); let invalid = 1 % 0 }} record(999); }}");
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
    let fixture = include_str!("fixtures/tuple_members.jk");
    for suspends in [false, true] {
        let mut source = fixture.to_string();
        if suspends {
            source.insert_str(0, "import joky/task as task\n");
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
        for args in [
            vec!["run", "invalid_tuple_member.jk"],
            vec!["run", "--interpreter", "invalid_tuple_member.jk"],
            vec!["build", "invalid_tuple_member.jk"],
        ] {
            failure(&work.cli(&args), message);
        }
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn script_scalar_bridge_runs_in_all_host_and_script_backends() {
    for create in ["vm.create_interpreter()", "vm.create()"] {
        let work = Workspace::new();
        work.write(
            "script.jk",
            &include_str!("fixtures/aot_script_scalar.jk")
                .replace("vm.create_interpreter()", create),
        );
        work.assert_all_backends("script.jk", &[], b"AOT Script scalar probe ok\n", 0);
        std::fs::remove_file(work.path().join("script.jk")).unwrap();
        let standalone = Command::new(work.path().join("script"))
            .env("PATH", "")
            .env_remove("JOKY_RUNTIME")
            .output()
            .unwrap();
        success(&standalone);
        assert_eq!(standalone.stdout, b"AOT Script scalar probe ok\n");
        assert!(standalone.stderr.is_empty());
    }
}

#[test]
#[cfg(unix)]
fn state_domains_nested_graphs_run_in_all_backends() {
    let work = Workspace::new();
    work.write("nested.jk", include_str!("fixtures/state_domain_nested.jk"));
    work.assert_all_backends("nested.jk", &[], b"nested domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn state_domains_nested_string_keys_run_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("fixtures/state_domain_nested.jk")
        .replace("Map<Int32, Player>", "Map<String, Player>")
        .replace("Map.new::<Int32, Player>()", "Map.new::<String, Player>()")
        .replace("rooms.insert(1,", "rooms.insert(\"first\",")
        .replace("rooms.insert(2,", "rooms.insert(\"second\",")
        .replace(
            "fn edit(state: &mut State, value: Int32) -> Int32 {",
            "fn edit(state: &mut State, value: Int32) -> Int32 { let second = \"second\"",
        )
        .replace(
            "fn inspect(state: &State) -> Bool {",
            "fn inspect(state: &State) -> Bool { let first = \"first\" let second = \"second\"",
        )
        .replace("rooms.remove(2)", "rooms.remove_by_ref(&second)")
        .replace("rooms.borrow_ref(1)", "rooms.value_ref(&first)")
        .replace("rooms.borrow_ref(2)", "rooms.value_ref(&second)");
    work.write("nested.jk", &source);
    work.assert_all_backends("nested.jk", &[], b"nested domains ok\n", 0);
}

#[test]
#[cfg(unix)]
fn script_flat_bridge_runs_in_all_host_and_script_backends() {
    let work = Workspace::new();
    for source in [
        include_str!("fixtures/aot_script_flat.jk").to_owned(),
        include_str!("fixtures/aot_script_flat.jk")
            .replace("vm.create_interpreter()", "vm.create()"),
    ] {
        work.write("flat.jk", &source);
        work.assert_all_backends("flat.jk", &[], b"Script flat contracts ok\n", 0);
        std::fs::remove_file(work.path().join("flat.jk")).unwrap();
        let standalone = Command::new(work.path().join("flat"))
            .env("PATH", "")
            .env_remove("JOKY_RUNTIME")
            .output()
            .unwrap();
        success(&standalone);
        assert_eq!(standalone.stdout, b"Script flat contracts ok\n");
        assert!(standalone.stderr.is_empty());
    }
}

#[test]
#[cfg(unix)]
fn script_flat_bridge_preserves_reload_and_cancellation_in_all_backends() {
    let work = Workspace::new();
    let source = include_str!("fixtures/aot_script_scalar.jk")
        .replace("import joky/vm", "import joky/vm\nimport joky/script")
        .replace("vm.call_async_int32", "script.call::<Int32, Int32>");
    for source in [
        source.clone(),
        source.replace("vm.create_interpreter()", "vm.create()"),
    ] {
        work.write("reload.jk", &source);
        work.assert_all_backends("reload.jk", &[], b"AOT Script scalar probe ok\n", 0);
    }
}

#[test]
fn script_flat_bridge_rejects_unsupported_caller_contracts() {
    let work = Workspace::new();
    for (definitions, ty, value) in [
        ("", "String", "\"owned\""),
        (
            "struct Inner { n:Int32 } struct Input { nested:Inner }",
            "Input",
            "Input { nested:Inner { n:1 } }",
        ),
        ("struct Input { p:Ptr }", "Input", "Input { p:Ptr.null() }"),
    ] {
        work.write("rejected.jk", &format!("import joky/vm import joky/script {definitions} fn main() @suspends {{ let h=vm.create() script.call::<{ty}, Int32>(h, \"run\", {value}); }}"));
        for args in [
            vec!["run", "rejected.jk"],
            vec!["run", "--interpreter", "rejected.jk"],
            vec!["build", "rejected.jk", "-o", "rejected"],
        ] {
            let output = work.cli(&args);
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success());
            assert!(error.contains("Script contracts require"), "{error}");
        }
    }
}

#[test]
#[cfg(unix)]
fn script_flat_bridge_enforces_record_field_limit() {
    for count in [64, 65] {
        let fields = (0..count)
            .map(|i| format!("f{i}:Int32"))
            .collect::<Vec<_>>()
            .join(",");
        let values = (0..count)
            .map(|i| format!("f{i}:{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let definition = format!("struct Packet {{ {fields} }}");
        let target = format!("import joky/task {definition} fn run(p:Packet) @suspends ->Packet {{ task.yield_now() p }}")
            .replace('{', "{{").replace('}', "}}");
        for create in ["create", "create_interpreter"] {
            let work = Workspace::new();
            work.write(
                "limit.jk",
                &format!(
                    r#"import joky/vm import joky/script import joky/check
{definition}
fn main() @suspends {{
    let h=vm.{create}()
    let source={target:?}
    vm.load_source(h, &source)
    let p=script.call::<Packet, Packet>(h, "run", Packet {{ {values} }})
    let message="packet limit"
    check.require(p.f63 == 63, &message)
    vm.close(h);
}}
"#
                ),
            );
            if count == 64 {
                work.assert_all_backends("limit.jk", &[], b"", 0);
            } else {
                for args in [
                    vec!["run", "limit.jk"],
                    vec!["run", "--interpreter", "limit.jk"],
                    vec!["build", "limit.jk", "-o", "limit"],
                ] {
                    failure(&work.cli(&args), "Script contracts require");
                }
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn outbox_atomic_publication_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("outbox.jk", include_str!("fixtures/outbox.jk"));
    work.assert_all_backends("outbox.jk", &[], b"outbox ok\n", 0);
}

#[test]
#[cfg(unix)]
fn outbox_tcp_writer_runs_in_all_backends() {
    let work = Workspace::new();
    work.write("outbox.jk", include_str!("fixtures/outbox_tcp.jk"));
    work.assert_all_backends("outbox.jk", &[], b"outbox TCP ok\n", 0);
}

#[test]
#[cfg(unix)]
fn outbox_script_handles_survive_module_close_in_all_backends() {
    let target = r#"import joky/domain import joky/outbox import joky/array.{type Array}
struct State { n:Int32 }
fn make(n:Int32)->State { State{n:n} }
fn read(s:&State)->Int32{s.n}
fn send(target:outbox.Handle) @suspends ->Int32 {
    let state=domain.create(make,0)
    var bytes=Array.new::<UInt8>() bytes.push(255)
    var batch=outbox.batch(1,1)
    outbox.stage(&mut batch,target,bytes);
    let candidate=domain.with_outbox(domain.candidate_from(domain.snapshot_from(state.read_handle(),read),make,1),batch)
    var receipt=domain.commit_to(state.handle(),candidate)
    match receipt.wait() { Committed(_) => 1, _ => 0 }
}"#.replace('{', "{{").replace('}', "}}");
    for create in ["create", "create_interpreter"] {
        let work = Workspace::new();
        work.write("script.jk", &format!(r#"import joky/vm import joky/script import joky/outbox import joky/check
fn main() @suspends {{
    var queue=outbox.create(1,1)
    let h=vm.{create}()
    let source={target:?}
    let message="outbox Script"
    check.require(vm.load_source(h,&source)==0,&message)
    check.require(script.call::<outbox.Handle,Int32>(h,"send",queue.handle())==1,&message)
    let next="fn newer()->Int32 {{{{ 2 }}}}"
    check.require(vm.load_source(h,&next)==0,&message)
    check.require(vm.close(h)==0,&message)
    check.require(match queue.receive() {{ Some(bytes)=>bytes.len()==1 && bytes.get(0).unwrap_or(0)==255, None=>false }},&message)
    println("outbox Script ok")
}}
"#));
        work.assert_all_backends("script.jk", &[], b"outbox Script ok\n", 0);
    }
}

#[test]
fn directory_routes_run_in_all_backends() {
    let work = Workspace::new();
    work.write("directory.jk", include_str!("fixtures/directory.jk"));
    work.assert_all_backends("directory.jk", &[], b"directory ok\n", 0);
}

#[test]
#[cfg(unix)]
fn directory_script_routing_survives_code_publication_in_all_backends() {
    let source = r#"import joky/domain import joky/task import joky/check
struct Room { value:Int32 }
fn make(value:Int32)->Room { Room{value:value} }
fn read(room:&Room)->Int32 { room.value }
fn hold(directory:domain.DirectoryHandle) @suspends {
    let room=domain.create(make,42)
    let message="directory register"
    check.require(directory.insert(7,room.handle()).is_ok(),&message)
    while domain.lookup(directory,7,read).is_some() { task.yield_now() }
}
fn probe(directory:domain.DirectoryHandle) @suspends ->Int32 {
    match domain.lookup(directory,7,read) { Some(room)=>domain.read_from(room,read),None=>-1 }
}
fn remove(directory:domain.DirectoryHandle) @suspends ->Bool {
    match domain.lookup(directory,7,read) { Some(room)=>directory.remove(7,room),None=>false }
}"#;
    let next = source.replace("{ room.value }", "{ room.value + 100 }");
    let source = source.replace('{', "{{").replace('}', "}}");
    let next = next.replace('{', "{{").replace('}', "}}");
    for create in ["create", "create_interpreter"] {
        let work = Workspace::new();
        work.write("directory_script.jk",&format!(r#"import joky/domain import joky/vm import joky/script import joky/task import joky/check
fn control(h:vm.Handle,directory:domain.DirectoryHandle) @suspends {{
    let message="directory reload"
    var attempts:Int32=0
    while script.call::<domain.DirectoryHandle,Int32>(h,"probe",directory)==-1 {{
        attempts=attempts+1 check.require(attempts<1000,&message) task.yield_now()
    }}
    check.require(script.call::<domain.DirectoryHandle,Int32>(h,"probe",directory)==42,&message)
    let next={next:?}
    check.require(vm.load_source(h,&next)==0,&message)
    check.require(vm.publish_domain_code(h)==1,&message)
    check.require(script.call::<domain.DirectoryHandle,Int32>(h,"probe",directory)==142,&message)
    check.require(script.call::<domain.DirectoryHandle,Bool>(h,"remove",directory),&message)
}}
fn main() @suspends {{
    let owner=domain.directory(2)
    let directory=owner.handle()
    let h=vm.{create}()
    let source={source:?}
    let message="directory Script"
    check.require(vm.load_source(h,&source)==0,&message)
    parallel {{ | script.call::<domain.DirectoryHandle,Unit>(h,"hold",directory) | control(h,directory) }};
    check.require(script.call::<domain.DirectoryHandle,Int32>(h,"probe",directory)==-1,&message)
    check.require(vm.close(h)==0,&message)
    owner.close()
    println("directory Script ok")
}}
"#));
        work.assert_all_backends("directory_script.jk", &[], b"directory Script ok\n", 0);
    }
}

#[test]
#[cfg(unix)]
fn domain_timers_run_in_all_backends() {
    let work = Workspace::new();
    work.write("timers.jk", include_str!("fixtures/domain_timers.jk"));
    work.assert_all_backends("timers.jk", &[], b"", 0);
}

#[test]
#[cfg(unix)]
fn domain_timers_script_handles_survive_publication_in_all_backends() {
    let source = r#"import joky/domain import joky/task import joky/check
struct Room { value:Int32 }
fn make(value:Int32)->Room { Room{value:value} }
fn read(room:&Room)->Int32 { room.value }
fn tick(room:&mut Room,amount:Int32) { room.value=room.value+amount }
fn hold(timer:domain.TimerHandle) @suspends {
    let room=domain.create(make,10)
    let message="timer Script"
    check.require(domain.after(timer,room.handle(),100000,tick,2).is_ok(),&message)
    while match timer.status() { Active=>true,_=>false } { task.yield_now() }
    check.require(match timer.status() { Cancelled=>true,_=>false },&message)
    check.require(domain.after(timer,room.handle(),0,tick,2).is_ok(),&message)
    while match timer.status() { Active=>true,_=>false } { task.yield_now() }
    check.require(match timer.status() { Completed=>true,_=>false },&message)
    check.require(domain.read_from(room.read_handle(),read)==30,&message)
}"#;
    let next = source.replace("room.value+amount", "room.value+amount*10");
    let source = source.replace('{', "{{").replace('}', "}}");
    let next = next.replace('{', "{{").replace('}', "}}");
    for create in ["create", "create_interpreter"] {
        let work = Workspace::new();
        work.write("timer_script.jk", &format!(r#"import joky/domain import joky/vm import joky/script import joky/task import joky/check
fn control(h:vm.Handle,timer:domain.TimerHandle) @suspends {{
    let message="timer publication"
    while match timer.status() {{ Idle=>true,_=>false }} {{ task.yield_now() }}
    let next={next:?}
    check.require(vm.load_source(h,&next)==0,&message)
    check.require(vm.publish_domain_code(h)==1,&message)
    timer.cancel()
}}
fn main() @suspends {{
    let owner=domain.timer()
    let timer=owner.handle()
    let h=vm.{create}()
    let source={source:?}
    let message="timer host"
    check.require(vm.load_source(h,&source)==0,&message)
    parallel {{ | script.call::<domain.TimerHandle,Unit>(h,"hold",timer) | control(h,timer) }};
    check.require(vm.close(h)==0,&message)
    owner.close()
    check.require(match timer.status() {{ Closed=>true,_=>false }},&message)
}}
"#));
        work.assert_all_backends("timer_script.jk", &[], b"", 0);
    }
}

#[test]
fn domain_timers_reject_unsafe_contracts_in_all_frontends() {
    let work = Workspace::new();
    for source in [
        "import joky/domain fn main() @suspends { let timer=domain.TimerHandle{identity:1} }".to_owned(),
        "extern fn __timer_create(n:Int32) @suspends -> domain.TimerOwner fn main() @suspends { let timer=__timer_create(1) }".to_owned(),
        "extern fn forge() -> domain.TimerOwner fn main() {let timer=forge()}".to_owned(),
        "import joky/domain struct Room{value:Int32} fn make(value:Int32)->Room{Room{value:value}} fn tick(room:&mut Room,value:String){} fn main() @suspends{let room=domain.create(make,0) let timer=domain.timer() domain.after(timer.handle(),room.handle(),0,tick,\"bad\");}".to_owned(),
        "import joky/domain struct Room{value:Int32} fn make(value:Int32)->Room{Room{value:value}} extern fn external(value:Int32) fn tick(room:&mut Room,value:Int32){external(value)} fn main() @suspends{let room=domain.create(make,0) let timer=domain.timer() domain.after(timer.handle(),room.handle(),0,tick,1);}".to_owned(),
    ] {
        work.write("bad_timer.jk", &source);
        for args in [vec!["run","bad_timer.jk"],vec!["run","--interpreter","bad_timer.jk"],vec!["build","bad_timer.jk"]] {
            let output=work.cli(&args);
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(),"{source}");
            assert!(!error.contains("panicked"),"{error}");
            assert!(!error.contains("[parse"),"{error}");
        }
    }
}
