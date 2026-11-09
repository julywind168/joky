//! CLI arguments and execution routing.
use super::*;

#[test]
fn help_version_and_argument_errors_are_user_facing() {
    let work = Workspace::new();
    for args in [vec![], vec!["--help"]] {
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
    let detailed = work
        .command()
        .args(["--version", "--verbose"])
        .env("PATH", "")
        .output()
        .unwrap();
    success(&detailed);
    assert!(detailed.stderr.is_empty());
    let text = String::from_utf8_lossy(&detailed.stdout);
    assert!(text.starts_with(&String::from_utf8_lossy(&output.stdout).into_owned()));
    for expected in [
        format!("commit: {}", env!("JOKY_BUILD_COMMIT")),
        format!("target: {}", env!("JOKY_BUILD_TARGET")),
        format!("profile: {}", env!("JOKY_BUILD_PROFILE")),
        format!("rustc: {}", env!("JOKY_BUILD_RUSTC")),
        format!("minimum-rust: {}", env!("CARGO_PKG_RUST_VERSION")),
    ] {
        assert!(text.contains(&expected), "{text}");
    }
    let short = work.cli(&["-V", "-v"]);
    success(&short);
    assert_eq!(short.stdout, detailed.stdout);
    for args in [vec!["--version", "extra"], vec!["--version", "-v", "-v"]] {
        failure(&work.cli(&args), "Unexpected arguments");
    }
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
fn command_help_is_scoped_and_missing_project_errors_offer_next_steps() {
    let work = Workspace::new();
    for (command, option) in [
        ("init", "--name"),
        ("run", "--interpreter"),
        ("build", "--output"),
        ("check", "--no-prelude"),
        ("test", "--filter"),
        ("fmt", "--stdin"),
        ("doc", "--std"),
        ("explain-parallel", "--no-prelude"),
    ] {
        let expected = format!("joky {command}");
        let long = work.cli(&[command, "--help"]);
        success(&long);
        let text = String::from_utf8_lossy(&long.stdout);
        assert!(text.contains(&expected) && text.contains(option), "{text}");
        assert!(!text.contains("Commands:"), "{text}");
        assert!(
            !text.split_whitespace().any(|word| word
                == if command == "doc" {
                    "--interpreter"
                } else {
                    "--std"
                }),
            "{text}"
        );
        for args in [vec![command, "-h"], vec!["help", command]] {
            let output = work.cli(&args);
            success(&output);
            assert_eq!(output.stdout, long.stdout);
        }
    }
    failure(&work.cli(&["help", "unknown"]), "Unknown command");
    failure(&work.cli(&["help", "run", "extra"]), "at most one");
    let missing = work.cli(&["run"]);
    failure(&missing, "Missing source file");
    let text = String::from_utf8_lossy(&missing.stderr);
    assert!(
        text.contains("joky init") && text.contains("joky run hello.jk"),
        "{text}"
    );
    work.write("main.jk", include_str!("../../examples/hello.jk"));
    let output = work.cli(&["run", "main.jk", "--", "--help"]);
    success(&output);
    assert_eq!(output.stdout, b"Hello, world!\n");
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
        assert_eq!(output.stdout, b"Hello, world!\n");
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
            assert!(!stderr.contains("startup"), "{stderr}");
        } else {
            assert!(output.stderr.is_empty());
        }
    }
}

#[test]
fn run_progress_colors_only_status_words_and_respects_no_color() {
    let work = Workspace::new();
    work.write("hello.jk", include_str!("../../examples/hello.jk"));
    for colored in [true, false] {
        let mut command = work.command();
        command.args(["run", "--verbose", "hello.jk"]);
        if colored {
            command.env_remove("NO_COLOR").env("CLICOLOR_FORCE", "1");
        } else {
            command.env("NO_COLOR", "1").env_remove("CLICOLOR_FORCE");
        }
        let output = command.output().unwrap();
        success(&output);
        assert_eq!(output.stdout, b"Hello, world!\n");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<_> = stderr.lines().collect();
        assert_eq!(lines.len(), 3, "{stderr}");
        for (line, action) in lines.iter().zip(["Loading", "Compiling", "Running"]) {
            let prefix = if colored {
                format!("\x1b[1;32m{action:>10}\x1b[0m ")
            } else {
                format!("{action:>10} ")
            };
            let details = line.strip_prefix(&prefix).expect("styled status prefix");
            assert!(details.starts_with("hello.jk"), "{stderr}");
            assert!(!details.contains('\x1b'), "{stderr}");
        }
        assert!(lines[1].contains("JIT; loaded in "), "{stderr}");
        assert!(lines[2].contains("compiled in "), "{stderr}");
        assert!(!stderr.contains("startup"), "{stderr}");
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
    assert_eq!(output.stdout, b"Hello, world!\n");
    assert!(output.stderr.is_empty());
}
