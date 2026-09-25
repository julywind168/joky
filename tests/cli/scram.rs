use super::parity::Package;
use std::process::Command;

#[test]
fn crypto_scram_sha256_rfc7677_and_strict_messages() {
    Package::new(
        "scram-sha256-vectors",
        &[("main.jk", include_str!("../fixtures/scram_sha256.jk"))],
    )
    .check_cached("scram sha256 ok\n", None, &[]);
}

#[test]
fn crypto_scram_sha256_independent_transcripts_and_forged_verifiers() {
    Package::new(
        "scram-independent-vectors",
        &[(
            "main.jk",
            include_str!("../fixtures/scram_sha256_vectors.jk"),
        )],
    )
    .check_cached("scram independent vectors ok\n", None, &[]);
}

#[test]
fn crypto_scram_random_nonce_and_entropy_failures() {
    Package::new(
        "scram-random",
        &[("main.jk", include_str!("../fixtures/scram_random.jk"))],
    )
    .check_cached("scram random ok\n", None, &[]);
}

#[test]
fn crypto_scram_state_transitions_consume_the_previous_state() {
    for (name, body) in [
        (
            "first",
            r#"
    let first = scram_sha256.start_with_nonce("user", "nonce", scram_sha256.Limits())!
    let _ = first.respond("invalid", b"password")
    println(first.message())
"#,
        ),
        (
            "final",
            r#"
    let first = scram_sha256.start_with_nonce("user", "nonce", scram_sha256.Limits())!
    let final = first.respond("r=nonceS,s=c2FsdA==,i=4096", b"password")!
    let _ = final.verify("e=invalid-proof")
    println(final.message())
"#,
        ),
    ] {
        let source = format!("import joky/crypto/scram_sha256\nfn main() {{ {body} }}\n");
        let package = Package::new(&format!("scram-consumed-{name}"), &[("main.jk", &source)]);
        let output = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .args(["check", "--no-cache"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "consumed {name} state was accepted"
        );
        assert!(stderr.contains("after move"), "{stderr}");
    }
}

#[test]
fn crypto_scram_start_requires_random_effect() {
    let package = Package::new(
        "scram-unhandled-random",
        &[(
            "main.jk",
            r#"
import joky/crypto/scram_sha256
fn unhandled() { let _ = scram_sha256.start("user", scram_sha256.Limits()); () }
fn main() { unhandled() }
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
