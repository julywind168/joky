//! Static checks share semantic passes without executing or publishing code.
use super::*;
use serde_json::{json, Value};

fn json_check(work: &Workspace, args: &[&str], status: i32) -> Value {
    let output = work.cli(args);
    assert_eq!(output.status.code(), Some(status), "{args:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schemaVersion"], 1);
    result["diagnostics"].clone()
}

#[test]
fn check_is_silent_without_execution_linker_or_artifacts() {
    let work = Workspace::new();
    work.write(
        "-program space.jk",
        "fn main() { println(42); panic(\"must not run\") }",
    );
    let output = work
        .command()
        .args(["check", "--", "-program space.jk"])
        .env("PATH", "")
        .env("JOKY_RUNTIME", "missing-runtime.a")
        .env("JOKY_TASK_WORKERS", "invalid")
        .output()
        .unwrap();
    success(&output);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 1);
}

#[test]
fn check_accepts_libraries_and_controls_the_prelude() {
    let work = Workspace::new();
    work.write("library.jk", "pub fn answer() -> Int32 { 42 }");
    success(&work.cli(&["check", "library.jk", "--no-prelude"]));
    failure(&work.cli(&["run", "library.jk"]), "Missing entry point");
    work.write("library.jk", "pub fn display() { println(42) }");
    success(&work.cli(&["check", "library.jk"]));
    failure(
        &work.cli(&["check", "--no-prelude", "library.jk"]),
        "println",
    );
    work.write(
        "library.jk",
        "import joky/io.{println} pub fn display() { println(42) }",
    );
    success(&work.cli(&["check", "--no-prelude", "library.jk"]));
    work.write(
        "pipeline.jk",
        include_str!("../../examples/captured_pipeline.jk"),
    );
    success(&work.cli(&["check", "pipeline.jk"]));
    work.write(
        "async.jk",
        "import joky/task fn main() @suspends { task.yield_now() }",
    );
    success(&work.cli(&["check", "async.jk"]));
}

#[test]
fn check_validates_unused_bodies_templates_and_resource_contracts() {
    let work = Workspace::new();
    for (source, message) in [
        ("fn unused() -> Int32 { true }", "Return type mismatch"),
        (
            "fn unused<T>(value: T) -> Int32 { value }",
            "Return type mismatch",
        ),
        (
            "fn unused() { let text = \"hello\"; let moved = text; println(text) }",
            "moved",
        ),
        (
            "fn unused() { var value = 1; let view = &value; value = 2; println(*view) }",
            "borrow",
        ),
        (
            "fn unused(value: &Int32) @noescape -> &Int32 { value }",
            "noescape",
        ),
        (
            "import joky/task fn unused() { task.yield_now() }",
            "suspend",
        ),
    ] {
        work.write("library.jk", &format!("// λ 中文\n{source}\n"));
        let output = work.cli(&["check", "library.jk"]);
        failure(&output, message);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("library.jk:2:"));
    }
}

#[test]
fn check_projects_resolve_roots_discover_manifests_and_require_entries() {
    let work = Workspace::new();
    work.write(
        "app/joky.toml",
        "[project]\nname='demo'\nentry='src/bin/main.jk'\nsource-root='src'",
    );
    work.write("app/src/shared.jk", "pub fn answer() -> Int32 { 42 }");
    work.write(
        "app/src/bin/main.jk",
        "import shared fn main() -> Int32 { shared.answer() }",
    );
    for input in ["app", "app/joky.toml"] {
        success(&work.cli(&["check", input]));
    }
    success(
        &work
            .command()
            .arg("check")
            .current_dir(work.path().join("app/src/bin"))
            .output()
            .unwrap(),
    );
    // Explicit files retain their own source root, even inside a project.
    failure(&work.cli(&["check", "app/src/bin/main.jk"]), "shared.jk");
    work.write("app/src/shared.jk", "pub fn answer() -> Int32 { false }");
    let output = work.cli(&["check", "app"]);
    failure(&output, "Return type mismatch");
    assert!(String::from_utf8_lossy(&output.stderr).contains("src/shared.jk:1:"));
    work.write("app/src/bin/main.jk", "pub fn helper() {}");
    failure(&work.cli(&["check", "app"]), "Missing entry point");
    success(&work.cli(&["check", "app/src/bin/main.jk"]));
    assert!(!work.path().join("app/dist").exists());
    work.assert_no_build_files();
}

#[test]
fn check_reports_argument_and_input_errors() {
    let work = Workspace::new();
    let help = work.cli(&["check", "--help"]);
    success(&help);
    assert!(String::from_utf8_lossy(&help.stdout).contains("joky check"));
    for (args, message) in [
        (vec!["check"], "Missing source file"),
        (vec!["check", "a.jk", "b.jk"], "exactly one source"),
        (vec!["check", "a.txt"], ".jk extension"),
        (vec!["check", "missing.jk"], "Cannot read"),
        (
            vec!["check", "--no-prelude", "--no-prelude"],
            "more than once",
        ),
        (vec!["check", "--interpreter"], "Unknown check option"),
        (vec!["check", "--parallel"], "Unknown check option"),
        (vec!["check", "--task-workers", "2"], "Unknown check option"),
        (vec!["check", "-o", "output"], "Unknown check option"),
    ] {
        let output = work.cli(&args);
        failure(&output, message);
        assert_eq!(output.status.code(), Some(1));
    }
    work.write(
        "app/joky.toml",
        "[project]\nname='demo'\nentry='../escape.jk'",
    );
    failure(&work.cli(&["check", "app"]), "relative");
}

#[test]
fn check_json_preserves_exit_status_and_separates_output_modes() {
    let work = Workspace::new();
    work.write("-file.jk", "pub fn answer() -> Int32 { 42 }");
    assert_eq!(
        json_check(
            &work,
            &["check", "--diagnostic-format", "json", "--", "-file.jk"],
            0
        ),
        json!([])
    );
    for (args, code, phase) in [
        (
            &["check", "--diagnostic-format", "json", "missing.jk"][..],
            "io_error",
            "resolve",
        ),
        (
            &["check", "--diagnostic-format", "json", "bad.txt"][..],
            "invalid_argument",
            "cli",
        ),
    ] {
        let diagnostics = json_check(&work, args, 1);
        assert_eq!(diagnostics.as_array().unwrap().len(), 1);
        let error = &diagnostics[0];
        assert_eq!(error["code"], code);
        assert_eq!(error["severity"], "error");
        assert_eq!(error["phase"], phase);
        assert_eq!(error["primary"], Value::Null);
        assert_eq!(error["labels"], json!([]));
        assert_eq!(error["help"], json!([]));
    }

    let output = work.cli(&["check", "--diagnostic-format", "text", "missing.jk"]);
    failure(&output, "Cannot read");
    assert!(output.stdout.is_empty());
    for args in [
        &["check", "--diagnostic-format"][..],
        &["check", "--diagnostic-format", "xml"][..],
        &[
            "check",
            "--diagnostic-format",
            "json",
            "--diagnostic-format",
            "text",
        ][..],
    ] {
        failure(&work.cli(args), "--diagnostic-format");
    }
}

#[test]
fn check_json_uses_original_utf8_byte_spans_and_canonical_file_paths() {
    let work = Workspace::new();
    work.write("main.jk", "import app/bad\nfn main() {}");
    let file = "app/bad.jk";
    let source = "\u{feff}// heading\r\npub fn bad() { let text = \"🙂\" $ }\r\n";
    work.write(file, source);
    let bad_path = work.path().join(file).canonicalize().unwrap();
    let start = source.find('$').unwrap();
    let diagnostics = json_check(
        &work,
        &["check", "main.jk", "--diagnostic-format", "json"],
        1,
    );
    let error = &diagnostics[0];
    assert_eq!(error["code"], "syntax_error");
    assert_eq!(error["phase"], "parse");
    assert_eq!(
        error["primary"],
        json!({
            "source": {"kind": "file", "path": bad_path},
            "startByte": start,
            "endByte": start + 1,
        })
    );
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("Unexpected character"));
    failure(&work.cli(&["check", "main.jk"]), message);

    let source = "fn unfinished(\r\n";
    work.write("eof.jk", source);
    let diagnostics = json_check(
        &work,
        &["check", "--diagnostic-format", "json", "./eof.jk"],
        1,
    );
    assert_eq!(
        diagnostics[0]["primary"],
        json!({
            "source": {"kind": "file", "path": work.path().join("eof.jk").canonicalize().unwrap()},
            "startByte": source.len(),
            "endByte": source.len(),
        })
    );
}
