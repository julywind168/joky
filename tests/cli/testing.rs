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
    work.write("timeout.jk", "import joky/task\n#test fn a_loop() { while true {} }\n#test fn b_sleep() @suspends { task.sleep_ms(60000) }\n#test fn c_pass() {}");
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
