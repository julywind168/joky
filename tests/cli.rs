//! End-to-end checks of the installed-style command-line interface.
use std::{
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "joky-cli-{}-{unique}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, source: &str) {
        std::fs::write(self.0.join(name), source).unwrap();
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
        command.current_dir(&self.0);
        command
    }
    fn cli(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn assert_no_build_files(&self) {
        assert!(std::fs::read_dir(&self.0).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".joky-build-")));
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn failure(output: &Output, message: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.starts_with("error: ") && error.contains(message),
        "{error}"
    );
    assert!(!error.contains("panicked"), "{error}");
}

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
    work.write("hello.jk", include_str!("../examples/hello.jk"));
    let output = work
        .command()
        .args(["run", "--interpreter", "hello.jk"])
        .env("PATH", "")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"42\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn run_needs_no_c_compiler_and_accepts_paths_after_double_dash() {
    let work = Workspace::new();
    work.write("-hello world.jk", include_str!("../examples/hello.jk"));
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

#[test]
fn run_and_build_report_source_and_entry_errors_consistently() {
    let work = Workspace::new();
    for (source, message) in [
        ("fn helper() {}", "Missing entry point"),
        ("fn main(x: Int32) {}", "Entry point must be"),
        ("fn main() -> Bool { true }", "Entry point must be"),
        ("fn main() @suspends {}", "does not support @suspends"),
        (
            "extern fn other() fn main() { other() }",
            "Unknown CLI host function",
        ),
        (
            "extern fn print_int(x: Bool) fn main() {}",
            "Expected extern fn print_int",
        ),
        ("fn main() {\n#\n}", "Line 2"),
        ("fn main() { let x: Bool = 1 }", "Type annotation mismatch"),
        (
            "fn main() { let x: Int32 = \"text\" }",
            "Type annotation mismatch",
        ),
        (
            "fn main() -> Int32 { true }",
            "Return type mismatch for `main`",
        ),
        (
            "fn __joky_cps_slots() {} fn main() {}",
            "Reserved runtime function",
        ),
    ] {
        work.write("bad.jk", source);
        for command in ["run", "build"] {
            let output = work.cli(&[command, "bad.jk"]);
            failure(&output, message);
            assert!(String::from_utf8_lossy(&output.stderr).contains("bad.jk:"));
        }
        assert!(!work.0.join("bad").exists());
        work.assert_no_build_files();
    }
}

#[test]
#[cfg(unix)]
fn built_program_matches_jit_output_and_avoids_native_symbol_collisions() {
    let work = Workspace::new();
    work.write(
        "hello world.jk",
        r#"
        extern fn print_int(value: Int32)
        fn printf(x: Int32) -> Int32 { x + 1 }
        fn pick(unit: Unit, value: Int32) -> Int32 { unit value }
        fn main() { print_int(pick({}, printf(41))) }
    "#,
    );
    let run = work.cli(&["run", "hello world.jk"]);
    success(&run);
    assert_eq!(run.stdout, b"42\n");
    let build = work.cli(&["build", "hello world.jk"]);
    success(&build);
    assert!(build.stdout.is_empty());
    assert!(String::from_utf8_lossy(&build.stderr).contains("Built hello world"));
    // The artifact remains runnable without either its source or the CLI.
    std::fs::remove_file(work.0.join("hello world.jk")).unwrap();
    let native = Command::new(work.0.join("hello world"))
        .env("PATH", "")
        .output()
        .unwrap();
    success(&native);
    assert_eq!(native.stdout, run.stdout);
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn explicit_output_and_main_exit_codes_match_run() {
    let work = Workspace::new();
    for (source, code) in [
        ("fn main() {}", 0),
        ("fn main() -> Int32 { 7 }", 7),
        ("fn main() -> Int32 { 263 }", 7),
        ("fn main() -> Int32 { 0 - 1 }", 255),
        (
            "fn print_int(x: Int32) -> Int32 { x } fn main() -> Int32 { print_int(8) }",
            8,
        ),
    ] {
        work.write("exit.jk", source);
        let run = work.cli(&["run", "exit.jk"]);
        assert_eq!(run.status.code(), Some(code));
        assert!(run.stdout.is_empty() && run.stderr.is_empty());
        success(&work.cli(&["build", "--output", "custom program", "exit.jk"]));
        let native = Command::new(work.0.join("custom program"))
            .output()
            .unwrap();
        assert_eq!(native.status.code(), Some(code));
        assert!(native.stdout.is_empty() && native.stderr.is_empty());
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn failed_build_preserves_source_and_existing_output() {
    use std::os::unix::{fs::symlink, fs::PermissionsExt};
    let work = Workspace::new();
    let source = "fn main() {}";
    work.write("safe.jk", source);
    work.write("existing", "keep this output");
    failure(
        &work.cli(&["build", "safe.jk", "-o", "./safe.jk"]),
        "must not replace the source",
    );
    symlink(work.0.join("safe.jk"), work.0.join("alias")).unwrap();
    failure(
        &work.cli(&["build", "safe.jk", "-o", "alias"]),
        "must not replace the source",
    );
    assert_eq!(
        std::fs::read_to_string(work.0.join("safe.jk")).unwrap(),
        source
    );
    let output = work
        .command()
        .args(["build", "safe.jk", "-o", "existing"])
        .env("PATH", "")
        .output()
        .unwrap();
    failure(&output, "Cannot start C compiler 'cc'");
    work.assert_no_build_files();
    work.write("cc", "#!/bin/sh\necho 'test linker failure' >&2\nexit 1\n");
    std::fs::set_permissions(work.0.join("cc"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = work
        .command()
        .args(["build", "safe.jk", "-o", "existing"])
        .env("PATH", &work.0)
        .output()
        .unwrap();
    failure(&output, "test linker failure");
    work.write("safe.jk", "fn main() { unknown() }");
    failure(
        &work.cli(&["build", "safe.jk", "-o", "existing"]),
        "Undeclared function",
    );
    assert_eq!(
        std::fs::read_to_string(work.0.join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[cfg(unix)]
fn runtime_archive() -> PathBuf {
    // Cargo emits the current library's static archive beside the test binary,
    // including on a clean `cargo test` without a preceding `cargo build`.
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    std::fs::read_dir(deps)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("libjoky-")
                && path.extension().is_some_and(|ext| ext == "a")
        })
        .max_by_key(|path| path.metadata().unwrap().modified().unwrap())
        .expect("Cargo must build the runtime static archive")
}

#[test]
#[cfg(unix)]
fn vm_standard_library_runs_in_jit_and_standalone_executable() {
    let work = Workspace::new();
    work.write("vm.jk", include_str!("../examples/vm.jk"));
    let jit = work.cli(&["run", "vm.jk"]);
    success(&jit);
    assert_eq!(jit.stdout, b"42\n100\n");
    let built = work
        .command()
        .args(["build", "vm.jk"])
        .env("JOKY_RUNTIME", runtime_archive())
        .output()
        .unwrap();
    success(&built);
    std::fs::remove_file(work.0.join("vm.jk")).unwrap();
    let native = Command::new(work.0.join("vm"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&native);
    assert_eq!(native.stdout, jit.stdout);
    assert!(native.stderr.is_empty());
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn standard_library_reports_failures_and_keeps_the_previous_module() {
    let work = Workspace::new();
    work.write("errors.jk", r#"
        import joky/vm
        extern fn print_int(value: Int32)
        fn source() -> String { "fn value() -> Int32 { 42 }" }
        fn identity(text: String) -> String { text }
        fn main() -> Int32 {
            let engine = vm.create()
            print_int(vm.load_source(engine, identity(source())))
            print_int(vm.call_int32(engine, "value"))
            print_int(vm.reload_source(engine, "fn value() -> Bool { 42 }"))
            print_int(vm.error_code())
            vm.print_error()
            print_int(vm.call_int32(engine, "value"))
            print_int(vm.call_int32(engine, "missing"))
            print_int(vm.error_code())
            vm.print_error()
            print_int(vm.load_source(engine, "import joky/vm fn nested() -> Int32 { let child = vm.create() vm.close(child) }"))
            print_int(vm.call_int32(engine, "nested"))
            print_int(vm.close(engine))
            print_int(vm.close(engine))
            vm.error_code()
        }
    "#);
    let jit = work.cli(&["run", "errors.jk"]);
    assert_eq!(jit.status.code(), Some(1));
    assert_eq!(jit.stdout, b"0\n42\n1\n1\n42\n0\n1\n0\n0\n0\n1\n");
    let stderr = String::from_utf8_lossy(&jit.stderr);
    assert!(
        stderr.contains("Return type mismatch") && stderr.contains("not found"),
        "{stderr}"
    );
    success(
        &work
            .command()
            .args(["build", "errors.jk"])
            .env("JOKY_RUNTIME", runtime_archive())
            .output()
            .unwrap(),
    );
    let native = Command::new(work.0.join("errors")).output().unwrap();
    assert_eq!(native.status.code(), jit.status.code());
    assert_eq!(native.stdout, jit.stdout);
    assert_eq!(native.stderr, jit.stderr);
}

#[test]
fn imports_and_standard_library_calls_are_checked_before_execution() {
    let work = Workspace::new();
    for (source, message) in [
        (
            "import joky/missing fn main() {}",
            "Unknown built-in module",
        ),
        (
            "import joky/vm import joky/vm fn main() {}",
            "Duplicate import",
        ),
        ("fn main() { let h = vm.create() }", "Undeclared function"),
        (
            "import joky/vm fn main() { vm.unknown() }",
            "Undeclared function",
        ),
        (
            "import joky/vm fn main() { let h = vm.create() vm.load_source(h, 42) }",
            "Argument type mismatch",
        ),
        (
            "import joky/vm fn main() { vm.close(1) }",
            "Argument type mismatch",
        ),
        (
            "import joky/vm fn main() { vm.call_int32() }",
            "zero to two Int32 arguments",
        ),
        (
            "import joky/vm fn main() { let h = vm.create() vm.register_int32(h, \"missing\", missing) }",
            "not defined in this module",
        ),
        (
            "import joky/vm fn host(value: String) -> Int32 { 1 } fn main() { let h = vm.create() vm.register_int32(h, \"host\", host) }",
            "must be a synchronous Int32 function",
        ),
        (
            "fn joky_vm_create() {} fn main() {}",
            "Reserved runtime function",
        ),
    ] {
        work.write("bad.jk", source);
        for command in ["run", "build"] {
            failure(&work.cli(&[command, "bad.jk"]), message);
        }
    }
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn missing_standard_runtime_has_an_actionable_error_and_preserves_output() {
    let work = Workspace::new();
    work.write("vm.jk", include_str!("../examples/vm.jk"));
    work.write("existing", "keep this output");
    let output = work
        .command()
        .args(["build", "vm.jk", "-o", "existing"])
        .env("JOKY_RUNTIME", work.0.join("missing.a"))
        .output()
        .unwrap();
    failure(&output, "JOKY_RUNTIME does not name a runtime archive");
    assert_eq!(
        std::fs::read_to_string(work.0.join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn installed_compiler_finds_its_runtime_without_repository_paths() {
    let work = Workspace::new();
    let bin = work.0.join("installation/bin");
    let lib = work.0.join("installation/lib/joky");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_joky"), bin.join("joky")).unwrap();
    std::fs::copy(runtime_archive(), lib.join("libjoky.a")).unwrap();
    work.write("vm.jk", include_str!("../examples/vm.jk"));
    let output = Command::new(bin.join("joky"))
        .current_dir(&work.0)
        .args(["build", "vm.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    std::fs::remove_dir_all(work.0.join("installation")).unwrap();
    std::fs::remove_file(work.0.join("vm.jk")).unwrap();
    let program = Command::new(work.0.join("vm"))
        .env("PATH", "")
        .output()
        .unwrap();
    success(&program);
    assert_eq!(program.stdout, b"42\n100\n");
}
