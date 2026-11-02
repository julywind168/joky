//! Independent native artifacts link the cryptographic runtime.
use super::*;

#[test]
#[cfg(unix)]
fn crypto_standalone_vectors_errors_and_binary_boundaries() {
    assert_sync_results(
        &crate::parser::parse(include_str!("../../../../tests/fixtures/crypto.jk")).unwrap(),
        &[
            ("crypto_vectors", Some(42)),
            ("crypto_verification_and_views", Some(42)),
            ("crypto_boundaries", Some(42)),
            ("crypto_repeated", Some(42)),
        ],
    );
}
