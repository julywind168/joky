use super::parity::{assert_result, run_bounded, Package};
use super::*;

#[cfg(unix)]
#[test]
fn path_and_process_match_in_cold_warm_legacy_and_aot() {
    use std::os::unix::fs::PermissionsExt;
    let package = Package::new("path-process", &[]);
    fs::create_dir_all(package.root.join("tools")).unwrap();
    fs::create_dir_all(package.root.join("work")).unwrap();
    let helper = package.root.join("tools/report");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s:%s' \"$JOKY_CHILD_VALUE\" \"$PWD\"\n",
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    // macOS /var can be a symlink; the shell exposes the physical cwd.
    let root = package.root.canonicalize().unwrap();
    let process = include_str!("../fixtures/process.jk").replace("@ROOT@", root.to_str().unwrap());
    let path = include_str!("../fixtures/path.jk").replace("fn main()", "pub fn exercise()");
    fs::write(package.root.join("src/paths.jk"), path).unwrap();
    let process = format!(
        "import paths\n{}",
        process.replace(
            "    println(\"process ok\")",
            "    paths.exercise()\n    println(\"process ok\")"
        )
    );
    fs::write(package.root.join("src/main.jk"), process).unwrap();
    let expected = "outpath ok\nprocess ok\n";
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", expected, None);
    package.check_cached(expected, None, &[]);
}

#[cfg(unix)]
#[test]
fn process_race_cancellation_reaps_before_scope_shutdown() {
    let package = Package::new("process-cancellation", &[]);
    let pidfile = package.root.join("pid");
    let source = r#"
import joky/process
import joky/file
import joky/time
fn main() effects { process, file, time } {
    let winner = race {
        | {
            let command = process.command("/bin/sh").with_args(List("-c", "printf '%s' $$ > '@PID@'; exec /bin/sleep 60"))
            let _ = process.capture(command)
            "child"
        }
        | {
            loop {
                match file.read("@PID@") {
                    Ok(pid) => if !pid.is_empty() { break } else {},
                    Err(_) => {}
                }
                time.sleep(1ms)
            }
            "cancelled"
        }
    }
    println(winner)
}
"#.replace("@PID@", pidfile.to_str().unwrap());
    fs::write(package.root.join("src/main.jk"), source).unwrap();
    package.check_with("cancelled\n", None, &[], |mode, _| {
        let pid = fs::read_to_string(&pidfile).unwrap();
        let alive = Command::new("/bin/kill")
            .args(["-0", &pid])
            .output()
            .unwrap();
        assert!(
            !alive.status.success(),
            "{mode}: child {pid} survived scope cleanup"
        );
        fs::remove_file(&pidfile).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn pipeline_has_parity_across_cold_warm_legacy_and_aot() {
    let package = Package::new(
        "pipeline",
        &[("main.jk", include_str!("../fixtures/pipeline.jk"))],
    );
    let legacy = run_bounded(
        Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .env("JOKY_TEST_RESOURCE_REPORT", "1")
            .args(["run", "--legacy"]),
    );
    assert_result(&legacy, "legacy", "INHERITEDpipeline ok\n", None);
    package.check_cached("INHERITEDpipeline ok\n", None, &[]);
}

#[cfg(unix)]
#[test]
fn pipeline_cancellation_reaps_all_stages_before_scope_shutdown() {
    let package = Package::new("pipeline-cancellation", &[]);
    let first = package.root.join("first");
    let last = package.root.join("last");
    let source = r#"
import joky/process
import joky/file
import joky/time
fn ready(path: String) -> Bool effects { file } {
    match file.read(path) { Ok(value) => !value.is_empty(), Err(_) => false }
}
fn main() effects { process, file, time } {
    let winner = race {
        | {
            let first = process.command("/bin/sh").with_args(List("-c", "printf '%s' $$ > '@FIRST@'; exec /bin/sleep 60"))
            let last = process.command("/bin/sh").with_args(List("-c", "printf '%s' $$ > '@LAST@'; exec /bin/sleep 60"))
            let _ = process.capture(first.pipe(last))
            "child"
        }
        | {
            while !(ready("@FIRST@") && ready("@LAST@")) { time.sleep(1ms) }
            "cancelled"
        }
    }
    println(winner)
}
"#.replace("@FIRST@", first.to_str().unwrap()).replace("@LAST@", last.to_str().unwrap());
    fs::write(package.root.join("src/main.jk"), source).unwrap();
    package.check_with("cancelled\n", None, &[], |mode, _| {
        for path in [&first, &last] {
            let pid = fs::read_to_string(path).unwrap();
            let alive = Command::new("/bin/kill")
                .args(["-0", &pid])
                .output()
                .unwrap();
            assert!(
                !alive.status.success(),
                "{mode}: stage {pid} survived cleanup"
            );
            fs::remove_file(path).unwrap();
        }
    });
}

#[cfg(unix)]
#[test]
fn process_generic_cache_distinguishes_callers_with_identical_public_abis() {
    let package = Package::new("process-generic-callers", &[]);
    let source = "import joky/process\nfn main() effects { process } { let out = process.capture(process.command(\"/bin/echo\").with_args(List(\"ok\")))!; println(out.stdout.to_string()!) }\n";
    for name in ["first.jk", "second.jk"] {
        fs::write(package.root.join("src").join(name), source).unwrap();
    }
    for name in ["first.jk", "first.jk", "second.jk", "second.jk", "first.jk"] {
        let run = run_bounded(
            Command::new(env!("CARGO_BIN_EXE_joky"))
                .current_dir(&package.root)
                .env("JOKY_TEST_RESOURCE_REPORT", "1")
                .arg("run")
                .arg(package.root.join("src").join(name)),
        );
        assert_result(&run, name, "ok\n\n", None);
    }
}
