//! Runner contracts, separate from the language tests that the runner executes.
use super::*;

#[test]
fn native_tests_run_private_sync_and_suspending_functions() {
    let work = Workspace::new();
    work.write("demo.jk", include_str!("../../examples/testing.jk"));
    work.write("stdlib_test.jk", include_str!("../joky/stdlib_test.jk"));
    for backend in [vec![], vec!["--interpreter"]] {
        let mut args = vec!["test", "demo.jk"];
        args.extend(backend.clone());
        let output = work.cli(&args);
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("3 passed; 0 failed"));
        assert!(output.stderr.is_empty());
        let mut args = vec!["test", "stdlib_test.jk"];
        args.extend(backend);
        let output = work.cli(&args);
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("4 passed; 0 failed"));
    }
    let list = work.cli(&["test", "demo.jk", "--list"]);
    success(&list);
    assert_eq!(String::from_utf8(list.stdout).unwrap(), "demo::adds_numbers\ndemo::filters_with_local_threshold\ndemo::preserves_values_across_suspension\n");
    let output = work.cli(&["test", "demo.jk", "--filter", "adds_numbers"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
    failure(
        &work.cli(&["test", "demo.jk", "--filter", "absent"]),
        "No tests match",
    );
    work.write("none.jk", "fn helper() {}");
    failure(&work.cli(&["test", "none.jk"]), "No #test functions");
}

#[test]
fn native_tests_report_failures_and_continue_with_fresh_processes() {
    let work = Workspace::new();
    work.write("suite.jk", r#"
import joky/task
fn main() { panic("must not execute main") }
#test
fn a_assertion() { println("failure output"); assert(false, "expected assertion") }
#test
fn b_suspending_failure() @suspends { task.yield_now(); panic("async failure") }
#test
fn c_pass() { println("hidden success output"); eprintln("hidden success error"); assert(true, "ok") }
"#);
    for backend in [vec![], vec!["--interpreter"]] {
        let mut args = vec!["test", "suite.jk"];
        args.extend(backend);
        let output = work.cli(&args);
        failed_test(&output, "expected assertion");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("1 passed; 2 failed"), "{stdout}");
        assert!(stderr.contains("async failure"), "{stderr}");
        assert!(stderr.contains("suite.jk:5:"), "{stderr}");
        assert!(stderr.contains("failure output"));
        assert!(!stderr.contains("must not execute main"));
        assert!(!stderr.contains("hidden success"));
        assert!(!stdout.contains("hidden success"));
    }
    let output = work.cli(&["test", "suite.jk", "--filter", "c_pass", "--nocapture"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("hidden success output"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("hidden success error"));
}

#[test]
fn native_tests_discover_project_sources_once_and_keep_import_tests_local() {
    let work = Workspace::new();
    work.write(
        "app/joky.toml",
        "[project]\nname='tests'\nentry='src/main.jk'\nsource-root='src'",
    );
    work.write("app/src/main.jk", "import maths fn main() -> Int32 { maths.answer() }\n#test fn entry_test() { assert(maths.answer() == 42, \"answer\") }");
    work.write("app/src/maths.jk", "fn private_answer() -> Int32 { 42 } pub fn answer() -> Int32 { private_answer() }\n#test fn private_test() { assert(private_answer() == 42, \"private\") }");
    work.write(
        "app/src/nested/smoke_test.jk",
        "import maths\n#test fn integration() { assert(maths.answer() == 42, \"integration\") }",
    );
    work.write("app/src/unused.jk", "this is intentionally not compiled");
    work.write("app/src/.hidden/ignored_test.jk", "invalid hidden source");
    work.write("app/src/dist/ignored_test.jk", "invalid output source");
    let output = work
        .command()
        .args(["test", "--list"])
        .current_dir(work.path().join("app/src/nested"))
        .output()
        .unwrap();
    success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "main::entry_test\nmaths::private_test\nnested/smoke_test::integration\n"
    );
    for input in ["app", "app/joky.toml"] {
        let output = work.cli(&["test", input]);
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("3 passed; 0 failed"));
    }
    // A single root imports production code, never the dependency's tests.
    work.write(
        "app/src/maths.jk",
        "pub fn answer() -> Int32 { 42 }\n#test fn fails() { panic(\"dependency test\") }",
    );
    let output = work.cli(&["test", "app/src/main.jk"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
    work.assert_no_build_files();
}

#[test]
fn native_tests_preflight_bodies_and_exclude_test_entries_from_production() {
    let work = Workspace::new();
    work.write(
        "program.jk",
        "fn main() -> Int32 { 7 }\n#test fn invalid_test() { missing() }",
    );
    for args in [
        vec!["test", "program.jk"],
        vec!["test", "program.jk", "--list"],
        vec!["test", "program.jk", "--filter", "absent"],
    ] {
        let output = work.cli(&args);
        failure(&output, "missing");
        assert!(String::from_utf8_lossy(&output.stderr).contains("program.jk:2:"));
        assert!(output.stdout.is_empty());
    }
    success(&work.cli(&["check", "program.jk"]));
    #[cfg(unix)]
    work.assert_all_backends("program.jk", &[], b"", 7);
    work.write("program.jk", "fn main() { helper() } #test fn helper() {}");
    failure(&work.cli(&["check", "program.jk"]), "helper");
    work.write("-library.jk", "#test fn checks() { assert(true, \"ok\") }");
    failure(
        &work.cli(&["test", "--no-prelude", "--", "-library.jk"]),
        "assert",
    );
    work.write(
        "-library.jk",
        "import joky/check.{assert}\n#test fn checks() { assert(true, \"ok\") }",
    );
    success(&work.cli(&["test", "--no-prelude", "--", "-library.jk"]));
}

#[test]
fn native_tests_timeout_sync_loops_and_suspended_tasks_then_continue() {
    let work = Workspace::new();
    work.write("timeout.jk", "import joky/task\n#test fn a_loop() { while true {} }\n#test fn b_sleep() @suspends { task.sleep(60s) }\n#test fn c_pass() {}");
    for backend in [vec![], vec!["--interpreter"]] {
        let mut args = vec!["test", "timeout.jk", "--no-prelude", "--timeout-ms", "2000"];
        args.extend(backend);
        let output = work.cli(&args);
        failed_test(&output, "exceeded 2000 ms");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout.matches("TIMEOUT").count(), 2, "{stdout}");
        assert!(stdout.contains("1 passed; 2 failed"), "{stdout}");
    }
}

#[test]
fn native_tests_drain_and_bound_captured_output() {
    let work = Workspace::new();
    let payload = "x".repeat(1024);
    work.write("output.jk", &format!("#test fn noisy() {{ var n = 0; while n < 1100 {{ print(\"{payload}\"); eprint(\"{payload}\"); n += 1 }}; panic(\"done\") }}"));
    let output = work.cli(&["test", "output.jk"]);
    failed_test(&output, "output truncated after 1 MiB");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("output truncated after 1 MiB")
            .count(),
        2
    );
    assert!(output.stderr.len() < 2 * 1024 * 1024 + 4096);
}

#[test]
fn native_tests_reject_invalid_runner_arguments() {
    let work = Workspace::new();
    for (args, message) in [
        (vec!["test"], "Missing source"),
        (vec!["test", "a.jk", "b.jk"], "exactly one"),
        (vec!["test", "a.txt"], ".jk extension"),
        (vec!["test", "--filter"], "requires a value"),
        (vec!["test", "--filter", ""], "must not be empty"),
        (vec!["test", "--timeout-ms", "0"], "1..=86400000"),
        (vec!["test", "--timeout-ms", "-1"], "1..=86400000"),
        (vec!["test", "--timeout-ms", "86400001"], "1..=86400000"),
        (vec!["test", "--list", "--list"], "more than once"),
        (vec!["test", "--parallel"], "Unknown test option"),
        (vec!["test", "--test-worker", "x"], "Invalid internal"),
    ] {
        failure(&work.cli(&args), message);
    }
    success(&work.cli(&["test", "--help"]));
}

fn failed_test(output: &std::process::Output, message: &str) {
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "{stderr}");
}

#[test]
fn test_context_injects_application_inputs_on_both_vms() {
    let work = Workspace::new();
    work.write(
        "inputs.jk",
        include_str!("../../examples/testing_inputs.jk"),
    );
    // Existing caller files and environment must not enter a test by accident.
    work.write("output.txt", "caller output");
    for backend in [vec![], vec!["--interpreter"]] {
        let output = work
            .command()
            .args(["test", "inputs.jk"])
            .args(backend)
            .env("APP_MODE", "caller environment")
            .output()
            .unwrap();
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("2 passed; 0 failed"));
        assert_eq!(
            std::fs::read_to_string(work.path().join("output.txt")).unwrap(),
            "caller output"
        );
        assert!(!work.path().join("prefix.txt").exists());
    }
}

#[test]
fn test_context_preserves_input_boundaries_and_freezes_snapshots() {
    let work = Workspace::new();
    work.write("boundaries.jk", r#"
import joky/testing
import joky/process
import joky/io
import joky/fs
#test
fn arguments() @suspends {
    testing.set_args([])!
    testing.set_args(["replaced"])!
    testing.set_args(["", "two words", "中文🙂", "--task-workers"])!
    assert(testing.set_args(["bad\0arg"]).err()!.kind == fs.ErrorKind.InvalidInput, "NUL rejected atomically")
    let args = process.args()!
    assert(args.len() == 4 && args[0] == "" && args[1] == "two words", "empty and spaced arguments")
    assert(args[2] == "中文🙂" && args[3] == "--task-workers", "Unicode and option-like arguments")
    assert(testing.set_args(["late"]).err()!.kind == fs.ErrorKind.Busy, "argument snapshot is frozen")
}
fn prepare_environment() @suspends {
    testing.set_env("EMPTY", "")!
    testing.set_env("VALUE", "first")!
    testing.set_env("VALUE", "second")!
    testing.set_env("REMOVED", "remove me")!
    testing.remove_env("REMOVED")!
    testing.remove_env("ABSENT")!
    assert(testing.set_env("bad=name", "bad").err()!.kind == fs.ErrorKind.InvalidInput, "bad key")
    assert(testing.set_env("VALUE", "bad\0value").err()!.kind == fs.ErrorKind.InvalidInput, "bad value")
}
#test
fn environment() @suspends {
    prepare_environment()
    assert(process.env("EMPTY")!! == "", "present empty environment value")
    assert(process.env("VALUE")!! == "second", "replacement survives rejected update")
    assert(process.env("REMOVED")!.is_none(), "removed environment value")
    assert(process.env("ABSENT")!.is_none(), "missing environment value")
    assert(testing.set_env("VALUE", "late").err()!.kind == fs.ErrorKind.Busy, "input snapshot is frozen")
    assert(testing.remove_env("EMPTY").err()!.kind == fs.ErrorKind.Busy, "removal is frozen")
}
#test
fn directory_and_stdin() @suspends {
    let directory = testing.temp_dir()!
    testing.set_stdin("replaced")!
    testing.set_stdin("\nfirst\r\n尾行")!
    assert(process.current_dir()! == directory, "working directory matches the test directory")
    assert(testing.temp_dir()! == directory, "setup directory remains available after freezing")
    assert(testing.set_stdin("late").err()!.kind == fs.ErrorKind.Busy, "stdin is frozen with process inputs")
    var input = io.stdin()!
    assert(io.stdin().is_err(), "reader is exclusive")
    assert(input.read_line(100)!! == "", "blank line")
    assert(input.read_line(100)!! == "first", "CRLF")
    assert(input.read_line(100)!! == "尾行", "unterminated Unicode tail")
    assert(input.read_line(100)!.is_none(), "EOF")
    input.close()!
}
#test
fn stdin_freezes_inputs_and_reopening_continues_the_cursor() @suspends {
    testing.set_stdin("ab\0c")!
    var first = fs.stdin()!
    assert(testing.set_env("LATE", "value").err()!.kind == fs.ErrorKind.Busy, "opening stdin freezes inputs")
    let byte = first.read(1)!
    assert(byte[0] == 97, "first byte")
    first.close()!
    var second = io.stdin()!
    assert(second.read_text_limited(3)! == "b\0c", "reopening does not rewind stdin")
    second.close()!
}
"#);
    for backend in [vec![], vec!["--interpreter"]] {
        success(
            &work
                .command()
                .args(["test", "boundaries.jk"])
                .args(backend)
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn test_context_cleans_directories_after_success_failure_and_timeout() {
    let work = Workspace::new();
    for (name, tail, expected) in [
        ("pass", "", 0),
        ("fail", "panic(\"intentional\")", 1),
        ("timeout", "while true {}", 1),
    ] {
        work.write(
            "cleanup.jk",
            &format!(
                r#"
import joky/testing
import joky/fs
#test
fn cleanup() @suspends {{
    let path = testing.temp_dir()!
    eprintln("TEST_DIR={{path}}")
    var options = fs.OpenOptions.default()
    options.write = true
    options.create = true
    var file = fs.open("marker.txt", options)!
    file.write_text("owned by the test")!
    {tail}
}}
"#
            ),
        );
        let output = work.cli(&["test", "cleanup.jk", "--nocapture", "--timeout-ms", "5000"]);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let directory = stderr
            .lines()
            .find_map(|line| line.strip_prefix("TEST_DIR="))
            .expect(&stderr);
        assert!(
            !std::path::Path::new(directory).exists(),
            "{name}: leaked {directory}"
        );
        assert!(!work.path().join("marker.txt").exists());
    }
}

#[test]
fn test_context_capability_is_absent_from_ordinary_programs() {
    let work = Workspace::new();
    work.write("ordinary.jk", r#"
import joky/testing
import joky/check.{assert}
import joky/fs
fn main() @suspends {
    assert(testing.set_env("VALUE", "test").err()!.kind == fs.ErrorKind.Unsupported, "explicit test capability required")
    assert(testing.set_args(["x"]).err()!.kind == fs.ErrorKind.Unsupported, "arguments require the capability")
    assert(testing.set_stdin("input").err()!.kind == fs.ErrorKind.Unsupported, "stdin requires the capability")
    assert(testing.temp_dir().err()!.kind == fs.ErrorKind.Unsupported, "directory requires the capability")
    assert(testing.stdout().err()!.kind == fs.ErrorKind.Unsupported, "stdout requires the capability")
    assert(testing.stderr().err()!.kind == fs.ErrorKind.Unsupported, "stderr requires the capability")
}

"#);
    #[cfg(unix)]
    work.assert_all_backends("ordinary.jk", &["--no-prelude", "--parallel"], b"", 0);
}

#[test]
fn test_output_snapshots_preserve_capture_and_nocapture_behavior() {
    let work = Workspace::new();
    work.write(
        "output_test.jk",
        include_str!("../../examples/testing_output.jk"),
    );
    for backend in [vec![], vec!["--interpreter"]] {
        for nocapture in [false, true] {
            let mut args = vec!["test", "output_test.jk"];
            args.extend(backend.clone());
            if nocapture {
                args.push("--nocapture");
            }
            let output = work.cli(&args);
            success(&output);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stdout.contains("2 passed; 0 failed"), "{stdout}");
            assert_eq!(stdout.contains("Hello, Joky!"), nocapture, "{stdout}");
            assert_eq!(stderr.contains("note: greeted"), nocapture, "{stderr}");
        }
    }
    work.write("failure.jk", "import joky/testing\n#test fn fails() @suspends { print(\"visible output\"); eprint(\"visible error\"); assert_eq(testing.stdout()!, \"wrong\") }");
    let output = work.cli(&["test", "failure.jk"]);
    failed_test(&output, "visible output");
    failed_test(&output, "visible error");
    failed_test(&output, "expected: \"wrong\"");
}

#[test]
fn test_output_snapshots_reject_overflow_without_stopping_output() {
    let work = Workspace::new();
    let chunk = "x".repeat(1024);
    work.write("limit.jk", &format!(r#"
import joky/testing
import joky/fs
#test fn limits() @suspends {{
    var n = 0
    while n < 1024 {{ print("{chunk}"); n += 1 }}
    assert_eq(testing.stdout()!.byte_length(), 1048576)
    print("")
    assert_eq(testing.stdout()!.byte_length(), 1048576)
    print("中")
    assert(testing.stdout().err()!.kind == fs.ErrorKind.LimitExceeded, "never return truncated output")
    assert(testing.stdout().err()!.kind == fs.ErrorKind.LimitExceeded, "overflow is sticky")
    eprint("stderr remains available")
    assert_eq(testing.stderr()!, "stderr remains available")
}}
"#));
    for backend in [vec![], vec!["--interpreter"]] {
        let mut args = vec!["test", "limit.jk"];
        args.extend(backend);
        success(&work.cli(&args));
    }
}
