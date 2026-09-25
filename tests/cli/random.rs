use super::parity::Package;
use std::process::Command;

#[test]
fn crypto_random_os_bytes_bounds_and_tasks() {
    Package::new(
        "random-os",
        &[("main.jk", include_str!("../fixtures/random.jk"))],
    )
    .check_cached("random ok\n", None, &[]);
}

#[test]
fn crypto_random_handler_can_supply_deterministic_bytes_and_failures() {
    Package::new(
        "random-handler",
        &[(
            "main.jk",
            r#"
import joky/crypto/random
fn request(length: UInt64) -> Result(Bytes, String) effects { random } {
    random.bytes(length)
}
fn fixture(length: UInt64) -> Result(Bytes, String) {
    if length == (4 as% UInt64) { Ok(b"test") } else { Err("entropy unavailable") }
}
fn main() {
    let result = do { request(4) } with { random.bytes(length) => fixture(length) }
    if result! != b"test" { panic("random handler bytes") }
    let failure = do { request(8) } with { random.bytes(length) => fixture(length) }
    match failure {
        Ok(_) => panic("random handler failure")
        Err(message) => if message != "entropy unavailable" { panic("error propagation") }
    }
    println("random handler ok")
}
"#,
        )],
    )
    .check_cached("random handler ok\n", None, &[]);
}

#[test]
fn crypto_random_requires_a_declared_or_handled_effect() {
    let package = Package::new(
        "random-unhandled",
        &[(
            "main.jk",
            r#"
import joky/crypto/random
fn unhandled() -> Result(Bytes, String) { random.bytes(24) }
fn main() { let _ = unhandled(); () }
"#,
        )],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&package.root)
        .args(["check", "--no-cache"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "unhandled random effect was accepted"
    );
    assert!(stderr.contains("random"), "{stderr}");
}
