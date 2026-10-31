//! Execute generated startup wrappers and observe real task worker identities.
use super::*;
use joky::{aot::AotCompiler, runtime::parallel::ExecutionMode, FunctionSignature};
use std::{path::Path, process::Command};

#[path = "../../../../tests/support/runtime.rs"]
mod runtime;
#[path = "../../../../tests/support/scratch.rs"]
mod scratch;

fn link(source: &str, directory: &Path, name: &str, object: Option<&Path>) -> std::path::PathBuf {
    let c = directory.join(format!("{name}.c"));
    let binary = directory.join(name);
    std::fs::write(&c, source).unwrap();
    let mut cc = Command::new("cc");
    cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&c)
        .arg("-o")
        .arg(&binary);
    if let Some(object) = object {
        cc.arg(object).arg(runtime::runtime_archive());
        if cfg!(target_os = "macos") {
            cc.args(["-liconv", "-lSystem"]);
        } else {
            cc.args([
                "-lgcc_s",
                "-lutil",
                "-lrt",
                "-lpthread",
                "-lm",
                "-ldl",
                "-lc",
            ]);
        }
    }
    let output = cc.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    binary
}

#[test]
fn startup_workers_native_parser_matches_run_policy() {
    let work = scratch::ScratchDir::new("startup-parser");
    // A parser-only entry can exercise 256 without allocating hundreds of threads.
    let driver =
        "static int joky_parallel_main(uint32_t workers) { printf(\"%u\\n\", workers); return 0; }";
    let binary = link(
        &startup(
            driver.into(),
            ExecutionMode::Parallel { workers: 1 },
            false,
            false,
        ),
        work.path(),
        "parser",
        None,
    );
    for value in [
        "1",
        "3",
        "256",
        "0003",
        "",
        "0",
        "257",
        "+3",
        "-3",
        " 3",
        "3 ",
        "3x",
        "99999999999999999999999999999",
        "３",
    ] {
        let expected = crate::cli::workers::parse(std::ffi::OsStr::new(value), "--task-workers");
        for environment in [false, true] {
            let mut command = Command::new(&binary);
            command.env_remove("JOKY_TASK_WORKERS");
            if environment {
                command.env("JOKY_TASK_WORKERS", value);
            } else {
                command.args(["--task-workers", value]);
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.success(),
                expected.is_ok(),
                "{value:?}: {output:?}"
            );
            if let Ok(count) = &expected {
                assert_eq!(output.stdout, format!("{count}\n").as_bytes());
            } else {
                assert!(output.stdout.is_empty());
                assert!(String::from_utf8_lossy(&output.stderr).contains("1..=256"));
            }
        }
    }
    for args in [
        vec!["--task-workers"],
        vec!["--task-workers", "1", "--task-workers", "2"],
        vec!["unknown"],
        vec!["--parallel"],
    ] {
        let output = Command::new(&binary)
            .env_remove("JOKY_TASK_WORKERS")
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let output = Command::new(&binary)
        .env_remove("JOKY_TASK_WORKERS")
        .args(["--", "--task-workers", "invalid", ""])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"1\n");
    // An invalid lower-priority environment value is not parsed.
    let output = Command::new(&binary)
        .env("JOKY_TASK_WORKERS", "bad")
        .args(["--task-workers", "3"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"3\n");
    let mode =
        crate::cli::workers::resolve(true, Some(3), Some(std::ffi::OsStr::new("bad"))).unwrap();
    assert_eq!(mode, ExecutionMode::Parallel { workers: 3 });
}

#[test]
fn startup_workers_same_aot_code_uses_selected_threads_and_preserves_embedding() {
    let work = scratch::ScratchDir::new("startup-workers");
    let mut compiler = AotCompiler::new("startup_workers").unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    // SAFETY: The linked scalar CPS callback below uses a mutex, retains no frame
    // or waker, and remains linked throughout every invocation and cleanup.
    unsafe {
        compiler
            .declare_parallel_host("observe", FunctionSignature::new(vec![], Type::Int32), 0)
            .unwrap();
    }
    compiler
        .compile_module(
            &joky::parser::parse(
                r#"
        extern fn observe() @suspends -> Int32
        fn root() @suspends -> Int32 {
            let (a, b, c) = join { | observe() | observe() | observe() }
            a + b + c
        }
    "#,
            )
            .unwrap(),
        )
        .unwrap();
    let entry = compiler.parallel_entry("root").unwrap();
    let object = work.path().join("program.o");
    compiler.write_object(&object).unwrap();
    // Each assigned worker gets one bit, independent of scheduling order or timing.
    // Three child tasks visit all three worker queues with N=3; N=1 reuses one.
    let probe = r#"
#include <pthread.h>
extern void joky_cps_set_result(JokyCpsFrame *, uint64_t);
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_t threads[3];
static unsigned count = 0;
int32_t observe(JokyCpsFrame *frame) {
    pthread_mutex_lock(&lock);
    unsigned i = 0;
    while (i < count && !pthread_equal(threads[i], pthread_self())) ++i;
    if (i == count) { if (count == 3) abort(); threads[count++] = pthread_self(); }
    pthread_mutex_unlock(&lock);
    joky_cps_set_result(frame, 1u << i);
    return 0;
}
"#;
    let driver = entry.standalone_driver_v1(64).unwrap();
    let binary = link(
        &format!(
            "{}\n{probe}",
            startup(driver, ExecutionMode::Parallel { workers: 3 }, false, true)
        ),
        work.path(),
        "parallel",
        Some(&object),
    );
    for (argument, environment, expected) in [
        (None, None, 7),
        (Some("1"), None, 3),
        (Some("3"), None, 7),
        (None, Some("1"), 3),
        (Some("3"), Some("bad"), 7),
    ] {
        let mut command = Command::new(&binary);
        command.env_remove("JOKY_TASK_WORKERS");
        if let Some(value) = argument {
            command.args(["--task-workers", value]);
        }
        if let Some(value) = environment {
            command.env("JOKY_TASK_WORKERS", value);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    // The old public generator and runtime ABI ignore CLI environment policy.
    let legacy = link(
        &format!(
            "#include <stdlib.h>\n{}\n{probe}",
            entry.standalone_main_v1(1, 64).unwrap()
        ),
        work.path(),
        "embedding",
        Some(&object),
    );
    let output = Command::new(legacy)
        .env("JOKY_TASK_WORKERS", "3")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
}
