use super::parity::Package;
use sha2::{Digest, Sha256};
use std::process::Command;

#[test]
fn crypto_base64_rfc4648_and_strict_decoding() {
    Package::new(
        "base64-vectors",
        &[("main.jk", include_str!("../fixtures/base64.jk"))],
    )
    .check_cached("base64 ok\n", None, &[]);
}

#[test]
fn crypto_sha256_standard_vectors_and_million_bytes() {
    Package::new(
        "sha256-vectors",
        &[("main.jk", include_str!("../fixtures/sha256.jk"))],
    )
    .check_cached("sha256 ok\n", None, &[]);
}

#[test]
fn crypto_sha256_matches_independent_digest_at_block_and_padding_boundaries() {
    // Include both sides of the 56-byte padding boundary, full blocks, and
    // chunks that complete a pending block before processing more full blocks.
    let lengths = [
        0usize, 1, 2, 3, 7, 31, 55, 56, 57, 63, 64, 65, 111, 112, 119, 120, 121, 127, 128, 129,
        255, 256, 257, 4097,
    ];
    let mut expected = String::new();
    for length in lengths {
        let data = (0..length)
            .map(|i| ((i * 131 + (i / 7) * 17) & 255) as u8)
            .collect::<Vec<_>>();
        let hash = Sha256::digest(&data);
        let debug = format!(
            "Bytes[{}]\n",
            hash.iter()
                .map(|b| format!("0x{b:02x}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        // One-shot digest plus each of the nine independently partitioned inputs.
        for _ in 0..10 {
            expected.push_str(&debug);
        }
    }
    let cases = lengths
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        r#"
import joky/crypto/sha256
fn exercise(length: UInt64) -> Result(Unit, String) {{
    let input = MutBytes()
    var index: UInt64 = 0
    while index < length {{
        input.push(((index * 131 + (index / 7) * 17) & 255) as% UInt8)
        index += 1
    }}
    let data = input.to_bytes()
    println(sha256.digest(data)?.debug())
    let sizes: List(UInt64) = List#{{1, 7, 55, 56, 63, 64, 65, 127, 1024}}
    for size in sizes {{
        let state = sha256.new()
        state.update(b"")?
        var offset: UInt64 = 0
        while offset < length {{
            let count = size.min(length - offset)
            state.update(data.slice(offset, count)!)?
            state.update(b"")?
            offset += count
        }}
        println(state.finish().debug())
    }}
    Ok(())
}}
fn main() -> Result(Unit, String) {{
    let lengths: List(UInt64) = List#{{{cases}}}
    for length in lengths {{ exercise(length)? }}
    Ok(())
}}
"#
    );
    Package::new("sha256-boundaries", &[("main.jk", &source)]).check(&expected, None, &[]);
}

#[test]
fn crypto_sha256_finish_consumes_the_state() {
    let package = Package::new(
        "sha256-consumed",
        &[(
            "main.jk",
            r#"
import joky/crypto/sha256
fn main() {
    let state = sha256.new()
    let _ = state.finish()
    state.update(b"again")!
}
"#,
        )],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_joky"))
        .current_dir(&package.root)
        .args(["check", "--no-cache"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "consumed hash state was accepted");
    assert!(stderr.contains("after move"), "{stderr}");
}

#[test]
fn crypto_example_combines_base64_and_incremental_sha256() {
    Package::new(
        "crypto-example",
        &[("main.jk", include_str!("../../examples/basics/crypto.jk"))],
    )
    .check(
        "Sm9reQ==\nJoky\nTtXa7I1iuvmdGbB6EeX5VxC0hC7Qjp02Mx6FA1SLhus=\n",
        None,
        &[],
    );
}
