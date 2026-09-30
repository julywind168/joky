//! CLI subprocess fixtures and shared success-path execution checks.
#[cfg(unix)]
use super::runtime::runtime_archive;
use super::scratch::ScratchDir;
use std::{
    path::Path,
    process::{Command, Output},
};

pub struct Workspace(ScratchDir);

impl Workspace {
    pub fn new() -> Self {
        Self(ScratchDir::new("cli"))
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }

    pub fn write(&self, name: &str, source: &str) {
        std::fs::create_dir_all(self.path().join(name).parent().unwrap()).unwrap();
        std::fs::write(self.path().join(name), source).unwrap();
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
        command.current_dir(self.path());
        #[cfg(unix)]
        command.env("JOKY_RUNTIME", runtime_archive());
        command
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    pub fn assert_no_build_files(&self) {
        assert!(std::fs::read_dir(self.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".joky-build-")));
    }

    /// Exercise the default build output with the same flags on all backends.
    #[cfg(unix)]
    #[track_caller]
    pub fn assert_all_backends(
        &self,
        file: &str,
        flags: &[&str],
        expected_stdout: &[u8],
        expected_status: i32,
    ) {
        for interpreter in [false, true] {
            let mut args = vec!["run", file];
            if interpreter {
                args.push("--interpreter");
            }
            args.extend_from_slice(flags);
            let output = self.cli(&args);
            assert_execution(&output, expected_stdout, expected_status, &args);
        }
        let mut args = vec!["build", file];
        args.extend_from_slice(flags);
        success(&self.cli(&args));
        let executable = self
            .path()
            .join(file)
            .with_extension(std::env::consts::EXE_EXTENSION);
        let output = Command::new(executable).output().unwrap();
        assert_execution(&output, expected_stdout, expected_status, &args);
        self.assert_no_build_files();
    }
}

#[track_caller]
pub fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[track_caller]
pub fn failure(output: &Output, message: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.starts_with("error: ") && error.contains(message),
        "{error}"
    );
    assert!(!error.contains("panicked"), "{error}");
}

#[cfg(unix)]
#[track_caller]
fn assert_execution(output: &Output, stdout: &[u8], status: i32, args: &[&str]) {
    assert_eq!(output.status.code(), Some(status), "{args:?}: {output:?}");
    assert_eq!(output.stdout, stdout, "{args:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
}
