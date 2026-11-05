//! Aggregates AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn transparent_aliases_use_the_existing_native_and_cps_abis() {
    let mut source = include_str!("../../../../tests/fixtures/aliases.jk").to_owned();
    source.push_str(
        r#"
        type Context = Host.Context
        extern fn context() -> Context
        extern fn context_value(value: Context) -> Count
        extern fn wait_value(value: Count) @suspends -> Count
        extern fn inspect_alias_text(value: Text) -> Count
        fn alias_opaque() -> Count { let value: Context = context(); context_value(value) }
        fn alias_suspended(value: Count) @suspends -> Count {
            let saved: Text = alias_string()
            let result: Count = wait_value(value)
            let text: Text = alias_text(saved)
            alias_add(result, inspect_alias_text(text))
        }
    "#,
    );
    let fixture = ScratchDir::new("aliases");
    let dir = fixture.path();
    let object = dir.join("aliases.o");
    let host = dir.join("host.c");
    let program = dir.join("aliases");
    let mut compiler = AotCompiler::new("aliases").unwrap();
    compiler.register_opaque_type("Host.Context").unwrap();
    compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("alias_suspended").unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_vm.h"
        #include "joky_cps.h"
        #include "text.h"
        #include <stdbool.h>
        #include <string.h>
        static int ready, starts;
        uint64_t context(void) { return 42; }
        int32_t context_value(uint64_t value) { return (int32_t)value; }
        int32_t inspect_alias_text(const JokyText *text) {
            const uint8_t expected[] = { 'h', 0xc3, 0xa9, 0, 'e', 'n', 'd' };
            uint64_t length = text->byte_length;
            return length == sizeof expected && !memcmp(text->data, expected, sizeof expected) ? 10 : -100;
        }
        int32_t wait_value(JokyCpsFrame *frame) {
            if (joky_cps_state(frame) == 0) {
                starts++;
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0] + 1);
            return JOKY_CPS_COMPLETED;
        }
        extern int32_t alias_add(int32_t, int32_t), alias_compute(void), alias_choose(int32_t);
        extern int32_t alias_mixed(int32_t), alias_opaque(void), alias_suspended(JokyCpsFrame *);
        extern bool alias_flag(bool);
        extern void alias_unit(void);

        int main(void) {
            joky_runtime_error_clear();
            if (alias_add(20, 22) != 42 || alias_compute() != 7 || alias_choose(0) != 42) return 1;
            if (!alias_flag(false) || alias_flag(true) || alias_mixed(42) != 42) return 2;
            alias_unit();
            JokyOwnedTextV2 input = {0}, text = {0};
            if (__joky_user_text_v2_alias_string(&input)
                || __joky_user_text_v2_alias_text(input.data, input.byte_length, &text)) return 3;
            joky_string_result_free_v2(&input);
            const uint8_t expected[] = { 'h', 0xc3, 0xa9, 0, 'e', 'n', 'd' };
            int bad = text.byte_length != sizeof expected || memcmp(text.data, expected, sizeof expected);
            joky_string_result_free_v2(&text);
            if (bad) return 3;
            if (alias_opaque() != 42 || joky_runtime_error_code()) return 4;
            JokyCpsFrame *frame = joky_cps_frame_new(SLOTS);
            __joky_cps_slots(frame)[0] = 31;
            for (int i = 0; i < 4; i++) {
                joky_runtime_error_clear();
                if (alias_suspended(frame) != JOKY_CPS_PENDING || joky_runtime_error_code()) return 5;
            }
            ready = 1;
            joky_runtime_error_clear();
            if (alias_suspended(frame) != JOKY_CPS_COMPLETED || joky_runtime_error_code()) return 6;
            if (joky_cps_result(frame) != 42 || starts != 1) return 7;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#.replace("SLOTS", &slots.to_string());
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let status = Command::new(program).status().unwrap();
    assert!(status.success(), "native alias test exited with {status}");
}

#[test]
#[cfg(unix)]
fn records_execute_in_native_objects_and_cps_with_the_shared_runtime() {
    let source = include_str!("../../../../tests/fixtures/records.jk");
    for suspends in [false, true] {
        let mut source = with_suspends(source, suspends, Suspends::All);
        source.push_str(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            fn direct_record(x: Int32) -> Point { Point { x: x, y: x + 1 } }
            struct Deferred<T> { value: T, stamp: Int32 }
            fn defer<T>(value: T, n: Int32) @suspends -> Deferred<T> {
                Deferred { stamp: delayed_value(record(n)), value: value }
            }
            fn cps_record(p: Point) @suspends -> Point {
                Point { x: p.x, ..{
                    let saved = defer(p, p.y)
                    Point { y: saved.stamp, ..saved.value }
                } }
            }
            fn pending() @suspends -> Int32 {
                let p = cps_record(direct_record(10))
                let q = direct_record(p.y)
                p.x * 100 + q.y
            }
            fn bad_record(n: Int32) -> Point { Point { x: 1 / n, y: record(99) } }
            fn failing() @suspends -> Int32 {
                let p = cps_record(direct_record(-2))
                bad_record(p.y).x
            }
        "#,
        );
        let fixture = ScratchDir::new("records");
        let dir = fixture.path();
        let object = dir.join("records.o");
        let program = dir.join("records");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("records").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let pending_slots = compiler.cps_frame_slots("pending").unwrap();
        let failing_slots = compiler.cps_frame_slots("failing").unwrap();
        let compute_slots = compiler.cps_frame_slots("compute").unwrap_or(0);
        compiler.write_object(&object).unwrap();
        let compute = if suspends {
            format!(
                r#"
            JokyCpsFrame *compute_frame = joky_cps_frame_new({compute_slots});
            if (compute(compute_frame) != 0 || joky_cps_result(compute_frame) != 87915) return 1;
            joky_cps_frame_free(compute_frame);
        "#
            )
        } else {
            "if (compute() != 87915) return 1;".to_owned()
        };
        let harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
        #include "joky_vm.h"
            static int ready, events;
            int32_t record(int32_t x) { events++; return x; }
            int32_t delayed_value(JokyCpsFrame *frame) {
                if (joky_cps_state(frame) == 0) {
                    events++;
                    joky_cps_set_state(frame, 1);
                    return 1;
                }
                if (!ready) return 1;
                joky_cps_set_result(frame, (uint32_t)((int32_t)__joky_cps_slots(frame)[0] + 1));
                return 0;
            }
            extern int32_t compute(COMPUTE_ARGS);
            extern int32_t pending(JokyCpsFrame *), failing(JokyCpsFrame *);
            int main(void) {
                COMPUTE_BODY
                JokyCpsFrame *p = joky_cps_frame_new(PENDING_SLOTS);
                JokyCpsFrame *f = joky_cps_frame_new(FAILING_SLOTS);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || failing(f) != 1 || pending(cancelled) != 1) return 2;
                }
                if (events != 6) return 3;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 0 || joky_cps_result(p) != 1013) return 4;
                if (failing(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 5;
                if (joky_cps_state(f) != JOKY_CPS_FAILED || events != 6) return 6;
                joky_cps_frame_free(p);
                joky_cps_frame_free(f);
                return 0;
            }
        "#
        .replace(
            "COMPUTE_ARGS",
            if suspends { "JokyCpsFrame *" } else { "void" },
        )
        .replace("COMPUTE_BODY", &compute)
        .replace("PENDING_SLOTS", &pending_slots.to_string())
        .replace("FAILING_SLOTS", &failing_slots.to_string());
        std::fs::write(&host, harness).unwrap();
        link(
            link_runtime(
                Command::new("cc")
                    .arg(&host)
                    .arg(&object)
                    .arg("-I")
                    .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
            ),
            &program,
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn enums_execute_in_native_objects_and_cps_with_the_shared_runtime() {
    let source = include_str!("../../../../tests/fixtures/enums.jk");
    for suspends in [false, true] {
        let mut source = with_suspends(source, suspends, Suspends::All);
        source.push_str(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            enum AsyncValue { Empty, Value(Int32) }
            fn direct_enum(x: Int32) -> AsyncValue { AsyncValue.Value(x) }
            fn read_enum(e: AsyncValue) -> Int32 { match e { Value(x) => x, Empty => 0 } }
            fn cps_enum(e: AsyncValue) @suspends -> AsyncValue {
                match e {
                    Value(x) => AsyncValue.Value(delayed_value(record(x)))
                    Empty => AsyncValue.Empty
                }
            }
            fn pending() @suspends -> Int32 {
                let e = cps_enum(direct_enum(11))
                match e { Value(x) => delayed_value(record(x)), Empty => record(99) }
            }
            fn bad_enum(n: Int32) -> AsyncValue { AsyncValue.Value(1 / n; record(99)) }
            fn failing() @suspends -> Int32 {
                let e = cps_enum(direct_enum(-1))
                read_enum(bad_enum(read_enum(e)))
            }
        "#,
        );
        let fixture = ScratchDir::new("enums");
        let dir = fixture.path();
        let object = dir.join("enums.o");
        let program = dir.join("enums");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("enums").unwrap();
        compiler.set_prelude_enabled(false).unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let pending_slots = compiler.cps_frame_slots("pending").unwrap();
        let failing_slots = compiler.cps_frame_slots("failing").unwrap();
        let compute_slots = compiler.cps_frame_slots("compute").unwrap_or(0);
        compiler.write_object(&object).unwrap();
        let compute = if suspends {
            format!(
                r#"
            JokyCpsFrame *compute_frame = joky_cps_frame_new({compute_slots});
            if (compute(compute_frame) != 0 || joky_cps_result(compute_frame) != 2498) return 1;
            joky_cps_frame_free(compute_frame);
        "#
            )
        } else {
            "if (compute() != 2498) return 1;".to_owned()
        };
        let harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
        #include "joky_vm.h"
            static int ready, events;
            int32_t record(int32_t x) { events++; return x; }
            int32_t delayed_value(JokyCpsFrame *frame) {
                if (joky_cps_state(frame) == 0) {
                    events++;
                    joky_cps_set_state(frame, 1);
                    return 1;
                }
                if (!ready) return 1;
                joky_cps_set_result(frame, (uint32_t)((int32_t)__joky_cps_slots(frame)[0] + 1));
                return 0;
            }
            extern int32_t compute(COMPUTE_ARGS);
            extern int32_t pending(JokyCpsFrame *), failing(JokyCpsFrame *);
            int main(void) {
                COMPUTE_BODY
                JokyCpsFrame *p = joky_cps_frame_new(PENDING_SLOTS);
                JokyCpsFrame *f = joky_cps_frame_new(FAILING_SLOTS);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || failing(f) != 1 || pending(cancelled) != 1) return 2;
                }
                if (events != 6) return 3;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 1 || events != 8) return 4;
                if (pending(p) != 0 || joky_cps_result(p) != 13) return 7;
                if (failing(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 5;
                if (joky_cps_state(f) != JOKY_CPS_FAILED || events != 8) return 6;
                joky_cps_frame_free(p);
                joky_cps_frame_free(f);
                return 0;
            }
        "#
        .replace(
            "COMPUTE_ARGS",
            if suspends { "JokyCpsFrame *" } else { "void" },
        )
        .replace("COMPUTE_BODY", &compute)
        .replace("PENDING_SLOTS", &pending_slots.to_string())
        .replace("FAILING_SLOTS", &failing_slots.to_string());
        std::fs::write(&host, harness).unwrap();
        link(
            link_runtime(
                Command::new("cc")
                    .arg(&host)
                    .arg(&object)
                    .arg("-I")
                    .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
            ),
            &program,
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn tuples_execute_in_native_objects_and_cps_with_the_shared_runtime() {
    let source = include_str!("../../../../tests/fixtures/tuples.jk");
    for suspends in [false, true] {
        let mut source = with_suspends(source, suspends, Suspends::Typed);
        source.push_str(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            fn direct_tuple(x: Int32) -> (Int32, Bool) { (x, true) }
            fn cps_tuple(pair: (Int32, Bool)) @suspends -> (Int32, Bool) {
                let (x, flag) = pair;
                (delayed_value(record(x)), flag)
            }
            fn pending() @suspends -> Int32 {
                let (x, flag) = cps_tuple(direct_tuple(11))
                if flag { delayed_value(record(x)) } else { record(99) }
            }
            fn bad_tuple(n: Int32) -> (Int32, Bool) { (1 / n; record(99), true) }
            fn failing() @suspends -> Int32 {
                let (x, _) = cps_tuple(direct_tuple(-1))
                let (y, _) = bad_tuple(x)
                y
            }
        "#,
        );
        let fixture = ScratchDir::new("tuples");
        let dir = fixture.path();
        let object = dir.join("tuples.o");
        let program = dir.join("tuples");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("tuples").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let pending_slots = compiler.cps_frame_slots("pending").unwrap();
        let failing_slots = compiler.cps_frame_slots("failing").unwrap();
        let compute_slots = compiler.cps_frame_slots("compute").unwrap_or(0);
        compiler.write_object(&object).unwrap();
        let compute = if suspends {
            format!(
                r#"
            JokyCpsFrame *compute_frame = joky_cps_frame_new({compute_slots});
            if (compute(compute_frame) != 0 || joky_cps_result(compute_frame) != 22813) return 1;
            joky_cps_frame_free(compute_frame);
        "#
            )
        } else {
            "if (compute() != 22813) return 1;".to_owned()
        };
        let harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
        #include "joky_vm.h"
            static int ready, events;
            int32_t record(int32_t x) { events++; return x; }
            int32_t delayed_value(JokyCpsFrame *frame) {
                if (joky_cps_state(frame) == 0) {
                    events++;
                    joky_cps_set_state(frame, 1);
                    return 1;
                }
                if (!ready) return 1;
                joky_cps_set_result(frame, (uint32_t)((int32_t)__joky_cps_slots(frame)[0] + 1));
                return 0;
            }
            extern int32_t compute(COMPUTE_ARGS);
            extern int32_t pending(JokyCpsFrame *), failing(JokyCpsFrame *);
            int main(void) {
                COMPUTE_BODY
                JokyCpsFrame *p = joky_cps_frame_new(PENDING_SLOTS);
                JokyCpsFrame *f = joky_cps_frame_new(FAILING_SLOTS);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || failing(f) != 1 || pending(cancelled) != 1) return 2;
                }
                if (events != 6) return 3;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 1 || events != 8) return 4;
                if (pending(p) != 0 || joky_cps_result(p) != 13) return 7;
                if (failing(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 5;
                if (joky_cps_state(f) != JOKY_CPS_FAILED || events != 8) return 6;
                joky_cps_frame_free(p);
                joky_cps_frame_free(f);
                return 0;
            }
        "#
        .replace(
            "COMPUTE_ARGS",
            if suspends { "JokyCpsFrame *" } else { "void" },
        )
        .replace("COMPUTE_BODY", &compute)
        .replace("PENDING_SLOTS", &pending_slots.to_string())
        .replace("FAILING_SLOTS", &failing_slots.to_string());
        std::fs::write(&host, harness).unwrap();
        link(
            link_runtime(
                Command::new("cc")
                    .arg(&host)
                    .arg(&object)
                    .arg("-I")
                    .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
            ),
            &program,
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn record_patterns_destructure_in_standalone_native_code() {
    let ast = crate::parser::parse(include_str!(
        "../../../../tests/fixtures/record_patterns.jk"
    ))
    .unwrap();
    let fixture = ScratchDir::new("record-patterns");
    let object = fixture.path().join("patterns.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("patterns");
    let mut compiler = AotCompiler::new("record-patterns").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        static int32_t events[64];
        static int count;
        void record_drop(int32_t value) { if (count < 64) events[count++] = value; }
        int32_t record(int32_t value) { (void)value; return value; }
        extern int32_t copy_full(void), copy_rename_and_rest(void), copy_reordered_and_var(void);
        extern int32_t copy_annotated(void), empty_record(void), nested_copy(void);
        extern int32_t nested_owned_rename(void), single_evaluation(void), owned_transfer(void);
        extern int32_t owned_mixed_and_ignored(void), borrowed_snapshots_and_views(void);
        extern int32_t borrowed_nested_projection(void), borrowed_original_survives(void);
        extern int32_t match_owned_payloads(void), match_borrowed_entry(void);
        extern int32_t generic_patterns(void), alias_pattern(void), tuple_combination(void);
        extern int32_t loops_with_destructuring(void);
        extern int32_t drop_copy_only_runs_immediately(void);
        extern int32_t ignored_fields_release_with_the_record(void);
        extern int32_t bound_fields_release_at_scope_exit(void);
        extern int32_t dropped_field_transfers_whole(void);
        extern int32_t failing_construction(void), failing_rhs_keeps_bindings_out(void);
        struct Case { int32_t (*call)(void); int failure; int32_t expected; int count; int32_t events[4]; };
        int main(void) {
            struct Case cases[] = {
                {copy_full, 0, 34, 0, {0}}, {copy_rename_and_rest, 0, 34, 0, {0}},
                {copy_reordered_and_var, 0, 44, 0, {0}}, {copy_annotated, 0, 34, 0, {0}},
                {empty_record, 0, 42, 0, {0}}, {nested_copy, 0, 34, 0, {0}},
                {nested_owned_rename, 0, 303, 0, {0}}, {single_evaluation, 0, 3, 0, {0}},
                {owned_transfer, 0, 342, 0, {0}}, {owned_mixed_and_ignored, 0, 7, 0, {0}},
                {borrowed_snapshots_and_views, 0, 3430, 0, {0}},
                {borrowed_nested_projection, 0, 217, 0, {0}},
                {borrowed_original_survives, 0, 35, 0, {0}},
                {match_owned_payloads, 0, 91, 0, {0}}, {match_borrowed_entry, 0, 64, 0, {0}},
                {generic_patterns, 0, 38, 0, {0}}, {alias_pattern, 0, 34, 0, {0}},
                {tuple_combination, 0, 37, 0, {0}}, {loops_with_destructuring, 0, 18, 0, {0}},
                {drop_copy_only_runs_immediately, 0, 7, 2, {7, -1}},
                {ignored_fields_release_with_the_record, 0, 5, 3, {1, 2, -1}},
                {bound_fields_release_at_scope_exit, 0, 42, 4, {-1, 3, 2, 1}},
                {dropped_field_transfers_whole, 0, 1, 3, {2, -1, 1}},
                {failing_construction, 1, 0, 2, {9, 1}},
                {failing_rhs_keeps_bindings_out, 1, 0, 1, {8}}
            };
            for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); i++) {
                count = 0;
                int32_t result = cases[i].call();
                if (cases[i].failure != !!joky_runtime_error_code()) return (int)i + 1;
                joky_runtime_error_clear();
                if (!cases[i].failure && result != cases[i].expected) return (int)i + 100;
                if (count != cases[i].count) return (int)i + 200;
                for (int j = 0; j < cases[i].count; j++) {
                    if (events[j] != cases[i].events[j]) return (int)i + 300 + j;
                }
            }
            return 0;
        }
    "#).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let status = Command::new(&executable).status().unwrap();
    assert!(status.success(), "native record patterns: {status}");
}
