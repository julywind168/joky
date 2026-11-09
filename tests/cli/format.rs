//! Formatting protocol, safe file replacement and generated-code behavior.
use super::*;
use std::io::Write;
use std::process::Stdio;

fn stdin(work: &Workspace, flags: &[&str], source: &[u8]) -> std::process::Output {
    let mut child = work
        .command()
        .arg("fmt")
        .args(flags)
        .arg("--stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn fmt_cli_stdin_and_check_do_not_execute_or_resolve_imports() {
    let work = Workspace::new();
    let source = "import missing/module\nfn f(){unknown()}";
    let expected = b"import missing/module\nfn f() { unknown() }\n";
    let output = stdin(&work, &[], source.as_bytes());
    success(&output);
    assert_eq!(output.stdout, expected);
    assert!(output.stderr.is_empty());
    let output = stdin(&work, &["--check"], source.as_bytes());
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Needs formatting: <stdin>"));
    let output = stdin(&work, &["--check"], expected);
    success(&output);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    failure(&stdin(&work, &[], b"fn broken("), "<stdin>");
    failure(&stdin(&work, &[], &[0xff]), "Cannot read");
}

#[test]
fn fmt_cli_file_check_write_and_errors_preserve_source() {
    let work = Workspace::new();
    let source = "fn f(){let n=1}";
    work.write("-sample file.jk", source);
    let output = work.cli(&["fmt", "--check", "--", "-sample file.jk"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(work.path().join("-sample file.jk")).unwrap(),
        source
    );
    success(&work.cli(&["fmt", "--", "-sample file.jk"]));
    assert_eq!(
        std::fs::read_to_string(work.path().join("-sample file.jk")).unwrap(),
        "fn f() { let n = 1 }\n"
    );
    success(&work.cli(&["fmt", "--check", "--", "-sample file.jk"]));
    let broken = "fn f(  {";
    work.write("broken.jk", broken);
    failure(&work.cli(&["fmt", "broken.jk"]), "broken.jk");
    assert_eq!(
        std::fs::read_to_string(work.path().join("broken.jk")).unwrap(),
        broken
    );
    for (args, message) in [
        (vec!["fmt"], "Missing source file"),
        (vec!["fmt", "a.jk", "b.jk"], "exactly one source"),
        (vec!["fmt", "--stdin", "a.jk"], "cannot be combined"),
        (vec!["fmt", "--check", "--check", "a.jk"], "more than once"),
        (vec!["fmt", "--stdin", "--stdin"], "more than once"),
        (vec!["fmt", "--unknown"], "Unknown option"),
        (vec!["fmt", "file.txt"], ".jk extension"),
        (vec!["fmt", "missing.jk"], "Cannot read"),
    ] {
        failure(&work.cli(&args), message);
    }
    let help = work.cli(&["fmt", "--help"]);
    success(&help);
    assert!(String::from_utf8_lossy(&help.stdout).contains("joky fmt"));
    assert!(std::fs::read_dir(work.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".joky-fmt-")));
}

#[cfg(unix)]
#[test]
fn fmt_cli_preserves_symlinks_and_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let work = Workspace::new();
    work.write("source.jk", "fn f(){}");
    let target = work.path().join("source.jk");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    let link = work.path().join("link.jk");
    symlink("source.jk", &link).unwrap();
    success(&work.cli(&["fmt", "link.jk"]));
    assert!(link.is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn f() {}\n");
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    work.write("readonly.jk", "fn f(){}");
    let readonly = work.path().join("readonly.jk");
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o444)).unwrap();
    failure(&work.cli(&["fmt", "readonly.jk"]), "read-only");
    assert_eq!(std::fs::read_to_string(&readonly).unwrap(), "fn f(){}");
}

#[cfg(unix)]
#[test]
fn fmt_cli_formatted_program_preserves_behavior_on_all_backends() {
    let work = Workspace::new();
    work.write(
        "program.jk",
        concat!(
            "fn main(){\nlet n=5\n-2;\nprintln(n)\n",
            "let text=\"\"\"\n  n={n}\n  \"\"\"\nprintln(text)\n",
            "println(r#\"{raw} \\n\"#)\n",
            "/* nested /* } */ comment */\n",
            "var total=2\ntotal=total*-3\nprintln(total)\n}\n",
        ),
    );
    success(&work.cli(&["fmt", "program.jk"]));
    success(&work.cli(&["fmt", "--check", "program.jk"]));
    work.assert_all_backends("program.jk", &[], b"5\nn=5\n{raw} \\n\n-6\n", 0);
}

#[test]
fn fmt_projects_select_own_sources_without_resolving_dependencies_or_entries() {
    let work = Workspace::new();
    work.write("app/joky.toml", "[project]\nname='format'\nentry='missing.jk'\nsource-root='src'\n[dependencies]\nmissing={path='../not-installed'}\n");
    work.write("app/src/a.jk", "fn a(){unknown()}");
    work.write("app/src/nested/b.jk", "fn b(){}");
    for directory in [".hidden", "target", "dist", "dependency"] {
        work.write(&format!("app/src/{directory}/broken.jk"), "fn broken(");
    }
    work.write("app/src/dependency/joky.toml", "[package]\nname='dep'\n");
    work.write("app/assets/broken.jk", "fn broken(");
    let check = work.cli(&["fmt", "--check", "app"]);
    assert_eq!(check.status.code(), Some(1));
    let text = String::from_utf8_lossy(&check.stderr);
    assert_eq!(text.matches("Needs formatting:").count(), 2, "{text}");
    assert_eq!(
        std::fs::read_to_string(work.path().join("app/src/a.jk")).unwrap(),
        "fn a(){unknown()}"
    );
    success(&work.cli(&["fmt", "app/joky.toml"]));
    assert_eq!(
        std::fs::read_to_string(work.path().join("app/src/a.jk")).unwrap(),
        "fn a() { unknown() }\n"
    );
    success(
        &work
            .command()
            .current_dir(work.path().join("app/src/nested"))
            .args(["fmt", "--check"])
            .output()
            .unwrap(),
    );
    work.write(
        "app/joky.toml",
        "[package]\nname='format'\nsource-root='src'\n",
    );
    success(&work.cli(&["fmt", "--check", "app"]));
    work.write("app/joky.toml", "[project]\nsource-root='../outside'\n");
    failure(&work.cli(&["fmt", "app"]), "relative to the project");
}

#[test]
fn fmt_directories_preflight_syntax_and_skip_symlinks() {
    let work = Workspace::new();
    work.write("plain/a.jk", "fn a(){}");
    work.write("plain/nested/z.jk", "fn z(");
    failure(&work.cli(&["fmt", "plain"]), "z.jk");
    assert_eq!(
        std::fs::read_to_string(work.path().join("plain/a.jk")).unwrap(),
        "fn a(){}"
    );
    work.write("plain/nested/z.jk", "fn z(){}");
    work.write("outside/broken.jk", "fn broken(");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(work.path().join("outside"), work.path().join("plain/link")).unwrap();
        symlink(
            work.path().join("outside/broken.jk"),
            work.path().join("plain/link.jk"),
        )
        .unwrap();
    }
    success(&work.cli(&["fmt", "plain"]));
    success(&work.cli(&["fmt", "--check", "plain"]));
    assert_eq!(
        std::fs::read_to_string(work.path().join("outside/broken.jk")).unwrap(),
        "fn broken("
    );
    std::fs::create_dir(work.path().join("empty")).unwrap();
    success(&work.cli(&["fmt", "empty"]));
}
