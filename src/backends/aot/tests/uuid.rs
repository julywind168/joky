//! Native UUID interoperability and the real OS entropy path.
use super::*;

#[test]
#[cfg(unix)]
fn uuid_standalone_values_errors_and_entropy() {
    assert_sync_results(
        &crate::parser::parse(include_str!("../../../../tests/fixtures/uuid.jk")).unwrap(),
        &[
            ("uuid_values", Some(42)),
            ("uuid_rejections", Some(42)),
            ("uuid_secure", Some(42)),
        ],
    );
}
