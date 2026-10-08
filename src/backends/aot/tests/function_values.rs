//! Function values AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn function_values_execute_native_indirect_calls_and_pending_frames() {
    let source = include_str!("../../../../tests/fixtures/function_values.jk");
    let fixture = ScratchDir::new("function-values");
    let dir = fixture.path();
    let object = dir.join("functions.o");
    let host = dir.join("host.c");
    let program = dir.join("functions");
    let mut compiler = AotCompiler::new("function-values").unwrap();
    let exports = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert!(!exports
        .iter()
        .any(|name| name.starts_with("__joky_") || name == "choose"));
    let slots = compiler.cps_frame_slots("function_task").unwrap();
    let order_slots = compiler.cps_frame_slots("function_task_order").unwrap();
    let aggregate_slots = compiler.cps_frame_slots("function_task_aggregate").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t function_basic(void), function_order(void), function_storage(void), function_layouts(void), function_combinators(void), function_arena(void), function_mixed_dispatch(void), function_failure(void);
        extern int32_t function_task(JokyCpsFrame *), function_task_order(JokyCpsFrame *), function_task_aggregate(JokyCpsFrame *);
        extern int32_t increment(int32_t);
        static int ready;
        int32_t pause(JokyCpsFrame *frame) {{
            if (!ready) return 1;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0]);
            return 0;
        }}
        int main(void) {{
            int32_t (*tasks[])(JokyCpsFrame *) = {{function_task, function_task_order, function_task_aggregate}};
            const uintptr_t slots[] = {{{slots}, {order_slots}, {aggregate_slots}}};
            for (int i = 0; i < 3; i++) {{
                if (function_basic() != 42 || function_order() != 42 || function_storage() != 42 || function_layouts() != 42 || function_combinators() != 42 || function_arena() != 42 || function_mixed_dispatch() != 42 || increment(41) != 42) return 1;
                ready = 0;
                JokyCpsFrame *frame = joky_cps_frame_new(slots[i]);
                if (tasks[i](frame) != 1 || tasks[i](frame) != 1) return 2;
                ready = 1;
                if (tasks[i](frame) != 0 || joky_cps_result(frame) != 42) return 3;
                joky_cps_frame_free(frame);
                ready = 0; frame = joky_cps_frame_new(slots[i]);
                if (tasks[i](frame) != 1) return 4;
                joky_cps_frame_free(frame);
                if (joky_runtime_error_code()) return 5;
                (void) function_failure();
                if (!joky_runtime_error_code()) return 6;
                joky_runtime_error_clear();
            }}
            return 0;
        }}
    "#)).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[cfg(unix)]
fn suspending_function_values_execute_relocated_cps_descriptors() {
    check_arena_cps_fixture(
        "suspending-function-values",
        include_str!("../../../../tests/fixtures/suspending_function_values.jk"),
        &[
            ("suspending_basic", "41,41,0,0,41,41", 6),
            ("suspending_storage", "41,41,41,41", 4),
            ("suspending_order", "1,40,41,43", 4),
            ("suspending_aggregates", "41,41,0,0", 4),
            ("suspending_scope", "1,101,1,302,201", 5),
            ("suspending_nested_scope", "0,101,1,102,2,201,202", 7),
            ("suspending_recursive", "41", 1),
            ("suspending_reload", "40", 1),
        ],
        |compiler| {
            let mut extra = String::new();
            let slots = compiler.cps_frame_slots("suspending_race").unwrap();
            extra.push_str(&format!(r#"
                {{
                    extern int32_t suspending_race(JokyCpsFrame *);
                    ready = 0; count = 0;
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    if (suspending_race(frame) != 0 || joky_cps_result(frame) != 42) return 20;
                    if (count != 3 || events[0] != 101 || events[1] != 1 || events[2] != 302) return 21;
                    joky_cps_frame_free(frame);
                }}
            "#));
            for name in ["suspending_failure", "suspending_scope_failure"] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_PENDING) return 22;
                        ready = 1;
                        if ({name}(frame) != 7 || joky_cps_state(frame) != JOKY_CPS_FAILED) return 23;
                        if ({name}(frame) != JOKY_CPS_INVALID_STATE) return 24;
                        joky_cps_frame_free(frame);
                    }}
                "#));
            }
            for name in [
                "suspending_reload",
                "suspending_scope",
                "suspending_recursive",
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(
                    r#"
                    {{
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_PENDING) return 25;
                        joky_cps_frame_free(frame);
                    }}
                "#
                ));
            }
            extra
        },
    );
}
