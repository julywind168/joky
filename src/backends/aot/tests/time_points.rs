//! Clock, calendar and absolute-wait APIs link against the distributed C ABI.
use super::*;

#[test]
#[cfg(unix)]
fn time_points_native_values_waits_and_cancellation() {
    let scratch = ScratchDir::new("time_points");
    let object = scratch.path().join("time.o");
    let host = scratch.path().join("host.c");
    let executable = scratch.path().join("time");
    let mut compiler = AotCompiler::new("time_points").unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(include_str!("../../../../tests/fixtures/time_points.jk"))
                .unwrap(),
        )
        .unwrap();
    let mut extra =
        String::from("#include \"joky_task.h\"\n#include \"joky_runtime.h\"\nint main(void) {\n");
    for name in [
        "timestamp_values",
        "timestamp_limits",
        "timestamp_formats",
        "timestamp_rejects",
        "time_wall",
        "time_instant",
    ] {
        extra.push_str(&format!("extern int32_t {name}(void); if ({name}() != 42 || joky_runtime_error_code()) return 30;\n"));
    }
    let slots = compiler.cps_frame_slots("time_wait").unwrap();
    extra.push_str(&format!(r#"{{
                extern int32_t time_wait(JokyCpsFrame *);
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (joky_cps_block_on(time_wait, frame) != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42 || joky_runtime_error_code()) return 31;
                joky_cps_frame_free(frame);
            }}"#));
    let slots = compiler.cps_frame_slots("time_cancel").unwrap();
    extra.push_str(&format!(
        r#"{{
                extern int32_t time_cancel(JokyCpsFrame *);
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (time_cancel(frame) != JOKY_CPS_PENDING || joky_runtime_error_code()) return 32;
                joky_cps_frame_free(frame);
            }}"#
    ));
    extra.push_str(r#"{
                uint64_t clock = joky_time_clock();
                uint64_t ticks = joky_time_ticks(clock);
                if (joky_time_ticks(clock) < ticks || joky_runtime_error_code()) return 33;
                if (joky_time_unix_micros() <= 0 || joky_runtime_error_code()) return 34;
                JokyCpsFrame *frame = joky_cps_frame_new(2);
                __joky_cps_slots(frame)[0] = clock;
                __joky_cps_slots(frame)[1] = ticks;
                if (joky_task_sleep_until(frame) != JOKY_CPS_COMPLETED) return 35;
                joky_cps_frame_free(frame);
                frame = joky_cps_frame_new(2);
                __joky_cps_slots(frame)[0] = clock + 1;
                __joky_cps_slots(frame)[1] = ticks;
                if (joky_task_sleep_until(frame) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 36;
                joky_cps_frame_free(frame);
                joky_runtime_error_clear();
            }"#);
    extra.push_str("return 0; }\n");
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, extra).unwrap();
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
