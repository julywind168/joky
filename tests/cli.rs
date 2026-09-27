//! End-to-end checks of the installed-style command-line interface.
use std::{
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "joky-cli-{}-{unique}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, source: &str) {
        std::fs::write(self.0.join(name), source).unwrap();
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
        command.current_dir(&self.0);
        #[cfg(unix)]
        command.env("JOKY_RUNTIME", runtime_archive());
        command
    }
    fn cli(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn assert_no_build_files(&self) {
        assert!(std::fs::read_dir(&self.0).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".joky-build-")));
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn failure(output: &Output, message: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.starts_with("error: ") && error.contains(message),
        "{error}"
    );
    assert!(!error.contains("panicked"), "{error}");
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
        for args in [
            vec!["run", "aliases.jk"],
            vec!["run", "--interpreter", "aliases.jk"],
        ] {
            let output = work.cli(&args);
            assert_eq!(
                output.status.code(),
                Some(expected_status),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout, expected_output.as_bytes());
            assert!(output.stderr.is_empty());
        }
        success(&work.cli(&["build", "aliases.jk"]));
        let output = Command::new(work.0.join("aliases")).output().unwrap();
        assert_eq!(output.status.code(), Some(expected_status));
        assert_eq!(output.stdout, expected_output.as_bytes());
        assert!(output.stderr.is_empty());
    }
    // Built-in callback validation must see Int32 rather than source aliases.
    work.write("callback.jk", r#"
        import joky/vm
        type Count = Int32
        type Handle = vm.Handle
        extern fn print_int(value: Count)
        fn next(value: Count) -> Count { value + 1 }
        fn main() {
            let engine: Handle = vm.create()
            vm.register_int32(engine, "next", next)
            vm.load_source(engine, "type Count = Int32 extern fn next(x: Count) -> Count fn value() -> Count { next(41) }")
            print_int(vm.call_int32(engine, "value"))
            vm.close(engine);
        }
    "#);
    let output = work.cli(&["run", "callback.jk"]);
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    success(&work.cli(&["build", "callback.jk"]));
    let output = Command::new(work.0.join("callback")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    work.assert_no_build_files();
}

#[test]
fn alias_errors_are_located_before_execution_or_publication() {
    let work = Workspace::new();
    for (declaration, message) in [
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
            "type Flag = Bool extern fn print_int(value: Flag)",
            "Expected extern fn print_int",
        ),
    ] {
        let source = if declaration.contains("fn main") {
            declaration.to_owned()
        } else {
            format!("{declaration} fn main() {{}}")
        };
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
        assert!(!work.0.join("invalid").exists());
        work.assert_no_build_files();
    }
}

#[test]
fn strict_syntax_errors_match_all_frontends() {
    let work = Workspace::new();
    for (source, message) in [
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
    ] {
        work.write("syntax.jk", &format!("// é🙂\n{source}"));
        for args in [
            vec!["run", "syntax.jk"],
            vec!["run", "--interpreter", "syntax.jk"],
            vec!["build", "syntax.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, message);
            assert_eq!(output.status.code(), Some(1));
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("syntax.jk:2:"), "{error}");
            assert!(error.contains("Line 2"), "{error}");
        }
        assert!(!work.0.join("syntax").exists());
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
            "print_int(99)"
        } else {
            "print_int(99); 0"
        };
        let source = format!("// é\nextern fn print_int(x: Int32)\nfn divide(x: Int32) -> Int32 {{ 7 / x }}\nfn main() {return_type} {{ divide(0); {tail} }}");
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
        let output = Command::new(work.0.join("division")).output().unwrap();
        failure(&output, "Division by zero");
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains(&format!("bytes {start}..{}", start + 5)));
    }
    work.write("wrapping.jk", "extern fn print_int(x: Int32) fn divide(a: Int32, b: Int32) -> Int32 { a / b } fn main() { print_int(divide(-2147483648, -1)) print_int(divide(-7, 3)) print_int(divide(7, -3)) }");
    for args in [
        vec!["run", "wrapping.jk"],
        vec!["run", "--interpreter", "wrapping.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"-2147483648\n-2\n-2\n");
    }
    success(&work.cli(&["build", "wrapping.jk"]));
    let output = Command::new(work.0.join("wrapping")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"-2147483648\n-2\n-2\n");
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn loop_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("loops.jk", include_str!("../examples/loops.jk"));
    for args in [
        vec!["run", "loops.jk"],
        vec!["run", "--interpreter", "loops.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"45\n");
    }
    success(&work.cli(&["build", "loops.jk"]));
    let output = Command::new(work.0.join("loops")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"45\n");
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn sequence_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("sequences.jk", include_str!("../examples/sequences.jk"));
    for args in [
        vec!["run", "sequences.jk"],
        vec!["run", "--interpreter", "sequences.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"4\n3\n");
    }
    success(&work.cli(&["build", "sequences.jk"]));
    let output = Command::new(work.0.join("sequences")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"4\n3\n");
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn conditional_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write(
        "conditionals.jk",
        include_str!("../examples/conditionals.jk"),
    );
    for args in [
        vec!["run", "conditionals.jk"],
        vec!["run", "--interpreter", "conditionals.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"-1\n0\n1\n0\n4\n");
    }
    success(&work.cli(&["build", "conditionals.jk"]));
    let output = Command::new(work.0.join("conditionals")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"-1\n0\n1\n0\n4\n");
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn jump_example_matches_jit_interpreter_and_native_execution() {
    let work = Workspace::new();
    work.write("jumps.jk", include_str!("../examples/jumps.jk"));
    for args in [
        vec!["run", "jumps.jk"],
        vec!["run", "--interpreter", "jumps.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"0\n4\n13\n");
    }
    success(&work.cli(&["build", "jumps.jk"]));
    let output = Command::new(work.0.join("jumps")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"0\n4\n13\n");
    work.assert_no_build_files();
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
        ("fn main() @suspends {}", "does not support @suspends"),
        (
            "extern fn other() fn main() { other() }",
            "Unknown CLI host function",
        ),
        (
            "extern fn print_int(x: Bool) fn main() {}",
            "Expected extern fn print_int",
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
        assert!(!work.0.join("bad").exists());
        work.assert_no_build_files();
    }
}

#[test]
fn parser_errors_render_ariadne_source_context() {
    let work = Workspace::new();
    work.write("bad.jk", "fn main() {\n#\n}\n");
    let output = work.cli(&["run", "bad.jk"]);
    failure(&output, "Unexpected character");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("bad.jk:2:1"), "{error}");
    assert!(error.contains("2 │ #"), "{error}");
    assert!(error.contains("Unexpected character '#'"), "{error}");
}

#[test]
fn parser_diagnostics_use_utf8_byte_spans() {
    let work = Workspace::new();
    work.write("unicode.jk", "fn main() { let x = \"🙂\" # }\n");
    let output = work.cli(&["run", "unicode.jk"]);
    failure(&output, "Unexpected character");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("unicode.jk:1:25"), "{error}");
    assert!(
        error.contains("1 │ fn main() { let x = \"🙂\" # }"),
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
        extern fn print_int(value: Int32)
        fn printf(x: Int32) -> Int32 { x + 1 }
        fn pick(unit: Unit, value: Int32) -> Int32 { unit value }
        fn main() { print_int(pick({}, printf(41))) }
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
    std::fs::remove_file(work.0.join("hello world.jk")).unwrap();
    let native = Command::new(work.0.join("hello world"))
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
            "fn print_int(x: Int32) -> Int32 { x } fn main() -> Int32 { print_int(8) }",
            8,
        ),
    ] {
        work.write("exit.jk", source);
        let run = work.cli(&["run", "exit.jk"]);
        assert_eq!(run.status.code(), Some(code));
        assert!(run.stdout.is_empty() && run.stderr.is_empty());
        success(&work.cli(&["build", "--output", "custom program", "exit.jk"]));
        let native = Command::new(work.0.join("custom program"))
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
    symlink(work.0.join("safe.jk"), work.0.join("alias")).unwrap();
    failure(
        &work.cli(&["build", "safe.jk", "-o", "alias"]),
        "must not replace the source",
    );
    assert_eq!(
        std::fs::read_to_string(work.0.join("safe.jk")).unwrap(),
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
    std::fs::set_permissions(work.0.join("cc"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = work
        .command()
        .args(["build", "safe.jk", "-o", "existing"])
        .env("PATH", &work.0)
        .output()
        .unwrap();
    failure(&output, "test linker failure");
    work.write("safe.jk", "fn main() { unknown() }");
    failure(
        &work.cli(&["build", "safe.jk", "-o", "existing"]),
        "Undeclared function",
    );
    assert_eq!(
        std::fs::read_to_string(work.0.join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[cfg(unix)]
fn runtime_archive() -> PathBuf {
    // Cargo emits the current library's static archive beside the test binary,
    // including on a clean `cargo test` without a preceding `cargo build`.
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    std::fs::read_dir(deps)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("libjoky-")
                && path.extension().is_some_and(|ext| ext == "a")
        })
        .max_by_key(|path| path.metadata().unwrap().modified().unwrap())
        .expect("Cargo must build the runtime static archive")
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
    std::fs::remove_file(work.0.join("vm.jk")).unwrap();
    let native = Command::new(work.0.join("vm"))
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
        extern fn print_int(value: Int32)
        fn source() -> String { "fn value() -> Int32 { 42 }" }
        fn identity(text: String) -> String { text }
        fn main() -> Int32 {
            let engine = vm.create()
            print_int(vm.load_source(engine, identity(source())))
            print_int(vm.call_int32(engine, "value"))
            print_int(vm.reload_source(engine, "fn value() -> Bool { 42 }"))
            print_int(vm.error_code())
            vm.print_error()
            print_int(vm.call_int32(engine, "value"))
            print_int(vm.call_int32(engine, "missing"))
            print_int(vm.error_code())
            vm.print_error()
            print_int(vm.load_source(engine, "import joky/vm fn nested() -> Int32 { let child = vm.create() vm.close(child) }"))
            print_int(vm.call_int32(engine, "nested"))
            print_int(vm.close(engine))
            print_int(vm.close(engine))
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
    let native = Command::new(work.0.join("errors")).output().unwrap();
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
            "import joky/vm fn main() { let h = vm.create() vm.register_int32(h, \"missing\", missing) }",
            "not defined in this module",
        ),
        (
            "import joky/vm fn host(value: String) -> Int32 { 1 } fn main() { let h = vm.create() vm.register_int32(h, \"host\", host) }",
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
        .env("JOKY_RUNTIME", work.0.join("missing.a"))
        .output()
        .unwrap();
    failure(&output, "JOKY_RUNTIME does not name a runtime archive");
    assert_eq!(
        std::fs::read_to_string(work.0.join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn installed_compiler_finds_its_runtime_without_repository_paths() {
    let work = Workspace::new();
    let bin = work.0.join("installation/bin");
    let lib = work.0.join("installation/lib/joky");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_joky"), bin.join("joky")).unwrap();
    std::fs::copy(runtime_archive(), lib.join("libjoky.a")).unwrap();
    work.write("vm.jk", include_str!("../examples/vm.jk"));
    let output = Command::new(bin.join("joky"))
        .current_dir(&work.0)
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
        .current_dir(&work.0)
        .args(["build", "division.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    std::fs::remove_dir_all(work.0.join("installation")).unwrap();
    std::fs::remove_file(work.0.join("vm.jk")).unwrap();
    let program = Command::new(work.0.join("vm"))
        .env("PATH", "")
        .output()
        .unwrap();
    success(&program);
    assert_eq!(program.stdout, b"42\n100\n");
    std::fs::remove_file(work.0.join("division.jk")).unwrap();
    let program = Command::new(work.0.join("division"))
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
    for args in [
        vec!["run", "records.jk"],
        vec!["run", "--interpreter", "records.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"1\n42\n");
    }
    success(&work.cli(&["build", "records.jk"]));
    let output = Command::new(work.0.join("records")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"1\n42\n");
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
            "struct P {} extern fn print_int(value: P) fn main() {}",
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
        assert!(!work.0.join("invalid_record").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn enums_run_and_build_with_nominal_type_diagnostics() {
    let work = Workspace::new();
    work.write("enums.jk", include_str!("../examples/enums.jk"));
    for args in [
        vec!["run", "enums.jk"],
        vec!["run", "--interpreter", "enums.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"0\n42\n");
    }
    success(&work.cli(&["build", "enums.jk"]));
    let output = Command::new(work.0.join("enums")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"0\n42\n");
    for (source, message) in [
        ("enum E { Again(E) } fn main() {}", "recursive_type"),
        (
            "enum E { A B } fn main() { match E.A { | A => {} } }",
            "non_exhaustive_match",
        ),
        (
            "enum E { A } fn main() { match E.A { | _ => {} | A => {} } }",
            "unreachable_code",
        ),
        (
            "enum E { A } fn main() { match E.A { | Missing => {} } }",
            "invalid_pattern",
        ),
        ("enum E { A } fn main() -> E { E.A }", "Entry point must be"),
        (
            "enum E { A } extern fn print_int(value: E) fn main() {}",
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
        assert!(!work.0.join("invalid_enum").exists());
    }
    work.assert_no_build_files();
}
