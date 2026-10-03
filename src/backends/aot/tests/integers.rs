//! Integers AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn aot_division_reports_failures_through_the_shared_c_runtime() {
    let mut source = include_str!("../../../../tests/fixtures/division.jk").to_owned();
    source.push_str(
        r#"
        extern fn record(x: Int32) -> Int32
        fn effects(n: Int32) -> Int32 { record(1); divide(7, n); record(99) }
        fn cps_divide(n: Int32) @suspends -> Int32 { return divide(7, n) }
    "#,
    );
    let fixture = ScratchDir::new("division");
    let dir = fixture.path();
    let object = dir.join("division.o");
    let host = dir.join("host.c");
    let program = dir.join("division");
    let mut compiler = AotCompiler::new("division").unwrap();
    compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    assert!(compiler.requires_runtime());
    let slots = compiler.cps_frame_slots("cps_divide").unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let start = source.find("a / b").unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_cps.h"
        #include "text.h"
        #include <stdbool.h>
        #include <limits.h>
        static int effects_seen;
        int32_t record(int32_t x) { effects_seen += x; return x; }
        extern int32_t divide(int32_t, int32_t), divide_constant(void), divide_nested(int32_t, int32_t);
        extern int32_t divide_recurse(int32_t), effects(int32_t), divide_skipped(int32_t);
        extern void divide_unit(int32_t);
        extern bool divide_bool(int32_t);

        extern int32_t cps_divide(JokyCpsFrame *);
        int main(void) {
            const int32_t values[] = { INT32_MIN, -7, -1, 0, 1, 7, INT32_MAX };
            for (int i = 0; i < 7; i++) for (int j = 0; j < 7; j++) {
                joky_runtime_error_clear();
                int32_t value = divide(values[i], values[j]);
                if (values[j] == 0) {
                    if (!joky_runtime_error_code() || joky_runtime_error_start() != START || joky_runtime_error_end() != END) return 1;
                } else {
                    int32_t expected = values[i] == INT32_MIN && values[j] == -1 ? INT32_MIN : values[i] / values[j];
                    if (joky_runtime_error_code() || value != expected) return 2;
                }
            }
            joky_runtime_error_clear();
            if (divide_constant() != INT32_MIN || joky_runtime_error_code()) return 3;
            (void)divide_nested(7, 0);
            if (!joky_runtime_error_code() || joky_runtime_error_start() != START) return 4;
            joky_runtime_error_clear(); (void)divide_bool(0);
            if (!joky_runtime_error_code()) return 5;
            joky_runtime_error_clear(); JokyOwnedTextV2 output = {0};
            if (!__joky_user_text_v2_divide_text(0, &output) || output.data || output.byte_length) return 10;
            if (!joky_runtime_error_code()) return 6;
            joky_runtime_error_clear(); divide_unit(0);
            if (!joky_runtime_error_code()) return 7;
            joky_runtime_error_clear(); (void)divide_recurse(5);
            if (!joky_runtime_error_code()) return 8;
            joky_runtime_error_clear(); (void)effects(0);
            if (!joky_runtime_error_code() || effects_seen != 1) return 9;
            joky_runtime_error_clear();
            if (divide_skipped(0) != 42 || joky_runtime_error_code()) return 10;
            JokyCpsFrame *frame = joky_cps_frame_new(SLOTS);
            if (cps_divide(frame) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 11;
            if (joky_cps_state(frame) != JOKY_CPS_FAILED) return 12;
            joky_runtime_error_clear();
            if (cps_divide(frame) != JOKY_CPS_INVALID_STATE || joky_runtime_error_code()) return 13;
            joky_cps_frame_free(frame);
            frame = joky_cps_frame_new(SLOTS);
            __joky_cps_slots(frame)[0] = 2;
            if (cps_divide(frame) != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 3 || joky_runtime_error_code()) return 14;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#.replace("START", &start.to_string()).replace("END", &(start + 5).to_string()).replace("SLOTS", &slots.to_string());
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let status = Command::new(program).status().unwrap();
    assert!(
        status.success(),
        "native division test exited with {status}"
    );
}

#[test]
#[cfg(unix)]
fn int64_native_objects_preserve_direct_cps_and_host_full_width_values() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../../tests/fixtures/int64.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(
            r#"
            extern fn delayed64(value: Int64) @suspends -> Int64
            extern fn record64(value: Int64) -> Int64
            fn direct64(value: Int64) -> Int64 { value + 1 }
            fn pending64(value: Int64) @suspends -> Int64 { direct64(delayed64(record64(value))) }
            fn failed64() @suspends -> Int64 { let n = delayed64(-1) 3000000000 / n }
        "#,
        );
        let fixture = ScratchDir::new("int64");
        let dir = fixture.path();
        let object = dir.join("int64.o");
        let program = dir.join("int64");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("int64").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let pending_slots = compiler.cps_frame_slots("pending64").unwrap();
        let failed_slots = compiler.cps_frame_slots("failed64").unwrap();
        let check = if suspends {
            let check_slots = compiler.cps_frame_slots("check").unwrap();
            let min_slots = compiler.cps_frame_slots("min").unwrap();
            format!("JokyCpsFrame *c = joky_cps_frame_new({check_slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c); c = joky_cps_frame_new({min_slots}); if (min(c) != 0 || joky_cps_result(c) != (UINT64_C(1) << 63)) return 2; joky_cps_frame_free(c);")
        } else {
            "if (check() != 42 || min() != INT64_MIN || divide(INT64_MIN, -1) != INT64_MIN || add(INT64_MAX, 1) != INT64_MIN || negate(INT64_MIN) != INT64_MIN) return 1;".to_owned()
        };
        std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
        compiler.write_object(&object).unwrap();
        let declarations = if suspends {
            "extern int32_t check(JokyCpsFrame *), min(JokyCpsFrame *);"
        } else {
            "extern int32_t check(void); extern int64_t min(void), divide(int64_t, int64_t), add(int64_t, int64_t), negate(int64_t);"
        };
        let harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            int64_t record64(int64_t value) { events++; return value; }
            int32_t delayed64(JokyCpsFrame *frame) {
                if (!joky_cps_state(frame)) { events++; joky_cps_set_state(frame, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(frame, __joky_cps_slots(frame)[0] + UINT64_C(1)); return 0;
            }
            DECLARATIONS
            extern int32_t pending64(JokyCpsFrame *), failed64(JokyCpsFrame *);
            int main(void) {
                CHECK
                JokyCpsFrame *p = joky_cps_frame_new(PENDING_SLOTS);
                JokyCpsFrame *f = joky_cps_frame_new(FAILED_SLOTS);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
                __joky_cps_slots(p)[0] = (uint64_t)-INT64_C(4000000000);
                __joky_cps_slots(cancelled)[0] = INT64_C(3000000000);
                for (int i = 0; i < 3; i++) { if (pending64(p) != 1 || failed64(f) != 1 || pending64(cancelled) != 1) return 3; }
                if (events != 5) return 4;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending64(p) != 0 || joky_cps_result(p) != (uint64_t)-INT64_C(3999999998)) return 5;
                if (failed64(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED || events != 5) return 6;
                joky_cps_frame_free(p); joky_cps_frame_free(f);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check).replace("PENDING_SLOTS", &pending_slots.to_string()).replace("FAILED_SLOTS", &failed_slots.to_string());
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
fn unsigned_native_objects_preserve_arithmetic_abi_and_pending_values() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../../tests/fixtures/unsigned.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn host32(value: UInt32) -> UInt32
            extern fn host64(value: UInt64) -> UInt64
            extern fn delay32(unit: Unit, value: UInt32, end: Unit) @suspends -> UInt32
            extern fn delay64(unit: Unit, value: UInt64, end: Unit) @suspends -> UInt64
            fn native32(n: UInt32) -> UInt32 { host32(n) / 2 }
            fn native64(n: UInt64) -> UInt64 { host64(n) / 2 }
            fn pending32(n: UInt32) @suspends -> UInt32 { host32(delay32({}, host32(n), {})) }
            fn pending64(n: UInt64) @suspends -> UInt64 { host64(delay64({}, host64(n), {})) }
            fn fail32() @suspends -> UInt32 { let d = delay32({}, 4294967295, {}) 4294967295 / d }
            fn fail64() @suspends -> UInt64 { let d = delay64({}, 18446744073709551615, {}) 18446744073709551615 / d }
        "#);
        let fixture = ScratchDir::new("unsigned");
        let dir = fixture.path();
        let object = dir.join("unsigned.o");
        let program = dir.join("unsigned");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("unsigned").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let check = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!("JokyCpsFrame *check_frame = joky_cps_frame_new({slots}); if (check(check_frame) != 0 || joky_cps_result(check_frame) != 42) return 1; joky_cps_frame_free(check_frame);")
        } else {
            "if (check() != 42 || max32() != UINT32_MAX || max64() != UINT64_MAX || div32(UINT32_MAX, UINT32_C(2147483648)) != 1 || div64(UINT64_MAX, UINT64_C(9223372036854775808)) != 1) return 1;".to_owned()
        };
        let declarations = if suspends {
            "extern int32_t check(JokyCpsFrame *);"
        } else {
            "extern int32_t check(void); extern uint32_t max32(void), div32(uint32_t, uint32_t); extern uint64_t max64(void), div64(uint64_t, uint64_t);"
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint32_t host32(uint32_t n) { events++; return n; }
            uint64_t host64(uint64_t n) { events++; return n; }
            static int32_t delayed(JokyCpsFrame *f, int narrow) {
                uint64_t *args = __joky_cps_slots(f);
                if (args[0] != 0 || args[2] != 0) return -1;
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, narrow ? (uint32_t)(args[1] + UINT64_C(1)) : args[1] + UINT64_C(1));
                return 0;
            }
            int32_t delay32(JokyCpsFrame *f) { return delayed(f, 1); }
            int32_t delay64(JokyCpsFrame *f) { return delayed(f, 0); }
            DECLARATIONS
            extern uint32_t native32(uint32_t);
            extern uint64_t native64(uint64_t);
            extern int32_t pending32(JokyCpsFrame *), pending64(JokyCpsFrame *), fail32(JokyCpsFrame *), fail64(JokyCpsFrame *);
            int main(void) {
                CHECK
                if (native32(UINT32_MAX) != UINT32_C(2147483647) || native64(UINT64_MAX) != UINT64_C(9223372036854775807)) return 2;
                events = 0;
                JokyCpsFrame *p32 = joky_cps_frame_new(PENDING32);
                JokyCpsFrame *p64 = joky_cps_frame_new(PENDING64);
                JokyCpsFrame *f32 = joky_cps_frame_new(FAILED32);
                JokyCpsFrame *f64 = joky_cps_frame_new(FAILED64);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING64);
                __joky_cps_slots(p32)[0] = UINT32_C(2147483648);
                __joky_cps_slots(p64)[0] = UINT64_C(9223372036854775808);
                __joky_cps_slots(cancelled)[0] = UINT64_MAX;
                for (int i = 0; i < 3; i++) {
                    if (pending32(p32) != 1 || pending64(p64) != 1 || fail32(f32) != 1 || fail64(f64) != 1 || pending64(cancelled) != 1) return 3;
                }
                if (events != 8) return 4;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending32(p32) != 0 || joky_cps_result(p32) != UINT32_C(2147483649)) return 5;
                if (pending64(p64) != 0 || joky_cps_result(p64) != UINT64_C(9223372036854775809)) return 6;
                if (events != 10) return 7;
                if (fail32(f32) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f32) != JOKY_CPS_FAILED) return 8;
                joky_runtime_error_clear();
                if (fail64(f64) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f64) != JOKY_CPS_FAILED) return 9;
                joky_cps_frame_free(p32); joky_cps_frame_free(p64); joky_cps_frame_free(f32); joky_cps_frame_free(f64);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING32", "pending32"),
            ("PENDING64", "pending64"),
            ("FAILED32", "fail32"),
            ("FAILED64", "fail64"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
        compiler.write_object(&object).unwrap();
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
fn uint8_native_objects_preserve_arithmetic_abi_and_pending_values() {
    for suspends in [false, true] {
        let mut source = with_suspends("fn check() -> Int32 { 42 }", suspends, Suspends::Typed);
        source.push_str(r#"
            extern fn host32(value: UInt8) -> UInt8
            extern fn host64(value: UInt64) -> UInt64
            extern fn delay32(unit: Unit, value: UInt8, end: Unit) @suspends -> UInt8
            extern fn delay64(unit: Unit, value: UInt64, end: Unit) @suspends -> UInt64
            fn native32(n: UInt8) -> UInt8 { host32(n) / 2 }
            fn native64(n: UInt64) -> UInt64 { host64(n) / 2 }
            fn pending32(n: UInt8) @suspends -> UInt8 { host32(delay32({}, host32(n), {})) }
            fn pending64(n: UInt64) @suspends -> UInt64 { host64(delay64({}, host64(n), {})) }
            fn fail32() @suspends -> UInt8 { let d = delay32({}, 255, {}) 255 / d }
            fn fail64() @suspends -> UInt64 { let d = delay64({}, 18446744073709551615, {}) 18446744073709551615 / d }
        "#);
        let fixture = ScratchDir::new("unsigned");
        let dir = fixture.path();
        let object = dir.join("unsigned.o");
        let program = dir.join("unsigned");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("unsigned").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let check = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!("JokyCpsFrame *check_frame = joky_cps_frame_new({slots}); if (check(check_frame) != 0 || joky_cps_result(check_frame) != 42) return 1; joky_cps_frame_free(check_frame);")
        } else {
            "if (check() != 42 || native32(UINT8_MAX) != UINT8_C(127) || native64(UINT64_MAX) != UINT64_C(9223372036854775807)) return 1;".to_owned()
        };
        let declarations = if suspends {
            "extern int32_t check(JokyCpsFrame *);"
        } else {
            "extern int32_t check(void); extern uint8_t max32(void), div32(uint8_t, uint8_t); extern uint64_t max64(void), div64(uint64_t, uint64_t);"
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint8_t host32(uint8_t n) { events++; return n; }
            uint64_t host64(uint64_t n) { events++; return n; }
            static int32_t delayed(JokyCpsFrame *f, int narrow) {
                uint64_t *args = __joky_cps_slots(f);
                if (args[0] != 0 || args[2] != 0) return -1;
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, narrow ? (uint8_t)(args[1] + UINT64_C(1)) : args[1] + UINT64_C(1));
                return 0;
            }
            int32_t delay32(JokyCpsFrame *f) { return delayed(f, 1); }
            int32_t delay64(JokyCpsFrame *f) { return delayed(f, 0); }
            DECLARATIONS
            extern uint8_t native32(uint8_t);
            extern uint64_t native64(uint64_t);
            extern int32_t pending32(JokyCpsFrame *), pending64(JokyCpsFrame *), fail32(JokyCpsFrame *), fail64(JokyCpsFrame *);
            int main(void) {
                CHECK
                if (native32(UINT8_MAX) != UINT8_C(127) || native64(UINT64_MAX) != UINT64_C(9223372036854775807)) return 2;
                events = 0;
                JokyCpsFrame *p32 = joky_cps_frame_new(PENDING32);
                JokyCpsFrame *p64 = joky_cps_frame_new(PENDING64);
                JokyCpsFrame *f32 = joky_cps_frame_new(FAILED32);
                JokyCpsFrame *f64 = joky_cps_frame_new(FAILED64);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING64);
                __joky_cps_slots(p32)[0] = UINT8_C(128);
                __joky_cps_slots(p64)[0] = UINT64_C(9223372036854775808);
                __joky_cps_slots(cancelled)[0] = UINT64_MAX;
                for (int i = 0; i < 3; i++) {
                    if (pending32(p32) != 1 || pending64(p64) != 1 || fail32(f32) != 1 || fail64(f64) != 1 || pending64(cancelled) != 1) return 3;
                }
                if (events != 8) return 4;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending32(p32) != 0 || joky_cps_result(p32) != UINT8_C(129)) return 5;
                if (pending64(p64) != 0 || joky_cps_result(p64) != UINT64_C(9223372036854775809)) return 6;
                if (events != 10) return 7;
                if (fail32(f32) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f32) != JOKY_CPS_FAILED) return 8;
                joky_runtime_error_clear();
                if (fail64(f64) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f64) != JOKY_CPS_FAILED) return 9;
                joky_cps_frame_free(p32); joky_cps_frame_free(p64); joky_cps_frame_free(f32); joky_cps_frame_free(f64);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING32", "pending32"),
            ("PENDING64", "pending64"),
            ("FAILED32", "fail32"),
            ("FAILED64", "fail64"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
        compiler.write_object(&object).unwrap();
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
fn bitwise_native_objects_check_shifts_and_preserve_pending_operands() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../../tests/fixtures/bitwise.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn record(n: UInt64) -> UInt64
            extern fn delay(n: UInt64) @suspends -> UInt64
            fn shl32(a: Int32, n: Int32) -> Int32 { a << n }
            fn shr32(a: Int32, n: Int32) -> Int32 { a >> n }
            fn shl64(a: Int64, n: Int64) -> Int64 { a << n }
            fn shr64(a: Int64, n: Int64) -> Int64 { a >> n }
            fn shlu32(a: UInt32, n: UInt32) -> UInt32 { a << n }
            fn shru32(a: UInt32, n: UInt32) -> UInt32 { a >> n }
            fn shlu64(a: UInt64, n: UInt64) -> UInt64 { a << n }
            fn shru64(a: UInt64, n: UInt64) -> UInt64 { a >> n }
            fn pending(n: UInt64) @suspends -> UInt64 {
                var value = n
                let bits = value & delay({ value = 0 record(n) })
                ~bits ^ (value | 3 << 1)
            }
            fn failure(n: UInt64) @suspends -> UInt64 { shlu64(1, delay(n)) | record(99) }
            fn bad_branch(n: UInt64) @suspends { shru64(1, delay(n)); }
            fn other(n: UInt64) @suspends { record(delay(n)); }
            fn bad_scope() @suspends -> UInt64 { scope { | bad_branch(64 | 0) | other(1 | 2) } record(99) }
        "#);
        let fixture = ScratchDir::new("bitwise");
        let dir = fixture.path();
        let object = dir.join("bitwise.o");
        let program = dir.join("bitwise");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("bitwise").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        assert!(compiler.requires_runtime());
        let check = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!("JokyCpsFrame *c = joky_cps_frame_new({slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c);")
        } else {
            "if (check() != 42) return 1;".to_owned()
        };
        let declarations = if suspends {
            "extern int32_t check(JokyCpsFrame *);"
        } else {
            "extern int32_t check(void);"
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint64_t record(uint64_t n) { events++; return n; }
            int32_t delay(JokyCpsFrame *f) {
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, __joky_cps_slots(f)[0]); return 0;
            }
            DECLARATIONS
            extern int32_t shl32(int32_t, int32_t), shr32(int32_t, int32_t);
            extern int64_t shl64(int64_t, int64_t), shr64(int64_t, int64_t);
            extern uint32_t shlu32(uint32_t, uint32_t), shru32(uint32_t, uint32_t);
            extern uint64_t shlu64(uint64_t, uint64_t), shru64(uint64_t, uint64_t);
            extern int32_t pending(JokyCpsFrame *), failure(JokyCpsFrame *), bad_scope(JokyCpsFrame *);
            #define INVALID(call) do { joky_runtime_error_clear(); (void)(call); if (!joky_runtime_error_code() || joky_runtime_error_end() <= joky_runtime_error_start()) return 3; } while (0)
            int main(void) {
                CHECK
                if (shl32(1, 31) != INT32_MIN || shr32(INT32_MIN, 31) != -1 || shl64(1, 63) != INT64_MIN || shr64(INT64_MIN, 63) != -1) return 2;
                if (shlu32(1, 31) != UINT32_C(2147483648) || shru32(UINT32_MAX, 31) != 1 || shlu64(1, 63) != (UINT64_C(1) << 63) || shru64(UINT64_MAX, 63) != 1) return 2;
                INVALID(shl32(1, -1)); INVALID(shr32(1, 32)); INVALID(shl64(1, 64)); INVALID(shr64(1, INT64_MIN));
                INVALID(shlu32(1, UINT32_MAX)); INVALID(shru32(1, 32)); INVALID(shlu64(1, UINT64_C(1) << 32)); INVALID(shru64(1, UINT64_MAX));
                joky_runtime_error_clear();
                JokyCpsFrame *p = joky_cps_frame_new(PENDING);
                JokyCpsFrame *f = joky_cps_frame_new(FAILURE);
                JokyCpsFrame *s = joky_cps_frame_new(SCOPE);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING);
                uint64_t high = UINT64_C(0x800000000000005a);
                __joky_cps_slots(p)[0] = high;
                __joky_cps_slots(f)[0] = UINT64_C(1) << 32;
                __joky_cps_slots(cancelled)[0] = 7;
                for (int i = 0; i < 3; i++) { if (pending(p) != 1 || failure(f) != 1 || bad_scope(s) != 1 || pending(cancelled) != 1) return 4; }
                if (events != 7) return 5;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 0 || joky_cps_result(p) != (~high ^ UINT64_C(6))) return 6;
                if (failure(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED) return 7;
                joky_runtime_error_clear();
                if (bad_scope(s) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(s) != JOKY_CPS_FAILED || events != 7) return 8;
                joky_cps_frame_free(p); joky_cps_frame_free(f); joky_cps_frame_free(s);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING", "pending"),
            ("FAILURE", "failure"),
            ("SCOPE", "bad_scope"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        link(
            link_runtime(
                Command::new("cc")
                    .arg(&host)
                    .arg(&object)
                    .args(["-Wall", "-Wextra", "-Werror", "-I"])
                    .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
            ),
            &program,
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
fn integer_conversion_only_objects_need_no_runtime() {
    let source = include_str!("../../../../tests/fixtures/conversions.jk");
    let mut compiler = AotCompiler::new("pure_conversions").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert!(!compiler.requires_runtime());
}

#[test]
#[cfg(unix)]
fn integer_conversion_native_objects_preserve_mixed_signatures_and_pending_values() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../../tests/fixtures/conversions.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn record(n: UInt64) -> UInt64
            extern fn delay(n: UInt64) @suspends -> UInt64
            fn widen_signed(n: Int32) -> UInt64 { uint64(n) }
            fn widen_unsigned(n: UInt32) -> Int64 { int64(n) }
            fn narrow(n: UInt64) -> Int32 { int32(n) }
            fn reinterpret(n: UInt64) -> Int64 { int64(n) }
            fn pending(n: UInt64) @suspends -> UInt64 {
                var value = n
                let pair = (int32(value), uint32(delay({ value = 0 record(n) })))
                let (signed, unsigned) = pair
                uint64(int64(signed) + int64(unsigned)) ^ uint64(int32(delay(3))) ^ value
            }
            fn pending_signed(n: Int32) @suspends -> Int64 { int64(int32(delay(uint64(n)))) }
            fn pending_narrow(n: UInt64) @suspends -> Int32 { int32(delay(n)) }
            fn bad(n: UInt64) -> UInt64 { 1 << n }
            fn failure(n: UInt64) @suspends -> UInt64 { record(uint64(int32(bad(delay(n))))) }
            fn branch(n: UInt64) @suspends { record(uint64(uint32(delay(n)))); }
            fn scoped() @suspends -> UInt64 { scope { | branch(uint64(-1)) | branch(uint64(42)) } 42 }
        "#);
        let fixture = ScratchDir::new("conversions");
        let dir = fixture.path();
        let object = dir.join("conversions.o");
        let program = dir.join("conversions");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("conversions").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let (declaration, check) = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            ("extern int32_t check(JokyCpsFrame *);", format!("JokyCpsFrame *c = joky_cps_frame_new({slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c);"))
        } else {
            (
                "extern int32_t check(void);",
                "if (check() != 42) return 1;".to_owned(),
            )
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint64_t record(uint64_t n) { events++; return n; }
            int32_t delay(JokyCpsFrame *f) {
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, __joky_cps_slots(f)[0]); return 0;
            }
            DECLARATION
            extern uint64_t widen_signed(int32_t);
            extern int64_t widen_unsigned(uint32_t), reinterpret(uint64_t);
            extern int32_t narrow(uint64_t);
            extern int32_t pending(JokyCpsFrame *), pending_signed(JokyCpsFrame *), pending_narrow(JokyCpsFrame *);
            extern int32_t failure(JokyCpsFrame *), scoped(JokyCpsFrame *);
            int main(void) {
                CHECK
                if (widen_signed(-1) != UINT64_MAX || widen_signed(INT32_MIN) != UINT64_MAX - INT32_MAX) return 2;
                if (widen_unsigned(UINT32_MAX) != INT64_C(4294967295) || narrow(UINT64_MAX) != -1 || narrow(UINT64_C(0x10000002a)) != 42) return 3;
                if (reinterpret(UINT64_MAX) != -1 || reinterpret(UINT64_C(1) << 63) != INT64_MIN) return 4;
                JokyCpsFrame *p = joky_cps_frame_new(PENDING);
                JokyCpsFrame *s = joky_cps_frame_new(SIGNED);
                JokyCpsFrame *n = joky_cps_frame_new(NARROW);
                JokyCpsFrame *f = joky_cps_frame_new(FAILURE);
                JokyCpsFrame *scope = joky_cps_frame_new(SCOPE);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING);
                __joky_cps_slots(p)[0] = UINT64_C(0x80000000ffffffff);
                __joky_cps_slots(s)[0] = UINT32_MAX;
                __joky_cps_slots(n)[0] = UINT64_MAX;
                __joky_cps_slots(f)[0] = 64;
                __joky_cps_slots(cancelled)[0] = 7;
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || pending_signed(s) != 1 || pending_narrow(n) != 1 || failure(f) != 1 || scoped(scope) != 1 || pending(cancelled) != 1) return 5;
                }
                if (events != 9) return 6;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 1 || pending(p) != 0 || joky_cps_result(p) != (UINT64_C(4294967294) ^ 3)) return 7;
                if (pending_signed(s) != 0 || joky_cps_result(s) != UINT64_MAX) return 8;
                if (pending_narrow(n) != 0 || (uint32_t)joky_cps_result(n) != UINT32_MAX) return 9;
                if (failure(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED) return 10;
                joky_runtime_error_clear();
                if (scoped(scope) != 0 || joky_cps_result(scope) != 42 || events != 12) return 11;
                joky_cps_frame_free(p); joky_cps_frame_free(s); joky_cps_frame_free(n);
                joky_cps_frame_free(f); joky_cps_frame_free(scope);
                return 0;
            }
        "#.replace("DECLARATION", declaration).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING", "pending"),
            ("SIGNED", "pending_signed"),
            ("NARROW", "pending_narrow"),
            ("FAILURE", "failure"),
            ("SCOPE", "scoped"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        link(
            link_runtime(
                Command::new("cc")
                    .arg(&host)
                    .arg(&object)
                    .args(["-Wall", "-Wextra", "-Werror", "-I"])
                    .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
            ),
            &program,
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}
