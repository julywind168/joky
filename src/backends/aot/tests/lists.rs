//! Production List runs in independent native processes, including cleanup failures.
use super::*;
use crate::vm::tests::list_tests::{fixture_entries, model_events, model_source, CASES};

fn check(source: &str, cases: &[collections::Case<'_>], invariants: bool) {
    let mut types = crate::TypeRegistry::default();
    types.prelude = false;
    if invariants {
        types
            .standard
            .set(crate::core::imports::list_test_library())
            .unwrap();
    }
    let ast = types
        .resolve_with_modules(
            &crate::parser::parse(source).unwrap(),
            &crate::ModuleSources::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    collections::check(&ast, cases, "");
}
#[test]
fn list_native_executes_public_apis_and_each_owner_cleanup() {
    let names: Vec<_> = CASES
        .iter()
        .map(|case| format!("probe_{}", case.0))
        .collect();
    let cases: Vec<_> = CASES
        .iter()
        .zip(&names)
        .map(|(&(entry, error), name)| {
            let events = match entry {
                "clear_failure" => collections::Events::Unordered(&[1, 2, 3]),
                "replace_drop_failure" => collections::Events::Unordered(&[1, 2]),
                _ => collections::Events::Balanced(7),
            };
            collections::Case {
                entry: name,
                failure: error.is_some(),
                events,
            }
        })
        .collect();
    let entries: Vec<_> = CASES.iter().map(|case| case.0).collect();
    check(&fixture_entries(&entries), &cases, false);
}
#[test]
fn list_native_model_checks_order_links_free_slots_and_dense_indices() {
    for seed in [123456789, 0xfedcba98] {
        check(
            &model_source(seed, 256),
            &[collections::Case {
                entry: "probe",
                failure: false,
                events: collections::Events::Exact(&model_events(seed, 256)),
            }],
            true,
        );
    }
}
