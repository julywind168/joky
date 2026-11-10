//! Relocatable dyn tables execute and clean up in an independent process.
use super::*;

#[test]
#[cfg(unix)]
fn borrowed_dyn_v2_standalone_lifecycle_and_cancellation() {
    let source = include_str!("../../../../tests/fixtures/borrowed_dyn_lifecycle.jk");
    let fixture = ScratchDir::new("borrowed-dyn-lifecycle");
    let object = fixture.path().join("dyn.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("dyn");
    let mut compiler = AotCompiler::new("borrowed-dyn-lifecycle").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("dyn_pending").unwrap();
    let local_slots = compiler.cps_frame_slots("dyn_local_task").unwrap();
    let between_slots = compiler
        .cps_frame_slots("dyn_local_between_pending")
        .unwrap();
    compiler.write_object(&object).unwrap();
    let division_start = source.find("42 / divisor").unwrap();
    let consume_start = source.match_indices("42 / divisor").nth(1).unwrap().0;
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_cps.h"
        #include "joky_runtime.h"
        static int32_t events[16];
        static int count, ready;
        int32_t record(int32_t value) {{ if (count < 16) events[count++] = value; return value; }}
        int32_t delayed_value(JokyCpsFrame *frame) {{
            if (!joky_cps_state(frame)) {{ record((int32_t)__joky_cps_slots(frame)[0]); joky_cps_set_state(frame, 1); return JOKY_CPS_PENDING; }}
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0] + 1); return JOKY_CPS_COMPLETED;
        }}
        extern int32_t dyn_success(void), dyn_failure(void), dyn_argument(void), dyn_argument_failure(void), dyn_argument_drop_failure(void), dyn_order(void), dyn_large_failure(void);
        extern int32_t dyn_pending(JokyCpsFrame *), dyn_local_task(JokyCpsFrame *), dyn_local_between_pending(JokyCpsFrame *);
        int main(void) {{
            struct Case {{ int32_t (*call)(void); int failure, count; int32_t events[4]; }} cases[] = {{
                {{dyn_success, 0, 1, {{10}}}}, {{dyn_failure, 1, 1, {{10}}}},
                {{dyn_order, 0, 3, {{20, 22, 30}}}}, {{dyn_argument, 0, 0, {{0}}}}, {{dyn_argument_failure, 1, 0, {{0}}}},

            }};
            for (int run = 0; run < 8; ++run) {{
                for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); ++i) {{
                    count = 0; joky_runtime_error_clear(); int32_t result = cases[i].call();
                    if (!!joky_runtime_error_code() != cases[i].failure || (!cases[i].failure && result != 42)) return 1;
                    if (cases[i].failure && joky_runtime_error_start() != (i == 1 ? {division_start} : {consume_start})) return 9;
                    if (count != cases[i].count) return 2;
                    for (int j = 0; j < count; ++j) if (events[j] != cases[i].events[j]) return 3;
                }}
                count = 0; joky_runtime_error_clear(); dyn_large_failure();
                if (!joky_runtime_error_code() || count != 0) return 16;
                joky_runtime_print_error();
                joky_runtime_error_clear();
                for (int cancel = 0; cancel < 2; ++cancel) {{
                    count = 0; ready = 0; joky_runtime_error_clear();
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    if (dyn_pending(frame) != JOKY_CPS_PENDING) return 4;
                    for (int i = 0; i < 3; ++i) if (dyn_pending(frame) != JOKY_CPS_PENDING || count != 2) return 5;
                    if (events[0] != 10 || events[1] != 41) return 6;
                    if (!cancel) {{ ready = 1; if (dyn_pending(frame) != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42) return 7; }}
                    joky_cps_frame_free(frame);
                    if (count != 2 || joky_runtime_error_code()) return 8;
                }}
                count = 0; joky_runtime_error_clear();
                JokyCpsFrame *local = joky_cps_frame_new({local_slots});
                if (dyn_local_task(local) != JOKY_CPS_COMPLETED || joky_cps_result(local) != 42) return 10;
                joky_cps_frame_free(local);
                if (count != 1 || events[0] != 10) return 11;
                count = 0; ready = 0;
                JokyCpsFrame *between = joky_cps_frame_new({between_slots});
                if (dyn_local_between_pending(between) != JOKY_CPS_PENDING) return 12;
                ready = 1;
                if (dyn_local_between_pending(between) != JOKY_CPS_PENDING) return 13;
                if (dyn_local_between_pending(between) != JOKY_CPS_COMPLETED || joky_cps_result(between) != 42) return 14;
                joky_cps_frame_free(between);
                if (count != 3 || events[0] != 41 || events[1] != 10 || events[2] != 41 || joky_runtime_error_code()) return 15;
            }}
            return 0;
        }}
    "#)).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{}: {stderr}", output.status);
    assert!(stderr.contains("budget"), "{stderr}");
}
