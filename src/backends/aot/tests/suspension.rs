//! Suspension AOT fixtures.
use super::*;

#[test]
fn race_executes_in_standalone_native_code() {
    let cases = [
        ("race_first", "1, 2, 101", 3),
        ("race_later", "1, 2, 3, 11, 102, 9", 6),
        ("race_nested", "1, 2, 3, 4, 5, 11, 12, 13, 14, 105, 9", 11),
        ("race_inside_scope", "1, 2, 6, 101, 700, 16, 206", 7),
        ("race_loop", "1, 2, 101, 1, 2, 101, 1, 2, 101", 9),
        ("race_arguments", "1, 2, 2, 1, 3, 101", 6),
        ("race_single", "3, 103", 2),
        ("race_generic", "0, 1, 2", 3),
        ("race_reference", "1, 2, 11, 102, 9", 5),
        ("race_allocations", "1, 2, 11, 102, 9", 5),
    ];
    check_arena_cps_fixture(
        "race",
        include_str!("../../../../tests/fixtures/race.jk"),
        &cases,
        |compiler| {
            let mut extra = String::new();
            for (name, expected, length) in [
                ("race_immediate", "1, 2, 3, 1, 2", 5),
                ("race_skip_failure", "1", 1),
                ("race_return_argument", "1", 1),
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(
                    r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_COMPLETED || count != {length}) return 20;
                        if (joky_cps_result(frame) != 42 || joky_runtime_error_code()) return 21;
                        const int32_t expected[] = {{{expected}}};
                        for (int i = 0; i < {length}; ++i) if (events[i] != expected[i]) return 22;
                        if ({name}(frame) != JOKY_CPS_INVALID_STATE) return 23;
                        joky_cps_frame_free(frame);
                    }}
                "#
                ));
            }
            for (name, expected, length) in [
                ("race_failure", "1, 2, 3, 11", 4),
                ("race_argument_failure", "1", 1),
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        __joky_cps_slots(frame)[0] = 0;
                        if ({name}(frame) != JOKY_CPS_PENDING) return 24;
                        ready = 1;
                        if ({name}(frame) != JOKY_CPS_RUNTIME_ERROR) return 25;
                        if (joky_cps_state(frame) != JOKY_CPS_FAILED || !joky_runtime_error_code()) return 26;
                        const int32_t expected[] = {{{expected}}};
                        if (count != {length}) return 27;
                        for (int i = 0; i < {length}; ++i) if (events[i] != expected[i]) return 28;
                        joky_runtime_error_clear();
                        if ({name}(frame) != JOKY_CPS_INVALID_STATE) return 29;
                        joky_cps_frame_free(frame);
                    }}
                "#));
            }
            let slots = compiler.cps_frame_slots("race_host_failure").unwrap();
            extra.push_str(&format!(
                r#"
                {{
                    extern int32_t race_host_failure(JokyCpsFrame *);
                    ready = 0; count = 0;
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    if (race_host_failure(frame) != 7 || count != 1 || events[0] != 1) return 30;
                    if (joky_cps_state(frame) != JOKY_CPS_FAILED) return 31;
                    if (race_host_failure(frame) != JOKY_CPS_INVALID_STATE) return 32;
                    joky_cps_frame_free(frame);
                }}
            "#
            ));
            for name in ["race_nested", "race_allocations"] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(
                    r#"
                    {{
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_PENDING) return 33;
                        joky_cps_frame_free(frame);
                    }}
                "#
                ));
            }
            extra
        },
    );
}

#[cfg(unix)]
#[test]
fn async_borrowing_executes_in_standalone_native_code() {
    let source = include_str!("../../../../tests/fixtures/async_borrowing.jk");
    let ast = crate::parser::parse(source).unwrap();
    let fixture = ScratchDir::new("async-borrowing");
    let dir = fixture.path();
    let object = dir.join("program.o");
    let host = dir.join("host.c");
    let executable = dir.join("program");
    let mut compiler = AotCompiler::new("async-borrowing").unwrap();
    compiler.compile_module(&ast).unwrap();
    assert!(compiler.requires_runtime());
    std::fs::write(
        dir.join("async_borrowing.h"),
        crate::ffi::c_header(&ast).unwrap(),
    )
    .unwrap();
    let mut harness = String::from(include_str!("../../../../tests/fixtures/async_borrowing.c"));
    harness.push_str(
        r#"
        static void check(JokyCpsEntry entry, size_t slots, int read, int timer,
                          int expected_starts, int completed, int cancelled) {
            async_reset(); joky_runtime_error_clear();
            JokyCpsFrame *frame = joky_cps_frame_new(slots);
            int status = entry(frame);
            if (status == JOKY_CPS_PENDING) {
                int initial_starts = async_count(0);
                for (int i = 0; i < 4; ++i) {
                    assert(entry(frame) == JOKY_CPS_PENDING);
                    assert(async_count(0) == initial_starts);
                }
                async_tick(); async_ready(read, timer);
                for (int i = 0; i < 100 && status == JOKY_CPS_PENDING; ++i) status = entry(frame);
            }
            assert(status == JOKY_CPS_COMPLETED && joky_cps_state(frame) == JOKY_CPS_DONE);
            assert(joky_cps_result(frame) == 42 && !joky_runtime_error_code());
            assert(async_count(0) == expected_starts && async_count(1) == completed);
            assert(async_count(2) == 0 && async_count(3) == cancelled && async_count(5) == 0);
            async_tick();
            assert(entry(frame) == JOKY_CPS_INVALID_STATE);
            __joky_cps_finish(frame); __joky_cps_finish(frame); joky_cps_frame_free(frame);
            assert(async_count(1) == completed && async_count(3) == cancelled);
        }
    "#,
    );
    for name in [
        "sequential",
        "race_timeout",
        "nested",
        "shared",
        "distinct",
        "unstarted",
        "immediate",
        "widths",
        "repeated",
        "start_failure",
        "pending_failure",
        "sibling_failure",
        "error_channel",
    ] {
        harness.push_str(&format!("extern int32_t {name}(JokyCpsFrame *);\n"));
    }
    harness.push_str("int main(void) { for (int run = 0; run < 10; ++run) {\n");
    for (name, read, timer, starts, completed, cancelled) in [
        ("sequential", 1, 0, 1, 1, 0),
        ("race_timeout", 0, 1, 1, 0, 1),
        ("race_timeout", 1, 0, 1, 1, 0),
        ("nested", 0, 1, 1, 0, 1),
        ("shared", 1, 0, 2, 2, 0),
        ("distinct", 1, 0, 2, 2, 0),
        ("unstarted", 0, 0, 0, 0, 0),
        ("immediate", 0, 0, 1, 1, 0),
        ("widths", 0, 0, 1, 1, 0),
        ("repeated", 0, 1, 20, 0, 20),
    ] {
        let slots = compiler.cps_frame_slots(name).unwrap();
        harness.push_str(&format!(
            "check({name}, {slots}, {read}, {timer}, {starts}, {completed}, {cancelled});\n"
        ));
    }
    for name in ["sequential", "nested", "shared"] {
        let slots = compiler.cps_frame_slots(name).unwrap();
        let expected = if name == "shared" { 2 } else { 1 };
        harness.push_str(&format!(
            r#"{{
            async_reset();
            JokyCpsFrame *frame = joky_cps_frame_new({slots});
            assert({name}(frame) == JOKY_CPS_PENDING); async_tick();
            joky_cps_frame_free(frame);
            assert(async_count(3) == {expected} && async_count(5) == 0); async_tick();
        }}"#
        ));
    }
    for (name, status, reason, pending) in [
        ("start_failure", 7, 2, false),
        ("pending_failure", 7, 2, true),
        ("sibling_failure", 3, 3, true),
        ("error_channel", 3, 2, false),
    ] {
        let slots = compiler.cps_frame_slots(name).unwrap();
        let pending = i32::from(pending);
        harness.push_str(&format!(
            r#"{{
            async_reset(); joky_runtime_error_clear();
            JokyCpsFrame *frame = joky_cps_frame_new({slots});
            int status = {name}(frame);
            if ({pending}) {{ assert(status == JOKY_CPS_PENDING); async_ready(1, 1);
                if ({reason} == 3) async_ready(0, 1);
                status = {name}(frame); }}
            assert(status == {status} && joky_cps_state(frame) == JOKY_CPS_FAILED);
            assert(async_count({reason}) == 1 && async_count(5) == 0);
            if ({status} == 3) assert(joky_runtime_error_code());
            __joky_cps_finish(frame); joky_cps_frame_free(frame);
            assert(async_count({reason}) == 1); async_tick();
            joky_runtime_error_clear();
        }}"#
        ));
    }
    harness.push_str("} return 0; }\n");
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(
            Command::new("cc")
                .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
                .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
                .arg(&host)
                .arg(&object),
        ),
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

#[test]
fn arena_suspension_executes_in_standalone_native_code() {
    let cases = [
        ("arena_suspend", "10, 30", 2),
        ("arena_return_suspend", "20, 0, 20, 0, 0", 5),
        ("arena_cycle_suspend", "20, 22, 0", 3),
        ("arena_loop_suspend", "13, 0, 13, 0, 13, 0", 6),
        ("arena_scope_wait", "42, 0, 0", 3),
    ];
    check_arena_cps_fixture(
        "arena-suspension",
        include_str!("../../../../tests/fixtures/arena_suspension.jk"),
        &cases,
        |compiler| {
            let mut extra = String::new();
            for (name, code) in [("arena_fail_suspend", 3), ("arena_host_fail_suspend", 7)] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_PENDING) return 11;
                        ready = 1;
                        if ({name}(frame) != {code} || joky_cps_state(frame) != JOKY_CPS_FAILED) return 12;
                        joky_runtime_error_clear();
                        if ({name}(frame) != JOKY_CPS_INVALID_STATE) return 13;
                        joky_cps_frame_free(frame);
                    }}
                "#));
            }
            let slots = compiler.cps_frame_slots("arena_cycle_suspend").unwrap();
            extra.push_str(&format!(
                r#"
                ready = 0; count = 0;
                JokyCpsFrame *cancelled = joky_cps_frame_new({slots});
                if (arena_cycle_suspend(cancelled) != JOKY_CPS_PENDING) return 14;
                joky_cps_frame_free(cancelled);
            "#
            ));
            extra
        },
    );
}

#[test]
fn arena_scope_executes_in_standalone_native_code() {
    let cases = [
        ("scope_shared", "20, 0, 20, 1, 20, 20", 6),
        ("scope_distinct", "0, 0, 21, 21", 4),
        ("scope_nested", "0, 20, 1, 20, 2, 21, 20, 20", 8),
        ("scope_graph", "20, 0, 22, 1, 22, 20", 6),
        ("scope_aggregate", "0, 1, 2", 3),
        ("scope_retained", "0, 1, 21, 21", 4),
        ("scope_allocations", "20, 20, 22, 22, 42, 42, 20, 22, 42", 9),
        ("scope_partial", "20, 20, 22, 1, 20, 2, 22", 7),
        ("scope_loop", "6, 6, 7, 7, 6, 6, 14, 14, 6, 6, 21, 21", 12),
        ("scope_arguments", "0, -1, -1, 0, 0, 21, 21", 7),
    ];
    check_arena_cps_fixture(
        "arena-scope",
        include_str!("../../../../tests/fixtures/arena_scope.jk"),
        &cases,
        |compiler| {
            let partial = compiler.cps_frame_slots("scope_partial").unwrap();
            let failure = compiler.cps_frame_slots("scope_failure").unwrap();
            let argument = compiler.cps_frame_slots("scope_argument_failure").unwrap();
            format!(
                r#"
                extern int32_t scope_failure(JokyCpsFrame *);
                extern int32_t scope_argument_failure(JokyCpsFrame *);
                ready = 0; count = 0;
                JokyCpsFrame *partial = joky_cps_frame_new({partial});
                if (scope_partial(partial) != JOKY_CPS_PENDING || count != 4) return 20;
                ready = 1;
                if (scope_partial(partial) != JOKY_CPS_PENDING || count != 6) return 21;
                if (events[4] != 20 || events[5] != 2) return 22;
                /* Cancel after one branch completes and another is still pending. */
                joky_cps_frame_free(partial);

                ready = 0; count = 0;
                JokyCpsFrame *failed = joky_cps_frame_new({failure});
                __joky_cps_slots(failed)[0] = 0;
                if (scope_failure(failed) != JOKY_CPS_PENDING) return 23;
                ready = 1;
                if (scope_failure(failed) != JOKY_CPS_RUNTIME_ERROR) return 24;
                if (joky_cps_state(failed) != JOKY_CPS_FAILED || !joky_runtime_error_code()) return 25;
                const int32_t expected[] = {{20, 20, 22, 1, 3, 40, 40, 20, 2}};
                if (count != 9) return 26;
                for (int i = 0; i < 9; ++i) if (events[i] != expected[i]) return 27;
                joky_runtime_error_clear();
                if (scope_failure(failed) != JOKY_CPS_INVALID_STATE) return 28;
                joky_cps_frame_free(failed);

                ready = 0; count = 0;
                JokyCpsFrame *argument = joky_cps_frame_new({argument});
                __joky_cps_slots(argument)[0] = 0;
                if (scope_argument_failure(argument) != JOKY_CPS_PENDING) return 29;
                ready = 1;
                if (scope_argument_failure(argument) != JOKY_CPS_RUNTIME_ERROR) return 30;
                if (joky_cps_state(argument) != JOKY_CPS_FAILED || count != 1 || events[0] != 1) return 31;
                joky_runtime_error_clear();
                joky_cps_frame_free(argument);
            "#
            )
        },
    );
}

#[test]
fn race_values_execute_and_clean_up_in_standalone_native_code() {
    check_arena_cps_fixture(
        "race-values",
        include_str!("../../../../tests/fixtures/race_values.jk"),
        &[("host_scalar", "41, 99", 2)],
        |compiler| {
            let mut harness = String::new();
            for (name, failure) in [
                ("scalar", false),
                ("host_scalar", false),
                ("widths", false),
                ("copy_aggregate", false),
                ("zero_slots", false),
                ("string_value", false),
                ("owned", false),
                ("unpolled", false),
                ("discard", false),
                ("pair_owned", false),
                ("arrays", false),
                ("results", false),
                ("nested", false),
                ("indirect", false),
                ("members", false),
                ("repeat", false),
                ("generic_context", false),
                ("cleanup_failure", true),
                ("winner_failure", true),
                ("branch_failure", true),
                ("cleanup_failure_unit", true),
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                let failure = i32::from(failure);
                harness.push_str(&format!(r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 1; count = 0; joky_runtime_error_clear();
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        int status = JOKY_CPS_PENDING;
                        for (int i = 0; i < 1000 && status == JOKY_CPS_PENDING; ++i) status = {name}(frame);
                        if ({failure} != !!joky_runtime_error_code()) return 40;
                        if (!{failure} && (status != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42)) return 41;
                        if ({failure} && status != JOKY_CPS_RUNTIME_ERROR) return 42;
                        for (int id = 1; id <= 9; ++id) {{
                            int created = 0, dropped = 0;
                            for (int i = 0; i < count; ++i) {{
                                created += events[i] == -id;
                                dropped += events[i] == id;
                                if ({failure} && events[i] == 100) return 43;
                            }}
                            if (created != dropped) return 44;
                        }}
                        joky_cps_frame_free(frame);
                        joky_runtime_error_clear();
                    }}
                "#));
            }
            for name in [
                "owned",
                "pair_owned",
                "arrays",
                "results",
                "nested",
                "indirect",
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                harness.push_str(&format!(
                    r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_PENDING) return 45;
                        joky_cps_frame_free(frame);
                        for (int id = 1; id <= 4; ++id) {{
                            int created = 0, dropped = 0;
                            for (int i = 0; i < count; ++i) {{
                                created += events[i] == -id;
                                dropped += events[i] == id;
                            }}
                            if (created != dropped || dropped > 1) return 46;
                        }}
                        if (joky_runtime_error_code()) return 47;
                    }}
                "#
                ));
            }
            harness
        },
    );
}

#[test]
fn structured_values_execute_and_clean_up_in_standalone_native_code() {
    check_arena_cps_fixture(
        "structured-values",
        include_str!("../../../../tests/fixtures/structured_values.jk"),
        &[("host_scalar", "19, 21", 2)],
        |compiler| {
            let mut harness = String::new();
            for (name, failure) in [
                ("scope_scalar", false),
                ("scope_owned", false),
                ("scope_return", false),
                ("mixed", false),
                ("aggregates", false),
                ("host_scalar", false),
                ("owned", false),
                ("discard", false),
                ("arrays", false),
                ("results", false),
                ("nested", false),
                ("indirect", false),
                ("repeat", false),
                ("generic_context", false),
                ("scope_control_flow", false),
                ("members", false),
                ("scope_failure", true),
                ("branch_failure", true),
                ("cleanup_failure", true),
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                let failure = i32::from(failure);
                harness.push_str(&format!(r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 1; count = 0; joky_runtime_error_clear();
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        int status = JOKY_CPS_PENDING;
                        for (int i = 0; i < 1000 && status == JOKY_CPS_PENDING; ++i) status = {name}(frame);
                        if ({failure} != !!joky_runtime_error_code()) return 40;
                        if (!{failure} && (status != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42)) return 41;
                        if ({failure} && status != JOKY_CPS_RUNTIME_ERROR) return 42;
                        for (int id = 1; id <= 9; ++id) {{
                            int created = 0, dropped = 0;
                            for (int i = 0; i < count; ++i) {{
                                created += events[i] == -id;
                                dropped += events[i] == id;
                                if ({failure} && events[i] == 100) return 43;
                            }}
                            if (created != dropped) return 44;
                        }}
                        joky_cps_frame_free(frame);
                        joky_runtime_error_clear();
                    }}
                "#));
            }
            for name in [
                "owned",
                "scope_owned",
                "scope_return",
                "arrays",
                "results",
                "nested",
                "indirect",
            ] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                harness.push_str(&format!(
                    r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_PENDING) return 45;
                        joky_cps_frame_free(frame);
                        for (int id = 1; id <= 4; ++id) {{
                            int created = 0, dropped = 0;
                            for (int i = 0; i < count; ++i) {{
                                created += events[i] == -id;
                                dropped += events[i] == id;
                            }}
                            if (created != dropped || dropped > 1) return 46;
                        }}
                        if (joky_runtime_error_code()) return 47;
                    }}
                "#
                ));
            }
            harness
        },
    );
}
