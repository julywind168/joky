//! Production List runs in independent native processes, including cleanup failures.
use super::*;
use crate::vm::list_tests::{fixture_source, model_events, model_source, CASES};

fn check(source: &str, model: Option<&[i32]>, failure: bool, drops: &[i32], invariants: bool) {
    let fixture = ScratchDir::new("list-native");
    let object = fixture.path().join("list.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("list");
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
    let mut compiler = AotCompiler::new("list-native").unwrap();
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
  if ((joky_runtime_error_code() != 0) != {failure} || (!{failure} && result != 42) || count > 65536) {{ joky_runtime_print_error(); fprintf(stderr, "List result=%d count=%d\n", result, count); return 1; }}
"#,
        failure = i32::from(failure)
    );
    if let Some(model) = model {
        harness.push_str(&format!("if (count != {}) return 2; for (int i = 0; i < count; ++i) if (events[i] != expected[i]) {{ fprintf(stderr, \"List event %d: %d != %d\\n\", i, events[i], expected[i]); return 3; }}\n", model.len()));
    } else if !drops.is_empty() {
        harness.push_str(&format!("if (count != {}) return 4; for (int i = 0; i < count; ++i) {{ int seen = 0; for (int j = 0; j < count; ++j) seen += events[j] == expected[i]; if (seen != 1) return 5; }}\n", drops.len()));
    } else {
        harness.push_str("for (int i = 0; i < count; ++i) { int n = events[i]; if (!((n >= -7 && n <= -1) || (n >= 1 && n <= 7) || n == 100 || n == 101)) return 8; }\n");
        harness.push_str("for (int id = 1; id <= 7; ++id) { int created = 0, dropped = 0; for (int i = 0; i < count; ++i) { created += events[i] == -id; dropped += events[i] == id; } if (created != dropped || created > 1) return 6; }\n");
        if failure {
            harness.push_str("for (int i = 0; i < count; ++i) if (events[i] == 99) return 7;\n");
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
fn list_native_executes_public_apis_and_each_owner_cleanup() {
    for &(entry, error) in CASES {
        let drops: &[i32] = match entry {
            "clear_failure" => &[1, 2, 3],
            "replace_drop_failure" => &[1, 2],
            _ => &[],
        };
        check(&fixture_source(entry), None, error.is_some(), drops, false);
    }
}
#[test]
fn list_native_model_checks_order_links_free_slots_and_dense_indices() {
    for seed in [123456789, 0xfedcba98] {
        check(
            &model_source(seed, 256),
            Some(&model_events(seed, 256)),
            false,
            &[],
            true,
        );
    }
}
