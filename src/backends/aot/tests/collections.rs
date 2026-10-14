//! Batch native collection cases while resetting runtime errors and host effects per call.
use super::*;

pub(super) enum Events<'a> {
    Exact(&'a [i32]),
    Unordered(&'a [i32]),
    Balanced(i32),
}

pub(super) struct Case<'a> {
    pub entry: &'a str,
    pub failure: bool,
    pub events: Events<'a>,
}

pub(super) fn check(ast: &crate::parser::ast::Module, cases: &[Case<'_>], extra: &str) {
    let fixture = ScratchDir::new("collection-native");
    let object = fixture.path().join("collection.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("collection");
    let mut compiler = AotCompiler::new("collection-native").unwrap();
    compiler
        .compile_module(ast)
        .unwrap_or_else(|e| panic!("{e}"));
    compiler.write_object(&object).unwrap();
    let mut harness = String::from(
        r#"
#include <stdint.h>
#include <stdio.h>
#include "joky_runtime.h"
static int32_t events[65536]; static int count;
void record_int(int32_t value) { if (count < 65536) events[count] = value; ++count; }
"#,
    );
    for case in cases {
        harness.push_str(&format!("extern int32_t {}(void);\n", case.entry));
    }
    harness.push_str("int main(void) {\n");
    for case in cases {
        let entry = case.entry;
        harness.push_str(&format!(
            r#"for (int run = 0; run < 2; ++run) {{
 count = 0; joky_runtime_error_clear(); int32_t result = {entry}();
 if ((joky_runtime_error_code() != 0) != {failure} || (!{failure} && result != 42) || count > 65536) {{ joky_runtime_print_error(); fprintf(stderr, "{entry}: result=%d count=%d\n", result, count); return 1; }}
"#,
            failure = i32::from(case.failure),
        ));
        match case.events {
            Events::Exact(expected) | Events::Unordered(expected) => {
                let values = expected
                    .iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let values = if values.is_empty() { "0" } else { &values };
                harness.push_str(&format!("const int32_t expected[] = {{{values}}}; if (count != {}) {{ fprintf(stderr, \"{entry}: event count=%d\\n\", count); return 2; }}\n", expected.len()));
                if matches!(case.events, Events::Exact(_)) {
                    harness.push_str(&format!("for (int i = 0; i < count; ++i) if (events[i] != expected[i]) {{ fprintf(stderr, \"{entry}: event %d: %d != %d\\n\", i, events[i], expected[i]); return 3; }}\n"));
                } else {
                    harness.push_str(&format!("for (int i = 0; i < count; ++i) {{ int seen = 0, wanted = 0; for (int j = 0; j < count; ++j) {{ seen += events[j] == expected[i]; wanted += expected[j] == expected[i]; }} if (seen != wanted) {{ fprintf(stderr, \"{entry}: owner %d count=%d expected=%d\\n\", expected[i], seen, wanted); return 4; }} }}\n"));
                }
            }
            Events::Balanced(max) => {
                harness.push_str(&format!("for (int i = 0; i < count; ++i) {{ int n = events[i]; if (!((n >= -{max} && n <= -1) || (n >= 1 && n <= {max}) || n == 100 || n == 101)) {{ fprintf(stderr, \"{entry}: unexpected event %d\\n\", n); return 5; }} }}\n"));
                harness.push_str(&format!("for (int id = 1; id <= {max}; ++id) {{ int created = 0, dropped = 0; for (int i = 0; i < count; ++i) {{ created += events[i] == -id; dropped += events[i] == id; }} if (created != dropped || created > 1) {{ fprintf(stderr, \"{entry}: owner %d created=%d dropped=%d\\n\", id, created, dropped); return 6; }} }}\n"));
            }
        }
        if case.failure {
            harness.push_str(&format!("for (int i = 0; i < count; ++i) if (events[i] == 99) {{ fprintf(stderr, \"{entry}: effects after failure\\n\"); return 7; }}\n"));
        }
        harness.push_str(extra);
        harness.push_str("}\n");
    }
    harness.push_str("return 0; }\n");
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
