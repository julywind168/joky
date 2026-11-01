//! Standalone native regions preserve borrowed host addresses across CPS polls.
use super::*;

#[test]
#[cfg(unix)]
fn suspending_arena_native_completion_failure_and_cancellation() {
    let source = include_str!("../../../../tests/fixtures/suspending_arena.jk");
    let ast = crate::parser::parse(source).unwrap();
    let fixture = ScratchDir::new("suspending-arena");
    let object = fixture.path().join("arena.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("arena");
    let mut compiler = AotCompiler::new("suspending-arena").unwrap();
    compiler
        .compile_module(&ast)
        .unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(
        fixture.path().join("arena.h"),
        crate::ffi::c_header(&ast).unwrap(),
    )
    .unwrap();
    let mut harness = String::from(
        r#"
        #include "arena.h"
        #include "joky_runtime.h"
        #include <assert.h>
        extern int32_t request_region(JokyCpsFrame *), request_nested(JokyCpsFrame *),
            request_transfer(JokyCpsFrame *), request_loop(JokyCpsFrame *), request_failure(JokyCpsFrame *);
        static int ready, failing, starts, stops[3], refunds;
        static int32_t *active;
        static int32_t poll_wait(JokyCpsFrame *frame) {
            int32_t *value = (int32_t *)(uintptr_t)__joky_cps_slots(frame)[0];
            if (!joky_cps_state(frame)) {
                assert(!active && *value == 40);
                active = value; ++starts;
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }
            assert(active == value && *value == 40);
            if (!ready) return JOKY_CPS_PENDING;
            if (failing) return 7;
            *value += 2;
            return JOKY_CPS_COMPLETED;
        }
        static void stop_wait(JokyCpsFrame *frame, uint32_t reason) {
            assert(reason < 3);
            assert(active == (int32_t *)(uintptr_t)__joky_cps_slots(frame)[0]);
            assert(*active == (reason == 0 ? 42 : 40));
            active = 0; ++stops[reason];
        }
        const JokyBorrowingHostV1 __joky_cps_borrow_v1_arena_wait = {1, poll_wait, stop_wait};
        void arena_probe(int32_t stage) { assert(stage == 1 && !active); ++refunds; }
        static void reset(void) {
            assert(!active); ready = failing = starts = refunds = 0;
            stops[0] = stops[1] = stops[2] = 0;
            joky_runtime_error_clear();
        }
        static void complete(JokyCpsEntry entry, size_t slots, int expected_waits, int expected_refunds) {
            reset(); ready = 1;
            JokyCpsFrame *frame = joky_cps_frame_new(slots);
            int status = JOKY_CPS_PENDING;
            for (int i = 0; i < 10000 && status == JOKY_CPS_PENDING; ++i) status = entry(frame);
            assert(status == JOKY_CPS_COMPLETED && joky_cps_result(frame) == 42);
            joky_cps_frame_free(frame);
            assert(!active && !joky_runtime_error_code());
            assert(starts == expected_waits && stops[0] == expected_waits && !stops[1] && !stops[2]);
            assert(refunds == expected_refunds);
        }
        static void interrupted(JokyCpsEntry entry, size_t slots, int fail) {
            reset();
            JokyCpsFrame *frame = joky_cps_frame_new(slots);
            for (int i = 0; i < 10 && !starts; ++i) assert(entry(frame) == JOKY_CPS_PENDING);
            assert(starts == 1);
            for (int i = 0; i < 3; ++i) assert(entry(frame) == JOKY_CPS_PENDING && starts == 1);
            if (fail) { ready = failing = 1; assert(entry(frame) == 7); }
            joky_cps_frame_free(frame);
            assert(!active && stops[fail ? 1 : 2] == 1 && !stops[0] && !refunds);
            joky_runtime_error_clear();
        }
        int main(void) {
    "#,
    );
    for (name, waits, refunds) in [
        ("request_region", 1, 1),
        ("request_nested", 0, 0),
        ("request_transfer", 1, 0),
        ("request_loop", 1000, 1000),
    ] {
        let slots = compiler.cps_frame_slots(name).unwrap();
        harness.push_str(&format!("complete({name}, {slots}, {waits}, {refunds});\n"));
    }
    let slots = compiler.cps_frame_slots("request_region").unwrap();
    harness.push_str(&format!("for (int i = 0; i < 20; ++i) {{ interrupted(request_region, {slots}, 0); interrupted(request_region, {slots}, 1); }}\n"));
    let slots = compiler.cps_frame_slots("request_failure").unwrap();
    harness.push_str(&format!(r#"
        reset(); ready = 1;
        JokyCpsFrame *frame = joky_cps_frame_new({slots});
        int status = JOKY_CPS_PENDING;
        for (int i = 0; i < 10 && status == JOKY_CPS_PENDING; ++i) status = request_failure(frame);
        assert(status != JOKY_CPS_COMPLETED && status != JOKY_CPS_PENDING && joky_runtime_error_code());
        joky_cps_frame_free(frame);
        assert(!active && stops[0] == 1);
        return 0;
        }}
    "#));
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
