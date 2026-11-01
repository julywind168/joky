//! Standalone native artifacts use the production OS entropy backend.
use super::*;

#[test]
#[cfg(unix)]
fn random_standalone_values_ranges_shuffle_and_entropy() {
    assert_sync_results(
        &crate::parser::parse(include_str!("../../../../tests/fixtures/random.jk")).unwrap(),
        &[
            ("random_vectors", Some(42)),
            ("random_ranges", Some(42)),
            ("random_shuffle", Some(42)),
            ("random_secure", Some(42)),
        ],
    );
}
