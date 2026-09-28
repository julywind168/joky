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
        std::fs::create_dir_all(self.0.join(name).parent().unwrap()).unwrap();
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

#[test]
#[cfg(unix)]
fn user_file_modules_run_and_build_from_the_entry_directory() {
    let work = Workspace::new();
    work.write(
        "project/app/value.jk",
        r#"
        pub enum Choice { Empty Value(Int32) }
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
        extern fn print_int(n: Int32)
        fn main() -> Int32 {
            let x = match l.value() { | Value(n) => n | Choice.Empty => 0 }
            let y = match r.value() { | Value(n) => n | Choice.Empty => 0 }
            print_int(x + y)
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
    let output = Command::new(work.0.join("program")).output().unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"42\n");
    work.assert_no_build_files();
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
        std::fs::read_to_string(work.0.join("app/math.jk")).unwrap(),
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
            host.load_source(h, "fn answer() -> Int32 { 42 }")
            let result = host.call_int32(h, "answer")
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
    let output = Command::new(work.0.join("program")).output().unwrap();
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
    let output = Command::new(work.0.join("program")).output().unwrap();
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
    let output = Command::new(work.0.join("boom")).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Division by zero"));
    work.assert_no_build_files();
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
    // Artifacts stay fixed during this test process. Avoid rescanning a large
    // deps directory for every CLI subprocess and every concurrent test.
    static ARCHIVE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ARCHIVE
        .get_or_init(|| {
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
        })
        .clone()
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
    work.write(
        "shift.jk",
        "fn shift(n: UInt64) -> UInt64 { 1 << n } fn main() { shift(64); }",
    );
    let output = Command::new(bin.join("joky"))
        .current_dir(&work.0)
        .args(["build", "shift.jk"])
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
    std::fs::remove_file(work.0.join("shift.jk")).unwrap();
    let program = Command::new(work.0.join("shift"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    failure(&program, "Shift count");
    assert!(program.stdout.is_empty());
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

#[test]
#[cfg(unix)]
fn tuples_run_and_build_with_pattern_diagnostics() {
    let work = Workspace::new();
    work.write("tuples.jk", include_str!("../examples/tuples.jk"));
    for args in [
        vec!["run", "tuples.jk"],
        vec!["run", "--interpreter", "tuples.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"42\n13\n7\n");
    }
    success(&work.cli(&["build", "tuples.jk"]));
    let output = Command::new(work.0.join("tuples")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n13\n7\n");
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
            "extern fn print_int(value: (Int32,)) fn main() {}",
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
        assert!(!work.0.join("invalid_tuple").exists());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn int64_literals_output_and_failures_match_all_frontends() {
    let work = Workspace::new();
    work.write("int64.jk", include_str!("../examples/int64.jk"));
    let expected = b"3000000042\n5000000000\n-9223372036854775808\n";
    for args in [
        vec!["run", "int64.jk"],
        vec!["run", "--interpreter", "int64.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
    success(&work.cli(&["build", "int64.jk"]));
    let output = Command::new(work.0.join("int64")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, expected);
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
            "extern fn print_int64(n: Int32) fn main() {}",
            "Expected extern fn print_int64(value: Int64)",
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
        assert!(!work.0.join("invalid64").exists());
    }
    work.write("division64.jk", "extern fn print_int64(n: Int64) fn divide(n: Int64) -> Int64 { 3000000000 / n } fn main() { print_int64(divide(0)) }");
    for args in [
        vec!["run", "division64.jk"],
        vec!["run", "--interpreter", "division64.jk"],
    ] {
        let output = work.cli(&args);
        failure(&output, "Division by zero");
        assert!(output.stdout.is_empty());
    }
    success(&work.cli(&["build", "division64.jk"]));
    let output = Command::new(work.0.join("division64")).output().unwrap();
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
    for args in [
        vec!["run", "unsigned.jk"],
        vec!["run", "--interpreter", "unsigned.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
    success(&work.cli(&["build", "unsigned.jk"]));
    let output = Command::new(work.0.join("unsigned")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, expected);
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
            "extern fn print_uint32(n: Int32) fn main() {}",
            "Expected extern fn print_uint32(value: UInt32)",
        ),
        (
            "extern fn print_uint64(n: Int64) fn main() {}",
            "Expected extern fn print_uint64(value: UInt64)",
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
        assert!(!work.0.join("invalid_unsigned").exists());
    }
    for (ty, print, max) in [
        ("UInt32", "print_uint32", "4294967295"),
        ("UInt64", "print_uint64", "18446744073709551615"),
    ] {
        work.write("division_unsigned.jk", &format!("extern fn {print}(n: {ty}) fn divide(n: {ty}) -> {ty} {{ {max} / n }} fn main() {{ {print}(divide(0)) }}"));
        for args in [
            vec!["run", "division_unsigned.jk"],
            vec!["run", "--interpreter", "division_unsigned.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "Division by zero");
            assert!(output.stdout.is_empty());
        }
        success(&work.cli(&["build", "division_unsigned.jk"]));
        let output = Command::new(work.0.join("division_unsigned"))
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
    for args in [
        vec!["run", "bitwise.jk"],
        vec!["run", "--interpreter", "bitwise.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
    success(&work.cli(&["build", "bitwise.jk"]));
    let output = Command::new(work.0.join("bitwise")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, expected);
    for (source, message) in [
        ("fn main() { let x = 0b102 }", "Invalid integer literal"),
        ("fn main() { let x = 0x_aa }", "Invalid integer literal"),
        ("fn main() { let x = 1__000 }", "Invalid integer literal"),
        (
            "fn main() { let x = 0x1_0000_0000_0000_0000 }",
            "outside UInt64 magnitude range",
        ),
        ("fn main() { let x = 0xffff_ffff }", "outside Int32 range"),
        ("fn main() { let x = true & false }", "same integer type"),
        ("fn main() { let x = ~true }", "Unary `~` operand"),
        (
            "enum E { A B } fn main() { match E.A { | A => 1 | 2 | B => 3 } }",
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
        assert!(!work.0.join("invalid_bits").exists());
    }
    for (ty, op, count) in [
        ("Int32", "<<", "-1"),
        ("UInt32", ">>", "32"),
        ("Int64", ">>", "64"),
        ("UInt64", "<<", "0x1_0000_0000"),
    ] {
        work.write("shift.jk", &format!("extern fn print_int(n: Int32) fn shift(n: {ty}) -> {ty} {{ 1 {op} n }} fn main() {{ shift({count}); print_int(99) }}"));
        for args in [
            vec!["run", "shift.jk"],
            vec!["run", "--interpreter", "shift.jk"],
        ] {
            let output = work.cli(&args);
            failure(&output, "Shift count");
            assert!(output.stdout.is_empty());
        }
        success(&work.cli(&["build", "shift.jk"]));
        let output = Command::new(work.0.join("shift")).output().unwrap();
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
    for args in [
        vec!["run", "conversions.jk"],
        vec!["run", "--interpreter", "conversions.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
    success(&work.cli(&["build", "conversions.jk"]));
    let output = Command::new(work.0.join("conversions")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, expected);
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
        assert!(!work.0.join("invalid_conversion").exists());
    }
    for (name, argument, message) in [
        ("division", "1 / 0", "Division by zero"),
        ("shift", "1 << 32", "Shift count"),
    ] {
        let file = format!("{name}_conversion.jk");
        work.write(
            &file,
            &format!(
                "extern fn print_int(n: Int32) fn main() {{ uint64({argument}); print_int(99) }}"
            ),
        );
        for args in [vec!["run", &file], vec!["run", "--interpreter", &file]] {
            let output = work.cli(&args);
            failure(&output, message);
            assert!(output.stdout.is_empty());
        }
        success(&work.cli(&["build", &file]));
        let output = Command::new(work.0.join(format!("{name}_conversion")))
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
    for args in [
        vec!["run", "generics.jk"],
        vec!["run", "--interpreter", "generics.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
    success(&work.cli(&["build", "generics.jk"]));
    let output = Command::new(work.0.join("generics")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, expected);
    for (source, message) in [
        (
            "fn bad<T>(n: T) -> T { n + n } fn main() {}",
            "same integer type",
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
            "Copy type",
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
            "Generic instantiation exceeds",
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
        assert!(!work.0.join("invalid_generic").exists());
    }
    work.write("generic_failure.jk", "extern fn print_int(n: Int32) fn fail<T>(x: T, n: Int32) -> T { 1 / n; x } fn main() { fail((1, true), 0); print_int(99) }");
    for args in [
        vec!["run", "generic_failure.jk"],
        vec!["run", "--interpreter", "generic_failure.jk"],
    ] {
        let output = work.cli(&args);
        failure(&output, "Division by zero");
        assert!(output.stdout.is_empty());
    }
    success(&work.cli(&["build", "generic_failure.jk"]));
    let output = Command::new(work.0.join("generic_failure"))
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
    for args in [
        vec!["run", "generic_enums.jk"],
        vec!["run", "--interpreter", "generic_enums.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
    success(&work.cli(&["build", "generic_enums.jk"]));
    let output = Command::new(work.0.join("generic_enums")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, expected);
    let source = include_str!("fixtures/generic_enums.jk");
    work.write(
        "enum_fixture.jk",
        &format!("{source}\nextern fn print_int(n: Int32)\nfn main() {{ print_int(check()) }}"),
    );
    for args in [
        vec!["run", "--no-prelude", "enum_fixture.jk"],
        vec!["run", "--interpreter", "--no-prelude", "enum_fixture.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, b"42\n");
    }
    success(&work.cli(&["build", "--no-prelude", "enum_fixture.jk"]));
    let output = Command::new(work.0.join("enum_fixture")).output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    for (source, message) in [
        ("fn main() { Option.None }", "Cannot infer type parameter"),
        ("fn main() { Option.Some(\"text\") }", "Copy type"),
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
            "enum Bad<T> { Value(String) } fn main() {}",
            "Field or payload",
        ),
        (
            "fn main() { match Option.Some(1) { | None => {} } }",
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
        assert!(!work.0.join("invalid_enum").exists());
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
    for args in [
        vec!["run", "generic_records.jk"],
        vec!["run", "--interpreter", "generic_records.jk"],
    ] {
        let output = work.cli(&args);
        success(&output);
        assert_eq!(output.stdout, expected);
    }
    success(&work.cli(&["build", "generic_records.jk"]));
    let output = Command::new(work.0.join("generic_records"))
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, expected);
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
            "struct Bad<T> { value: String } fn main() {}",
            "Field or payload",
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
                "{}\nextern fn print_int(n: Int32)\nfn main() {{ print_int(check()) }}",
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
        let output = Command::new(work.0.join(file.trim_end_matches(".jk")))
            .output()
            .unwrap();
        success(&output);
        assert_eq!(output.stdout, expected.as_bytes());
    }
    for (source, message) in [
        (
            "impl Int32 { fn f(self) @suspends {} } fn main() {}",
            "CLI does not support @suspends",
        ),
        (
            "impl Int32 { const BAD: Int32 = 1 / 0 } fn main() {}",
            "constant expression",
        ),
        (
            "import joky/integer fn main() { true.to_u64(); }",
            "has no method",
        ),
        (
            "import joky/integer enum Option<T> { No Yes(T) } fn main() {}",
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
        ("prelude", "fn main() -> Int32 { match (42).to_u64() { | Some(n) => int32(n) | None => 0 } }".to_owned(), false),
        ("imports", format!("{}\nfn main() -> Int32 {{ check() }}", include_str!("fixtures/imports.jk")), true),
        ("plain", "enum Option { Present(Int32) } fn main() -> Int32 { match Option.Present(42) { | Present(n) => n } }".into(), true),
        ("explicit", "import joky/prelude fn main() -> Int32 { match Some(42) { | Some(n) => n | None => 0 } }".into(), true),
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
        let output = Command::new(work.0.join(file)).output().unwrap();
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
            "register(h, \"next\", next) v.load_source(h, \"extern fn next(n: Int32) -> Int32 fn answer() -> Int32 { next(41) }\")"
        } else {
            "v.load_source(h, \"fn answer() -> Int32 { 42 }\")"
        };
        let source = format!("import joky/vm as v.{{type Handle as Engine, create as new, register_int32 as register, call_int32 as invoke, close}} fn next(n: Int32) -> Int32 {{ n + 1 }} fn main() -> Int32 {{ let vm = 7 let h: Engine = new() {initialization} let answer = invoke(h, \"answer\") close(h) answer }}");
        work.write("host_alias.jk", &source);
        let output = work.cli(&["run", "host_alias.jk"]);
        assert_eq!(output.status.code(), Some(42), "{output:?}");
        if !register {
            let output = work.cli(&["run", "--interpreter", "host_alias.jk"]);
            assert_eq!(output.status.code(), Some(42), "{output:?}");
        }
        success(&work.cli(&["build", "host_alias.jk"]));
        let output = Command::new(work.0.join("host_alias")).output().unwrap();
        assert_eq!(output.status.code(), Some(42), "{output:?}");
    }
    work.assert_no_build_files();
}
