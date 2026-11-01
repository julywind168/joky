//! Standalone code uses the same descriptor proof and local storage ABI.
use super::*;

#[test]
#[cfg(unix)]
fn local_slice_descriptors_native_lifetimes() {
    for (name, source) in [
        (
            "local-slices",
            include_str!("../../../../tests/fixtures/local_slice_descriptors.jk"),
        ),
        (
            "local-fixed-slices",
            include_str!("../../../../tests/fixtures/local_fixed_slice_descriptors.jk"),
        ),
    ] {
        check_arena_cps_fixture(
            name,
            source,
            &[("descriptor_suspend", "0,0,0", 3)],
            |compiler| {
                let mut checks = String::from(
                "extern int32_t descriptor_loop(void), descriptor_previous(void), descriptor_returned(void);\n\
                 if (descriptor_loop() != 42 || descriptor_previous() != 42 || descriptor_returned() != 42) return 11;\n",
            );
                for entry in [
                    "descriptor_suspend",
                    "descriptor_failure",
                    "descriptor_bounds",
                ] {
                    let slots = compiler.cps_frame_slots(entry).unwrap();
                    checks.push_str(&format!(
                        "{{ extern int32_t {entry}(JokyCpsFrame *); ready = 0;\n\
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});\n\
                    if ({entry}(frame) != JOKY_CPS_PENDING) return 12;\n\
                    joky_cps_frame_free(frame); }}\n"
                    ));
                    if entry != "descriptor_suspend" {
                        checks.push_str(&format!(
                            "{{ extern int32_t {entry}(JokyCpsFrame *); ready = 1;\n\
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});\n\
                        if ({entry}(frame) != JOKY_CPS_PENDING) return 13;\n\
                        if ({entry}(frame) <= JOKY_CPS_PENDING) return 14;\n\
                        joky_cps_frame_free(frame); joky_runtime_error_clear(); }}\n"
                        ));
                    }
                }
                checks
            },
        );
    }
}
