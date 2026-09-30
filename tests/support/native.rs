//! Common C compiler and runtime-link helpers for standalone AOT fixtures.
use std::{path::Path, process::Command};

pub fn c_compiler() -> Command {
    let mut command = Command::new("cc");
    command
        .args(["-Wall", "-Wextra", "-Werror", "-I"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("include"));
    command
}

pub fn link_runtime(command: &mut Command) -> &mut Command {
    command.arg(super::runtime::runtime_archive());
    if cfg!(target_os = "macos") {
        command.args(["-liconv", "-lSystem"]);
    } else {
        command.args([
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]);
    }
    command
}

#[track_caller]
pub fn link(command: &mut Command, destination: &Path) {
    let output = command.arg("-o").arg(destination).output().unwrap();
    assert!(output.status.success(), "{command:?}: {output:?}");
}
