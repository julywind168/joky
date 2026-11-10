//! Duration source APIs and native nanosecond timers link in a standalone program.
use super::*;

#[test]
#[cfg(unix)]
fn duration_native_values_timers_and_timeout_cleanup() {
    check_arena_cps_fixture(
        "durations",
        include_str!("../../../../tests/fixtures/durations.jk"),
        &[
            ("duration_success", "-1,40", 2),
            ("duration_owned_result", "-1,40", 2),
            ("duration_place_result", "40", 1),
        ],
        |compiler| {
            let mut extra = String::from(
                "extern int32_t duration_values(void); if (duration_values() != 42) return 30;\n",
            );
            for name in ["duration_elapsed", "duration_zero", "duration_sleep"] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(r#"{{
                extern int32_t {name}(JokyCpsFrame *);
                ready = 0; count = 0;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (joky_cps_block_on({name}, frame) != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42) return 31;
                joky_cps_frame_free(frame);
                if (count && (count != 2 || events[0] != -1 || events[1] != 40)) return 32;
            }}"#));
            }
            for name in [
                "duration_success",
                "duration_owned_result",
                "duration_failure",
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(
                    r#"{{
                extern int32_t {name}(JokyCpsFrame *);
                ready = 0; count = 0;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if ({name}(frame) != JOKY_CPS_PENDING) return 33;
                joky_cps_frame_free(frame);
                if (count != 2 || events[0] != -1 || events[1] != 40) return 34;
            }}"#
                ));
            }
            let slots = compiler.cps_frame_slots("duration_failure").unwrap();
            extra.push_str(&format!(
                r#"{{
            extern int32_t duration_failure(JokyCpsFrame *);
            ready = 0; count = 0;
            JokyCpsFrame *frame = joky_cps_frame_new({slots});
            if (duration_failure(frame) != JOKY_CPS_PENDING) return 35;
            ready = 1;
            if (duration_failure(frame) != 7) return 36;
            joky_cps_frame_free(frame);
            if (count != 2 || events[1] != 40) return 37;
            joky_runtime_error_clear();
        }}"#
            ));
            extra
        },
    );
}
