//! Runtime configuration routing; parser edge cases live in generated-driver tests.
use super::*;

#[test]
fn startup_workers_run_build_and_nonexecution_policy() {
    let work = Workspace::new();
    work.write(
        "main.jk",
        "import joky/task fn main() @suspends { task.yield_now(); println(42) }",
    );
    for interpreter in [false, true] {
        for (flags, environment) in [
            (vec!["--parallel"], "3"),
            (vec!["--parallel", "--task-workers", "1"], "bad"),
            (vec!["--task-workers", "3", "--parallel"], "bad"),
        ] {
            let mut command = work.command();
            command
                .args(["run", "main.jk"])
                .args(flags)
                .env("JOKY_TASK_WORKERS", environment);
            if interpreter {
                command.arg("--interpreter");
            }
            let output = command.output().unwrap();
            success(&output);
            assert_eq!(output.stdout, b"42\n");
        }
    }
    for (flags, environment, message) in [
        (vec![], "3", "requires Parallel"),
        (vec!["--parallel"], "bad", "1..=256"),
        (vec!["--parallel", "--parallel"], "3", "more than once"),
        (vec!["--task-workers", "+3"], "3", "1..=256"),
    ] {
        let output = work
            .command()
            .args(["run", "main.jk"])
            .args(flags)
            .env("JOKY_TASK_WORKERS", environment)
            .output()
            .unwrap();
        failure(&output, message);
    }
    for args in [
        vec!["--help"],
        vec!["run", "--help"],
        vec!["--version"],
        vec!["fmt", "main.jk"],
    ] {
        success(
            &work
                .command()
                .args(args)
                .env("JOKY_TASK_WORKERS", "bad")
                .output()
                .unwrap(),
        );
    }
    for flags in [vec![], vec!["--parallel"], vec!["--task-workers", "3"]] {
        success(
            &work
                .command()
                .args(["build", "main.jk"])
                .args(&flags)
                .env("JOKY_TASK_WORKERS", "bad")
                .output()
                .unwrap(),
        );
        let mut command = Command::new(work.path().join("main"));
        command.env_remove("JOKY_TASK_WORKERS");
        let output = command.output().unwrap();
        success(&output);
        assert_eq!(output.stdout, b"42\n");
        if flags.is_empty() {
            for args in [vec![], vec!["--task-workers", "3"]] {
                let output = Command::new(work.path().join("main"))
                    .args(args)
                    .env("JOKY_TASK_WORKERS", "3")
                    .output()
                    .unwrap();
                assert!(!output.status.success());
                assert!(output.stdout.is_empty());
                assert!(String::from_utf8_lossy(&output.stderr)
                    .contains("requires a Parallel executable"));
            }
        }
    }
    // Mode selection still requires the transitive Parallel proof even with N=1.
    work.write("local.jk", "import joky/domain struct State {n:Int32} fn make(n:Int32)->State {State{n:n}} fn main() @suspends {let owner=domain.create(make,1)}");
    failure(&work.cli(&["build", "local.jk", "--parallel"]), "Parallel");
}

#[test]
fn startup_workers_project_wrapper_forwards_options_after_relocation() {
    let work = Workspace::new();
    work.write(
        "app/joky.toml",
        "[project]\nname='demo'\nentry='main.jk'\nresources=['message.txt']",
    );
    work.write("app/message.txt", "bundle");
    work.write("app/main.jk", "import joky/fs fn main() @suspends { var file = fs.open(\"message.txt\", fs.OpenOptions.new())!; println(file.read_text()!) }");
    success(&work.cli(&["build", "app", "--parallel"]));
    let moved = work.path().join("relocated");
    std::fs::rename(work.path().join("app/dist"), &moved).unwrap();
    std::fs::remove_dir_all(work.path().join("app")).unwrap();
    std::os::unix::fs::symlink(moved.join("demo"), work.path().join("launch")).unwrap();
    for count in ["1", "3"] {
        let output = Command::new(work.path().join("launch"))
            .current_dir("/")
            .env("PATH", "")
            .env("JOKY_TASK_WORKERS", "bad")
            .args(["--task-workers", count])
            .output()
            .unwrap();
        success(&output);
        assert_eq!(output.stdout, b"bundle\n");
    }
}
