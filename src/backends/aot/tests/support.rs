//! Support AOT fixtures.
use super::*;

pub(super) fn check_arena_cps_fixture(
    name: &str,
    source: &str,
    cases: &[(&str, &str, usize)],
    extra: impl FnOnce(&AotCompiler) -> String,
) {
    let fixture = ScratchDir::new(name);
    let dir = fixture.path();
    let object = dir.join("arena.o");
    let host = dir.join("host.c");
    let executable = dir.join("arena");
    let mut compiler = AotCompiler::new(name).unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let mut harness = String::from(
        r#"
        #include "joky_cps.h"
        #include "joky_runtime.h"
        static int ready, count;
        static int32_t events[64];
        int32_t record(int32_t value) {
            if (count < 64) events[count] = value;
            ++count; return value;
        }
        int32_t delayed_value(JokyCpsFrame *frame) {
            if (joky_cps_state(frame) == 0) {
                record((int32_t)__joky_cps_slots(frame)[0]);
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0] + 1);
            return JOKY_CPS_COMPLETED;
        }
        int32_t fail_now(JokyCpsFrame *frame) { (void)frame; return 7; }
        static int check(JokyCpsEntry entry, size_t slots, const int32_t *expected, int length) {
            ready = 0; count = 0;
            JokyCpsFrame *frame = joky_cps_frame_new(slots);
            if (entry(frame) != JOKY_CPS_PENDING) return 1;
            int pending_count = count;
            for (int i = 0; i < 4; ++i) {
                if (entry(frame) != JOKY_CPS_PENDING || count != pending_count) return 2;
            }
            ready = 1;
            int status = JOKY_CPS_PENDING;
            for (int i = 0; i < 16 && status == JOKY_CPS_PENDING; ++i) status = entry(frame);
            if (status != JOKY_CPS_COMPLETED || joky_cps_state(frame) != JOKY_CPS_DONE) return 3;
            if (joky_cps_result(frame) != 42 || joky_runtime_error_code() || count != length) return 4;
            for (int i = 0; i < length; ++i) if (events[i] != expected[i]) return 5;
            if (entry(frame) != JOKY_CPS_INVALID_STATE) return 6;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#,
    );
    for (name, _, _) in cases {
        harness.push_str(&format!("extern int32_t {name}(JokyCpsFrame *);\n"));
    }
    harness.push_str("int main(void) { for (int run = 0; run < 20; ++run) {\n");
    for (name, events, length) in cases {
        let slots = compiler.cps_frame_slots(name).unwrap();
        harness.push_str(&format!("{{ const int32_t expected[] = {{{events}}}; if (check({name}, {slots}, expected, {length})) return 10; }}\n"));
    }
    harness.push_str(&extra(&compiler));
    harness.push_str("} return 0; }\n");
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
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

#[cfg(unix)]
pub(crate) fn assert_sync_int32(ast: &crate::parser::ast::Module, function: &str, expected: i32) {
    assert_sync_results(ast, &[(function, Some(expected))]);
}

#[cfg(unix)]
pub(crate) fn assert_sync_results(ast: &crate::parser::ast::Module, cases: &[(&str, Option<i32>)]) {
    let fixture = ScratchDir::new("checked-source");
    let object = fixture.path().join("checked.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("checked");
    let mut compiler = AotCompiler::new("checked-source").unwrap();
    compiler.compile_module(ast).unwrap();
    compiler.write_object(&object).unwrap();
    let mut harness = String::from("#include <stdint.h>\n#include \"joky_runtime.h\"\nvoid record_int(int32_t value) { (void)value; }\n");
    let mut calls = String::new();
    for (index, (function, expected)) in cases.iter().enumerate() {
        harness.push_str(&format!("extern int32_t {function}(void);\n"));
        let failed = match expected {
            Some(value) => format!("joky_runtime_error_code() || result != {value}"),
            None => "joky_runtime_error_code() == 0".to_owned(),
        };
        calls.push_str(&format!("{{ joky_runtime_error_clear(); int32_t result = {function}(); (void)result; if ({failed}) return {}; }}\n", index + 1));
    }
    harness.push_str(&format!("int main(void) {{ {calls} return 0; }}\n"));
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    let status = Command::new(executable).status().unwrap();
    assert!(status.success(), "native cases {cases:?}: {status}");
}
