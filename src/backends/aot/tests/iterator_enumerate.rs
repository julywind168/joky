use super::*;

#[test]
fn iterator_enumerate_native_checks_index_boundaries_and_failure_cleanup() {
    let mut types = crate::TypeRegistry::default();
    types.prelude = false;
    types
        .standard
        .set(crate::core::imports::iter_test_library())
        .unwrap();
    let ast = types
        .resolve_with_modules(
            &crate::parser::parse(include_str!(
                "../../../../tests/fixtures/iterator_enumerate_boundaries.jk"
            ))
            .unwrap(),
            &crate::ModuleSources::new(),
        )
        .unwrap();
    collections::check(
        &ast,
        &[
            collections::Case {
                entry: "maximum",
                failure: false,
                events: collections::Events::Unordered(&[1, 2]),
            },
            collections::Case {
                entry: "overflow",
                failure: true,
                events: collections::Events::Unordered(&[1, 2, 3, 10]),
            },
            collections::Case {
                entry: "after_none",
                failure: true,
                events: collections::Events::Exact(&[101, 102, 103]),
            },
        ],
        "",
    );
}
