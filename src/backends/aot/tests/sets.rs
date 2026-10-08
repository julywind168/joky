//! Set runs in independent native processes with identity and cleanup observations.
use super::*;
use crate::vm::set_tests::{fixture_source, model_events, model_source, CASES};

fn check(source: &str, model: Option<&[i32]>, failure: bool, drops: &[i32], full_matrix: bool) {
    let fixture = ScratchDir::new("set-native");
    let object = fixture.path().join("set.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("set");
    let mut types = crate::TypeRegistry::default();
    types.prelude = false;
    let ast = types
        .resolve_with_modules(
            &crate::parser::parse(source).unwrap(),
            &crate::ModuleSources::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let mut compiler = AotCompiler::new("set-native").unwrap();
    compiler
        .compile_module(&ast)
        .unwrap_or_else(|e| panic!("{e}"));
    compiler.write_object(&object).unwrap();
    let expected = model
        .unwrap_or(drops)
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let expected = if expected.is_empty() { "0" } else { &expected };
    let mut harness = format!(
        r#"
#include <stdint.h>
#include <stdio.h>
#include "joky_runtime.h"
static int32_t events[65536]; static int count;
void record_int(int32_t value) {{ if (count < 65536) events[count] = value; ++count; }}
extern int32_t probe(void);
int main(void) {{
 const int32_t expected[] = {{{expected}}}; (void)expected;
 for (int run = 0; run < 2; ++run) {{
  count = 0; joky_runtime_error_clear(); int32_t result = probe();
  if ((joky_runtime_error_code() != 0) != {failure} || (!{failure} && result != 42) || count > 65536) {{ joky_runtime_print_error(); fprintf(stderr, "Set result=%d count=%d\n", result, count); return 1; }}
"#,
        failure = i32::from(failure)
    );
    if let Some(model) = model {
        harness.push_str(&format!("if (count != {}) return 2; for (int i = 0; i < count; ++i) if (events[i] != expected[i]) {{ fprintf(stderr, \"Set event %d: %d != %d\\n\", i, events[i], expected[i]); return 3; }}\n", model.len()));
    } else if !drops.is_empty() {
        harness.push_str(&format!("if (count != {}) return 4; for (int i = 0; i < count; ++i) {{ int seen = 0, wanted = 0; for (int j = 0; j < count; ++j) {{ seen += events[j] == expected[i]; wanted += expected[j] == expected[i]; }} if (seen != wanted) return 5; }}\n", drops.len()));
    } else {
        harness.push_str("for (int i = 0; i < count; ++i) { int n = events[i]; if (!((n >= -5 && n <= -1) || (n >= 1 && n <= 5) || n == 100 || n == 101)) return 8; }\n");
        harness.push_str("for (int id = 1; id <= 5; ++id) { int created = 0, dropped = 0; for (int i = 0; i < count; ++i) { created += events[i] == -id; dropped += events[i] == id; } if (created != dropped || created > 1) return 6; }\n");
        if failure {
            harness.push_str("for (int i = 0; i < count; ++i) if (events[i] == 99) return 7;\n");
        }
    }
    if model.is_some() {
        if full_matrix {
            harness.push_str("extern int32_t row(int32_t); for (int a = 0; a < 256; ++a) if (row(a) != 42 || joky_runtime_error_code()) return 9;\n");
        } else {
            harness.push_str("extern int32_t matrix(void); if (matrix() != 42 || joky_runtime_error_code()) return 9;\n");
        }
    }
    harness.push_str("} return 0; }\n");
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn set_native_executes_ownership_identity_and_error_paths() {
    for &(entry, error, drops) in CASES {
        check(&fixture_source(entry), None, error.is_some(), drops, false);
    }
}
#[test]
fn set_native_model_checks_membership_and_all_algebra_results() {
    for seed in [123456789, 0xfedcba98] {
        check(
            &model_source(seed, 256),
            Some(&model_events(seed, 256)),
            false,
            &[],
            false,
        );
    }
}

#[test]
#[ignore = "exhaustive: native long Set model and full eight-element algebra matrix"]
fn exhaustive_set_native_model_and_algebra() {
    for seed in [123456789, 0xfedcba98] {
        check(
            &model_source(seed, 4096),
            Some(&model_events(seed, 4096)),
            false,
            &[],
            seed == 123456789,
        );
    }
}
