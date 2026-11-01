//! Independent native artifacts exercise all codec configurations.
use super::*;

#[test]
#[cfg(unix)]
fn codecs_standalone_vectors_errors_and_binary_boundaries() {
    assert_sync_results(
        &crate::parser::parse(include_str!("../../../../tests/fixtures/codecs.jk")).unwrap(),
        &[
            ("codecs_vectors", Some(42)),
            ("codecs_rejections", Some(42)),
            ("codecs_binary_boundaries", Some(42)),
            ("codecs_golden_binary", Some(42)),
            ("codecs_repeated", Some(42)),
        ],
    );
}
