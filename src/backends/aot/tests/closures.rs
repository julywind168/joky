//! Closure ownership and hidden entries link without compiler-process addresses.
use super::*;

#[test]
#[cfg(unix)]
fn closure_owning_cleanup_executes_in_standalone_native_code() {
    let fixture = ScratchDir::new("closure-cleanup");
    let object = fixture.path().join("closures.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("closures");
    let mut compiler = AotCompiler::new("closure-cleanup").unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(include_str!("../../../../tests/fixtures/closures.jk")).unwrap(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        static int32_t events[64]; static int count;
        void closure_event(int32_t value) { if (count < 64) events[count++] = value; }
        extern int32_t unused(void), consume(void), repeated(void), skipped(void);
        extern int32_t consumed_helper(void), failure(void), cancellation_work(void), consume_last(void), consuming_failure(void);
        extern int32_t cleanup_failure(void), failure_before_cleanup_failure(void);
        extern int32_t argument_order(void), consuming_argument_failure(void);
        extern int32_t lazy_skipped(void), lazy_consumed(void), lazy_error_consumed(void);
        extern int32_t query_owned(void), query_temporary(void), lazy_failure(void), lazy_cancellation_work(void);
        extern int32_t stored_work(void), stored_unused(void), stored_replace(void), stored_failure(void), stored_drop_failure(void), stored_return_drop_failure(void), stored_empty_drop(void);
        int main(void) {
            struct Case { int32_t (*call)(void); int fails, count; int32_t events[6]; } cases[] = {
                {stored_empty_drop, 0, 1, {7}},
                {stored_work, 0, 4, {-1,-2,2,1}}, {stored_unused, 0, 2, {-1,1}},
                {stored_replace, 0, 4, {-1,-2,1,2}}, {stored_failure, 1, 4, {-1,-2,1,2}},
                {stored_return_drop_failure, 1, 4, {-1,-4,4,1}},
                {stored_drop_failure, 1, 4, {-4,-5,4,5}},
                {lazy_skipped, 0, 4, {-1,1,-2,2}}, {lazy_consumed, 0, 2, {-1,1}},
                {lazy_cancellation_work, 0, 4, {-1,-2,2,1}}, {lazy_error_consumed, 0, 2, {-1,1}}, {query_owned, 0, 4, {-1,-2,2,1}},
                {query_temporary, 0, 3, {-1,1,100}}, {lazy_failure, 1, 4, {-1,-2,1,2}},
                {unused, 0, 4, {-1,-2,1,2}}, {consume, 0, 2, {-1,1}},
                {repeated, 0, 2, {-1,1}}, {skipped, 0, 2, {-1,1}},
                {consumed_helper, 0, 4, {-1,1,-2,2}}, {failure, 1, 4, {-1,-2,1,2}},
                {cancellation_work, 0, 4, {-1,-2,2,1}}, {consume_last, 0, 6, {-1,-2,-3,3,1,2}}, {consuming_failure, 1, 6, {-1,-2,-3,1,2,3}},
                {cleanup_failure, 1, 6, {-4,-5,-1,4,5,1}}, {failure_before_cleanup_failure, 1, 6, {-4,-5,-1,4,5,1}},
                {argument_order, 0, 4, {100,200,300,400}}, {consuming_argument_failure, 1, 2, {-1,1}}
            };
            for (unsigned run = 0; run < 10; ++run) {
                for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); ++i) {
                    count = 0; joky_runtime_error_clear();
                    int32_t result = cases[i].call();
                    if (cases[i].fails != !!joky_runtime_error_code()) return 1;
                    if (!cases[i].fails && result != 42) return 2;
                    if (count != cases[i].count) return 3;
                    for (int j = 0; j < count; ++j) if (events[j] != cases[i].events[j]) return 4;
                }
            }
            return 0;
        }
    "#,
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn suspending_closures_execute_relocated_entries_and_cleanup() {
    check_arena_cps_fixture(
        "suspending-closures",
        include_str!("../../../../tests/fixtures/suspending_closures.jk"),
        &[
            ("closure_direct", "-1,100,40,1", 4),
            ("closure_transfer", "-1,40,1", 3),
            ("closure_nested", "-1,-2,40,1,2", 5),
            ("closure_argument", "-1,39,40,1", 4),
            ("closure_pointer", "41", 1),
            ("closure_named", "41", 1),
            ("closure_unused", "-1,41,1", 3),
            ("closure_reference_argument", "40", 1),
            ("closure_factory", "41", 1),
        ],
        |compiler| {
            let mut extra = String::new();
            for name in [
                "closure_direct",
                "closure_transfer",
                "closure_nested",
                "closure_argument",
                "closure_unused",
                "closure_failure",
                "closure_race",
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
                            if (made != dropped || made > 1) return 21;
                        }}
                    }}
                "#
                ));
            }
            let slots = compiler.cps_frame_slots("closure_failure").unwrap();
            extra.push_str(&format!(r#"
                {{
                    extern int32_t closure_failure(JokyCpsFrame *);
                    ready = 0; count = 0;
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    if (closure_failure(frame) != JOKY_CPS_PENDING) return 22;
                    ready = 1;
                    if (closure_failure(frame) != 7) return 23;
                    joky_cps_frame_free(frame);
                    if (count != 3 || events[0] != -1 || events[1] != 40 || events[2] != 1) return 24;
                    joky_runtime_error_clear();
                }}
            "#));
            extra
        },
    );
}
