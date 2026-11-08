//! Native output publication and installed runtime discovery.
use super::*;

#[test]
#[cfg(unix)]
fn built_program_matches_jit_output_and_avoids_native_symbol_collisions() {
    let work = Workspace::new();
    work.write(
        "hello world.jk",
        r#"
        import joky/io.{println} fn report_int32(value: Int32) { println(&value) }
        fn printf(x: Int32) -> Int32 { x + 1 }
        fn pick(unit: Unit, value: Int32) -> Int32 { unit; value }
        fn main() { report_int32(pick({}, printf(41))) }
    "#,
    );
    let run = work.cli(&["run", "hello world.jk"]);
    success(&run);
    assert_eq!(run.stdout, b"42\n");
    for colored in [true, false] {
        let mut command = work.command();
        command.args(["build", "hello world.jk"]);
        if colored {
            command.env_remove("NO_COLOR").env("CLICOLOR_FORCE", "1");
        } else {
            command.env("NO_COLOR", "1").env_remove("CLICOLOR_FORCE");
        }
        let build = command.output().unwrap();
        success(&build);
        assert!(build.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&build.stderr),
            if colored {
                "\x1b[1;32m     Built\x1b[0m hello world\n"
            } else {
                "     Built hello world\n"
            }
        );
    }
    // The artifact remains runnable without either its source or the CLI.
    std::fs::remove_file(work.path().join("hello world.jk")).unwrap();
    let native = Command::new(work.path().join("hello world"))
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
            "fn report_int32(x: Int32) -> Int32 { x } fn main() -> Int32 { report_int32(8) }",
            8,
        ),
    ] {
        work.write("exit.jk", source);
        let run = work.cli(&["run", "exit.jk"]);
        assert_eq!(run.status.code(), Some(code));
        assert!(run.stdout.is_empty() && run.stderr.is_empty());
        success(&work.cli(&["build", "--output", "custom program", "exit.jk"]));
        let native = Command::new(work.path().join("custom program"))
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
    symlink(work.path().join("safe.jk"), work.path().join("alias")).unwrap();
    failure(
        &work.cli(&["build", "safe.jk", "-o", "alias"]),
        "must not replace the source",
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("safe.jk")).unwrap(),
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
    std::fs::set_permissions(
        work.path().join("cc"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let output = work
        .command()
        .args(["build", "safe.jk", "-o", "existing"])
        .env("PATH", work.path())
        .output()
        .unwrap();
    failure(&output, "test linker failure");
    work.write("safe.jk", "fn main() { unknown() }");
    failure(
        &work.cli(&["build", "safe.jk", "-o", "existing"]),
        "Undeclared function",
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn missing_standard_runtime_has_an_actionable_error_and_preserves_output() {
    let work = Workspace::new();
    work.write("vm.jk", include_str!("../../examples/vm.jk"));
    work.write("existing", "keep this output");
    let output = work
        .command()
        .args(["build", "vm.jk", "-o", "existing"])
        .env("JOKY_RUNTIME", work.path().join("missing.a"))
        .output()
        .unwrap();
    failure(&output, "JOKY_RUNTIME does not name a runtime archive");
    assert_eq!(
        std::fs::read_to_string(work.path().join("existing")).unwrap(),
        "keep this output"
    );
    work.assert_no_build_files();
}

#[test]
#[cfg(unix)]
fn installed_compiler_finds_its_runtime_without_repository_paths() {
    let work = Workspace::new();
    let bin = work.path().join("installation/bin");
    let lib = work.path().join("installation/lib/joky");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_joky"), bin.join("joky")).unwrap();
    std::fs::copy(runtime_archive(), lib.join("libjoky.a")).unwrap();
    work.write("vm.jk", include_str!("../../examples/vm.jk"));
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "vm.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    work.write(
        "division.jk",
        "fn divide() -> Int32 { 7 / 0 } fn main() -> Int32 { divide() }",
    );
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "division.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    work.write(
        "shift.jk",
        "fn shift(n: UInt64) -> UInt64 { 1 << n } fn main() { shift(64); }",
    );
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "shift.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    work.write(
        "async_main.jk",
        r#"import joky/task
import joky/domain
import joky/random
import joky/crypto
import joky/password
import joky/hex
import joky/check.{assert}
import joky/io.{println}
fn value(n: Int32, waits: Int32) @suspends -> Int32 {
    var i = 0
    while i < waits { task.yield_now(); i = i + 1 }
    n
}
fn main() @suspends {
    let entropy = random.secure_bytes(32).unwrap()
    assert(entropy.len() == 32, "Installed entropy backend")
    let message = "abc"
    let digest = crypto.sha256(message.bytes())
    assert(hex.encode(digest.slice(0, 32)) == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "Installed SHA-256 backend")
    var streamed = crypto.Sha256.new()
    streamed.update(message.bytes())
    let streamed_digest = streamed.finish()
    assert(hex.encode(streamed_digest.slice(0, 32)) == hex.encode(digest.slice(0, 32)), "Installed streaming SHA backend")
    var streamed_mac = crypto.HmacSha256.new(entropy.slice(0, 32))
    streamed_mac.update(message.bytes())
    let tag = streamed_mac.finish()
    var verifier = crypto.HmacSha256.new(entropy.slice(0, 32))
    verifier.update(message.bytes())
    assert(verifier.verify(tag.slice(0, 32)), "Installed streaming HMAC backend")
    assert(crypto.verify_hmac_sha256(entropy.slice(0, 32), message.bytes(), tag.slice(0, 32)), "Installed HMAC backend")
    let password_hash = password.hash("installed example password").unwrap()
    assert(!password.needs_rehash(&password_hash).unwrap(), "Installed password policy")
    assert(password.verify("installed example password", password_hash).unwrap(), "Installed Argon2id backend")
    task.sleep(250us)
    let deadline = domain.deadline_after(0ns)
    let (a, b) = join { value(40, 2), value(2, 0) }
    let winner = race { value(a + b, 0), value(0, 3) }
    println(&winner)
}
"#,
    );
    let output = Command::new(bin.join("joky"))
        .current_dir(work.path())
        .args(["build", "async_main.jk"])
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&output);
    std::fs::remove_dir_all(work.path().join("installation")).unwrap();
    std::fs::remove_file(work.path().join("async_main.jk")).unwrap();
    let program = Command::new(work.path().join("async_main"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    success(&program);
    assert_eq!(program.stdout, b"42\n");
    std::fs::remove_file(work.path().join("vm.jk")).unwrap();
    let program = Command::new(work.path().join("vm"))
        .env("PATH", "")
        .output()
        .unwrap();
    success(&program);
    assert_eq!(program.stdout, b"42\n100\n");
    std::fs::remove_file(work.path().join("shift.jk")).unwrap();
    let program = Command::new(work.path().join("shift"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    failure(&program, "Shift count");
    assert!(program.stdout.is_empty());
    std::fs::remove_file(work.path().join("division.jk")).unwrap();
    let program = Command::new(work.path().join("division"))
        .env("PATH", "")
        .env_remove("JOKY_RUNTIME")
        .output()
        .unwrap();
    failure(&program, "Division by zero");
    assert_eq!(program.status.code(), Some(1));
}
