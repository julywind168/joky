//! CLI arguments and execution routing.
use super::*;

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
        (vec!["build", "-v", "hello.jk"], "only available"),
        (
            vec!["run", "-v", "--quiet", "hello.jk"],
            "cannot be combined",
        ),
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
    work.write("hello.jk", include_str!("../../examples/hello.jk"));
    for flags in [vec![], vec!["--quiet"], vec!["--verbose"]] {
        let output = work
            .command()
            .args(["run", "--interpreter", "hello.jk"])
            .args(&flags)
            .env("PATH", "")
            .output()
            .unwrap();
        success(&output);
        assert_eq!(output.stdout, b"42\n");
        if flags == ["--verbose"] {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let lines: Vec<_> = stderr.lines().map(str::trim_start).collect();
            assert_eq!(lines.len(), 3, "{stderr}");
            assert_eq!(lines[0], "Loading hello.jk");
            assert!(
                lines[1].starts_with("Compiling hello.jk (bytecode; loaded in "),
                "{stderr}"
            );
            assert!(
                lines[2].starts_with("Running hello.jk (compiled in "),
                "{stderr}"
            );
            assert!(lines[2].contains("; startup "), "{stderr}");
        } else {
            assert!(output.stderr.is_empty());
        }
    }
}

#[test]
fn run_needs_no_c_compiler_and_accepts_paths_after_double_dash() {
    let work = Workspace::new();
    work.write("-hello world.jk", include_str!("../../examples/hello.jk"));
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
