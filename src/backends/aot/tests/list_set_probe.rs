//! LS0 runs the same source and resource observations in a standalone process.
use super::*;
use crate::vm::list_set_probe_tests::{modules, source, CASES};

#[test]
#[cfg(unix)]
fn list_set_probe_executes_in_standalone_native_code() {
    for group in ["list", "set"] {
        check(group);
    }
}

fn check(group: &str) {
    let fixture = ScratchDir::new("list-set-probe");
    let object = fixture.path().join("probe.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("probe");
    let mut types = crate::TypeRegistry::default();
    types.prelude = false;
    let ast = types
        .resolve_with_modules(
            &crate::parser::parse(&source(group)).unwrap(),
            &modules(group),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let mut compiler = AotCompiler::new("list-set-probe").unwrap();
    compiler
        .compile_module(&ast)
        .unwrap_or_else(|e| panic!("{e}"));
    compiler.write_object(&object).unwrap();
    let mut harness = String::from(
        "#include <stdint.h>\n#include <stdio.h>\n#include \"joky_runtime.h\"\nstatic int32_t events[32]; static int count;\nvoid record_int(int32_t value) { if (count < 32) events[count] = value; ++count; }\n",
    );
    for (name, _) in CASES.iter().filter(|(name, _)| name.starts_with(group)) {
        harness.push_str(&format!("extern int32_t {name}(void);\n"));
    }
    harness.push_str("int main(void) {\n");
    for (name, expected) in CASES.iter().filter(|(name, _)| name.starts_with(group)) {
        let values = expected
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let values = if values.is_empty() { "0" } else { &values };
        harness.push_str(&format!(
            "{{ const int32_t expected[] = {{{values}}}; count = 0; joky_runtime_error_clear();\nint32_t result = {name}(); if (result != 42 || joky_runtime_error_code() || count != {}) {{ fprintf(stderr, \"{name}: result=%d count=%d\\n\", result, count); return 1; }}\nfor (int i = 0; i < count; ++i) if (events[i] != expected[i]) {{ fprintf(stderr, \"{name}: event %d got %d expected %d\\n\", i, events[i], expected[i]); return 2; }} }}\n",
            expected.len()
        ));
    }
    harness.push_str("return 0; }\n");
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}
