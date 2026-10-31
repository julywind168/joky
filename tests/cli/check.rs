//! Static checks share semantic passes without executing or publishing code.
use super::*;

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
