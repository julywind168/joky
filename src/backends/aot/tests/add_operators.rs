use super::*;
#[test]
#[cfg(unix)]
fn add_operators_native_dispatch_failure_and_cleanup() {
    check_arena_cps_fixture(
        "add-operators",
        include_str!("../../../../tests/fixtures/add_operators.jk"),
        &[("add_pending", "120,21,122,300,20,22,42", 7)],
        |compiler| {
            let slots = compiler.cps_frame_slots("add_failed_rhs").unwrap();
            format!(
                r#"
                extern int32_t add_values(void), add_overflow(void);
                if (add_values() != 42) return 30;
                extern int32_t add_failed_rhs(JokyCpsFrame *);
                ready = 0; count = 0;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (add_failed_rhs(frame) != JOKY_CPS_PENDING) return 31;
                ready = 1;
                if (add_failed_rhs(frame) != 7) return 32;
                joky_cps_frame_free(frame);
                if (count != 3 || events[0] != 120 || events[1] != 21 || events[2] != 20) return 33;
                joky_runtime_error_clear();
                add_overflow();
                if (!joky_runtime_error_code()) return 34;
                joky_runtime_error_clear();
            "#
            )
        },
    );
}
