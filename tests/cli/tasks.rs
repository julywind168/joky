//! Suspending entries and structured task lifecycles.
use super::*;

#[test]
fn local_task_width_is_independent_of_recursion_depth() {
    let work = Workspace::new();
    work.write(
        "width.jk",
        r#"
        import joky/task
        fn fib(n: Int32) @suspends -> Int32 {
            if n < 2 { task.yield_now(); return n }
            let (a, b) = join { fib(n - 1), fib(n - 2) }
            a + b
        }
        fn main() @suspends {
            scope {
                var n = 0
                while n < 1100 {
                    branch { task.yield_now(); println(n) }
                    n += 1
                }
            }
            println(fib(13))
        }
    "#,
    );
    let mut expected: String = (0..1100).map(|n| format!("{n}\n")).collect();
    expected.push_str("233\n");
    work.assert_all_backends("width.jk", &[], expected.as_bytes(), 0);
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
        branch { report_int32(id); yield_now(); report_int32(id + 1) }
        yield_now()
        report_int32(id + 2)
        yield_now()
        report_int32(id + 3)
    }
}
fn relay(id: Int32) @suspends { helper(id) }
fn main() @suspends {
    relay(10)
    join { relay(20), relay(30) }
    race { relay(40), relay(50) }
}
"#,
    );
    work.assert_all_backends(
        "rounds.jk",
        &[],
        b"10\n12\n11\n13\n20\n30\n22\n32\n21\n31\n23\n33\n40\n50\n42\n52\n41\n51\n43\n",
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
            branch { branch { report_int32(3) }; yield_now(); report_int32(2) }
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
fn say() @suspends { yield_now(); report_int32(1) }
fn other() @suspends { report_int32(2) }
fn main() @suspends {
    let f = say
    let text = "hello"
    scope {
        branch { f() }
        branch { report_int32(int32(text.byte_length())) }
        branch { let f = other; f() }
    }
}
"#,
    );
    work.assert_all_backends("captures.jk", &[], b"5\n2\n1\n", 0);
}

#[test]
#[cfg(unix)]
fn suspending_cli_entries_match_jit_interpreter_and_standalone() {
    let work = Workspace::new();
    work.write(
        "async_main.jk",
        include_str!("../../examples/async_main.jk"),
    );
    work.assert_all_backends("async_main.jk", &[], b"42\n", 0);
    for (source, code, stdout) in [
        ("fn main() @suspends {}", 0, ""),
        ("fn unused() @suspends {} struct Unused {} impl Unused { fn f(self) @suspends {} } fn main() {}", 0, ""),
        ("fn callback() @suspends -> Int32 { 42 } fn apply(f: fn() @suspends -> Int32) @suspends -> Int32 { f() } fn main() @suspends -> Int32 { apply(callback) }", 42, ""),
        (r#"
            import joky/task.{yield_now, sleep}
            fn main() @suspends -> Int32 {
                let first = 100
                let second = 150
                yield_now()
                sleep(1ms)
                first + second + 49
            }
        "#, 43, ""),
        (r#"
            import joky/task
            import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
            trait Worker { fn worker(self) @suspends }
            impl Worker for Int32 {
                fn worker(self) @suspends {
                    report_int32(self)
                    task.yield_now()
                    task.yield_now()
                    report_int32(self + 10)
                }
            }
            fn worker(n: Int32) @suspends { n.worker() }
            fn winner() @suspends { task.yield_now(); report_int32(99) }
            fn loser() @suspends { task.sleep(1ms); report_int32(-1) }
            fn main() @suspends {
                let f: fn(Int32) @suspends -> Unit = worker
                join { f(1), f(2) }
                race { winner(), loser() }
                task.sleep(1ms)
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
fn suspending_owners_execute_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/suspending_owners.jk").replace(
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
fn async_owner_borrows_short_segments_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "async_owner_borrows.jk",
        include_str!("../fixtures/async_owner_borrows.jk"),
    );
    work.assert_native("async_owner_borrows.jk", &[], b"", 0);
}

#[test]
fn async_owner_views_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "async_owner_views.jk",
        include_str!("../fixtures/async_owner_views.jk"),
    );
    work.assert_native("async_owner_views.jk", &[], b"", 0);
}

#[test]
fn async_owner_views_drop_in_order_in_standalone_aot() {
    let work = Workspace::new();
    let source = include_str!("../fixtures/async_owner_drop.jk").replace(
        "extern fn record_drop(id: Int32)",
        "fn record_drop(id: Int32) { println(id) }",
    );
    work.write("async_owner_drop.jk", &source);
    work.assert_native("async_owner_drop.jk", &[], b"2\n42\n1\n2\n3\n1\n", 0);
}

#[test]
#[cfg(unix)]
fn task_pool_cli_explicit_mode_and_standalone_driver() {
    let work = Workspace::new();
    work.write(
        "parallel.jk",
        r#"
        import joky/task
        fn leaf(n: Int32) @suspends -> Int32 { task.sleep(2ms); n }
        fn main() @suspends -> Int32 {
            scope { branch { task.yield_now() }; task.sleep(1ms) }
            let (a, b) = join { leaf(20), leaf(22) }
            let winner = race { leaf(0), leaf(0) }
            a + b + winner
        }
    "#,
    );
    for workers in ["1", "2"] {
        for interpreter in [false, true] {
            let mut command = work.command();
            command.args(["run", "--task-workers", workers]);
            if interpreter {
                command.arg("--interpreter");
            }
            let output = command.arg("parallel.jk").output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(42),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    let build = work
        .command()
        .args([
            "build",
            "--task-workers",
            "2",
            "parallel.jk",
            "-o",
            "parallel",
        ])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&build);
    assert_eq!(
        Command::new(work.path().join("parallel"))
            .output()
            .unwrap()
            .status
            .code(),
        Some(42)
    );
    work.write(
        "local.jk",
        "import joky/domain fn main() @suspends { let timer = domain.timer() }",
    );
    for route in ["run", "build"] {
        let rejected = work
            .command()
            .args([route, "--task-workers", "1", "local.jk"])
            .output()
            .unwrap();
        failure(&rejected, "Parallel");
        assert!(rejected.stdout.is_empty());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("Parallel"));
    }
    for args in [
        vec!["0"],
        vec!["257"],
        vec!["x"],
        vec!["1", "--task-workers", "2"],
    ] {
        let output = work
            .command()
            .arg("run")
            .arg("--task-workers")
            .args(args)
            .arg("parallel.jk")
            .output()
            .unwrap();
        failure(&output, "--task-workers");
        assert!(String::from_utf8_lossy(&output.stderr).contains("--task-workers"));
    }
}

#[test]
#[cfg(unix)]
fn task_parallel_stdout_preserves_writes_and_reports_errors_on_all_backends() {
    use std::{
        os::{fd::OwnedFd, unix::net::UnixDatagram},
        process::Stdio,
    };
    let work = Workspace::new();
    // Exceed typical pipe atomic-write sizes: stdout's whole-call lock, not
    // short OS writes, must prevent fragments of these records interleaving.
    let payload = "中".repeat(2048);
    let source = include_str!("../fixtures/parallel_stdout.jk").replace("__PAYLOAD__", &payload);
    work.write("output.jk", &source);
    let mut expected: Vec<_> = (0..4)
        .flat_map(|id| (0..8).map(move |item| (id, item)))
        .map(|(id, item)| format!("worker={id};item={item};{payload}\0🙂"))
        .collect();
    expected.sort();
    let build = work
        .command()
        .args(["build", "--task-workers", "2", "output.jk", "-o", "output"])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&build);
    for backend in ["jit", "interpreter", "aot"] {
        for workers in ["1", "3"] {
            let command = || {
                if backend == "aot" {
                    let mut cmd = Command::new(work.path().join("output"));
                    cmd.args(["--task-workers", workers]);
                    cmd
                } else {
                    let mut cmd = work.command();
                    cmd.args(["run", "--task-workers", workers]);
                    if backend == "interpreter" {
                        cmd.arg("--interpreter");
                    }
                    cmd.arg("output.jk");
                    cmd
                }
            };
            let output = command().output().unwrap();
            success(&output);
            assert!(output.stderr.is_empty(), "{backend}: {:?}", output.stderr);
            let text = String::from_utf8(output.stdout).unwrap();
            let mut lines = text.lines();
            assert_eq!(lines.next(), Some("42"));
            let mut records: Vec<_> = lines.map(str::to_owned).collect();
            records.sort();
            assert_eq!(records, expected, "{backend}");
            // An unconnected datagram socket fails writes without SIGPIPE or a
            // pipe-reader race. Invalid/read-only fds can be ignored by Rust stdout.
            let failed = command()
                .stdout(Stdio::from(OwnedFd::from(UnixDatagram::unbound().unwrap())))
                .output()
                .unwrap();
            failure(&failed, "Destination address required");
        }
    }
}

#[test]
fn task_parallel_tcp_handoff_runs_in_standalone_aot() {
    let work = Workspace::new();
    for (name, source) in [
        ("parallel_tcp", include_str!("../fixtures/parallel_tcp.jk")),
        (
            "scoped_borrow_tcp",
            include_str!("../fixtures/scoped_borrow_tcp.jk"),
        ),
    ] {
        work.write(&format!("{name}.jk"), source);
        let build = work
            .command()
            .args([
                "build",
                "--task-workers",
                "3",
                &format!("{name}.jk"),
                "-o",
                name,
            ])
            .env("JOKY_RUNTIME", runtime_archive())
            .output()
            .unwrap();
        success(&build);
        for workers in ["1", "3"] {
            success(
                &Command::new(work.path().join(name))
                    .args(["--task-workers", workers])
                    .output()
                    .unwrap(),
            );
        }
    }
}

#[test]
fn task_parallel_function_values_runs_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "parallel_functions.jk",
        include_str!("../fixtures/parallel_functions.jk"),
    );
    let build = work
        .command()
        .args([
            "build",
            "--task-workers",
            "3",
            "parallel_functions.jk",
            "-o",
            "parallel_functions",
        ])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&build);
    success(
        &Command::new(work.path().join("parallel_functions"))
            .output()
            .unwrap(),
    );
}

#[test]
fn task_parallel_control_bridge_runs_in_standalone_aot() {
    for source in [
        include_str!("../fixtures/parallel_control.jk"),
        include_str!("../fixtures/scoped_borrow_vm.jk"),
        include_str!("../fixtures/scoped_borrow_admission.jk"),
        include_str!("../fixtures/scoped_borrow_outbox.jk"),
        include_str!("../fixtures/scoped_borrow_fs.jk"),
        include_str!("../fixtures/exclusive_mailbox.jk"),
    ] {
        let work = Workspace::new();
        work.write("parallel_control.jk", source);
        let build = work
            .command()
            .args([
                "build",
                "--task-workers",
                "3",
                "parallel_control.jk",
                "-o",
                "parallel_control",
            ])
            .env("JOKY_RUNTIME", runtime_archive())
            .output()
            .unwrap();
        success(&build);
        for workers in ["1", "3"] {
            success(
                &Command::new(work.path().join("parallel_control"))
                    .current_dir(work.path())
                    .args(["--task-workers", workers])
                    .output()
                    .unwrap(),
            );
        }
    }
}

#[test]
fn task_parallel_domain_transactions_run_in_standalone_aot() {
    let work = Workspace::new();
    for (name, source) in [
        (
            "parallel_domains.jk",
            include_str!("../fixtures/parallel_domains.jk"),
        ),
        (
            "state_domain_when.jk",
            include_str!("../fixtures/state_domain_when.jk"),
        ),
        (
            "state_domain_paths.jk",
            include_str!("../fixtures/state_domain_paths.jk"),
        ),
        (
            "state_domain_creation.jk",
            include_str!("../fixtures/state_domain_creation.jk"),
        ),
    ] {
        work.write(name, source);
    }
    let build = work
        .command()
        .args([
            "build",
            "--task-workers",
            "3",
            "parallel_domains.jk",
            "-o",
            "parallel_domains",
        ])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&build);
    success(
        &Command::new(work.path().join("parallel_domains"))
            .current_dir(work.path())
            .output()
            .unwrap(),
    );
}

#[test]
fn isolate_and_reservation_run_in_standalone_aot() {
    let work = Workspace::new();
    work.write("isolate.jk", include_str!("../fixtures/isolate.jk"));
    work.write(
        "reservation.jk",
        include_str!("../../examples/reservation.jk"),
    );
    work.write(
        "examples/reservation_logic.jk",
        include_str!("../../examples/reservation_logic.jk"),
    );
    for source in ["isolate", "reservation"] {
        for workers in ["1", "3"] {
            let output = format!("{source}-{workers}");
            success(
                &work
                    .command()
                    .args([
                        "build",
                        "--task-workers",
                        workers,
                        &format!("{source}.jk"),
                        "-o",
                        &output,
                    ])
                    .env("JOKY_RUNTIME", runtime_archive())
                    .output()
                    .unwrap(),
            );
            success(
                &Command::new(work.path().join(output))
                    .current_dir(work.path())
                    .output()
                    .unwrap(),
            );
        }
    }
}

#[test]
fn parallel_borrowed_iteration_runs_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "borrowed.jk",
        include_str!("../fixtures/parallel_borrowed_iteration.jk"),
    );
    work.assert_native("borrowed.jk", &["--task-workers", "3"], b"", 0);
}

#[test]
#[cfg(unix)]
fn parallel_owned_closures_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "closures.jk",
        include_str!("../fixtures/parallel_owned_closures.jk"),
    );
    work.assert_native("closures.jk", &["--parallel"], b"", 0);
    let output = Command::new(work.path().join("closures"))
        .args(["--task-workers", "3"])
        .output()
        .unwrap();
    success(&output);
    assert!(output.stdout.is_empty());
}

#[test]
fn parallel_drop_values_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "drops.jk",
        include_str!("../fixtures/parallel_drop_values.jk"),
    );
    let output = work.cli(&["build", "drops.jk", "--task-workers", "3"]);
    success(&output);
    let output = Command::new(work.path().join("drops")).output().unwrap();
    success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines: Vec<_> = stdout.lines().collect();
    lines.sort();
    assert_eq!(lines, ["a", "array", "b", "closure", "loser", "winner"]);
}

#[test]
fn owned_dyn_values_execute_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "objects.jk",
        include_str!("../fixtures/owned_dyn_values.jk"),
    );
    success(&work.cli(&["build", "objects.jk", "--parallel"]));
    for workers in ["1", "3"] {
        let output = Command::new(work.path().join("objects"))
            .args(["--task-workers", workers])
            .output()
            .unwrap();
        success(&output);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let mut lines: Vec<_> = stdout.lines().collect();
        lines.sort();
        assert_eq!(lines, ["loser", "owned"]);
    }
}

#[test]
fn parallel_explicit_arena_executes_in_standalone_aot() {
    let work = Workspace::new();
    work.write(
        "regions.jk",
        include_str!("../fixtures/parallel_explicit_arena.jk"),
    );
    success(&work.cli(&["build", "regions.jk", "--parallel"]));
    for workers in ["1", "3"] {
        success(
            &Command::new(work.path().join("regions"))
                .args(["--task-workers", workers])
                .output()
                .unwrap(),
        );
    }
}

#[path = "../fixtures/migration_session.rs"]
mod migration_fixture;
#[test]
fn migration_sessions_run_in_one_standalone_aot_binary() {
    let work = Workspace::new();
    work.write("migration.jk", &migration_fixture::source());
    let build = work
        .command()
        .args(["build", "--parallel", "migration.jk", "-o", "migration"])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&build);
    for workers in ["1", "3"] {
        success(
            &Command::new(work.path().join("migration"))
                .args(["--task-workers", workers])
                .output()
                .unwrap(),
        );
    }
}
