//! Dynamic CPS descriptors relocate with their containing native module.
use super::*;

#[test]
#[cfg(unix)]
fn suspending_dyn_execute_relocated_entries_and_cleanup() {
    check_arena_cps_fixture(
        "suspending-dyn",
        include_str!("../../../../tests/fixtures/suspending_dyn.jk"),
        &[
            ("dyn_remote", "-1,19,19,20,0", 5),
            ("dyn_memory", "19,19,20,0", 4),
            ("dyn_result", "-1,-2,40", 3),
            ("dyn_nested", "-1,-2,40", 3),
            ("dyn_text", "-1,40", 2),
            ("dyn_argument", "-1,39,40", 3),
            ("dyn_place", "-1,40", 2),
        ],
        |compiler| {
            let mut extra = String::new();
            for name in [
                "dyn_remote",
                "dyn_result",
                "dyn_nested",
                "dyn_place",
                "dyn_failure",
                "dyn_race",
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(
                    r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        int status = {name}(frame);
                        if (status != JOKY_CPS_PENDING && status != JOKY_CPS_COMPLETED) return 20;
                        joky_cps_frame_free(frame);
                        for (int id = 1; id <= 2; ++id) {{
                            int made = 0, dropped = 0;
                            for (int i = 0; i < count; ++i) {{
                                made += events[i] == -id;
                                dropped += events[i] == id;
                            }}
                            if (dropped != 0 || made > 1) return 21;
                        }}
                    }}
                "#
                ));
            }
            let slots = compiler.cps_frame_slots("dyn_failure").unwrap();
            extra.push_str(&format!(r#"
                {{
                    extern int32_t dyn_failure(JokyCpsFrame *);
                    ready = 0; count = 0;
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    if (dyn_failure(frame) != JOKY_CPS_PENDING) return 22;
                    ready = 1;
                    if (dyn_failure(frame) != 7) return 23;
                    joky_cps_frame_free(frame);
                    if (count != 3 || events[0] != -1 || events[1] != -2 || events[2] != 40) return 24;
                    joky_runtime_error_clear();
                }}
            "#));
            extra
        },
    );
}
