//! Set runs in independent native processes with identity and cleanup observations.
use super::*;
use crate::vm::tests::set_tests::{fixture_entries, model_events, model_source, CASES};

fn check(source: &str, cases: &[collections::Case<'_>], extra: &str) {
    let mut types = crate::TypeRegistry::default();
    types.prelude = false;
    let ast = types
        .resolve_with_modules(
            &crate::parser::parse(source).unwrap(),
            &crate::ModuleSources::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    collections::check(&ast, cases, extra);
}
#[test]
fn set_native_executes_ownership_identity_and_error_paths() {
    let names: Vec<_> = CASES
        .iter()
        .map(|case| format!("probe_{}", case.0))
        .collect();
    let cases: Vec<_> = CASES
        .iter()
        .zip(&names)
        .map(|(&(_, error, drops), name)| collections::Case {
            entry: name,
            failure: error.is_some(),
            events: if drops.is_empty() {
                collections::Events::Balanced(5)
            } else {
                collections::Events::Unordered(drops)
            },
        })
        .collect();
    let entries: Vec<_> = CASES.iter().map(|case| case.0).collect();
    check(&fixture_entries(&entries), &cases, "");
}
#[test]
fn set_native_model_checks_membership_and_all_algebra_results() {
    for seed in [123456789, 0xfedcba98] {
        check(
            &model_source(seed, 256),
            &[collections::Case { entry: "probe", failure: false, events: collections::Events::Exact(&model_events(seed, 256)) }],
            "extern int32_t matrix(void); if (matrix() != 42 || joky_runtime_error_code()) return 9;\n",
        );
    }
}

#[test]
#[ignore = "exhaustive: native long Set model and full eight-element algebra matrix"]
fn exhaustive_set_native_model_and_algebra() {
    for seed in [123456789, 0xfedcba98] {
        check(
            &model_source(seed, 4096),
            &[collections::Case {
                entry: "probe",
                failure: false,
                events: collections::Events::Exact(&model_events(seed, 4096)),
            }],
            if seed == 123456789 {
                "extern int32_t row(int32_t); for (int a = 0; a < 256; ++a) if (row(a) != 42 || joky_runtime_error_code()) return 9;\n"
            } else {
                ""
            },
        );
    }
}
