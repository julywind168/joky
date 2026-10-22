//! Float AOT fixtures linked against real C code.
use super::*;

#[cfg(unix)]
#[test]
fn aot_float_environment_isolated_at_native_and_cps_boundaries() {
    use crate::runtime::float_env::{testing, Guard};
    let fixture = ScratchDir::new("float-environment");
    let object = fixture.path().join("float.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("float");
    let mut compiler = AotCompiler::new("float-environment").unwrap();
    let ast = crate::parser::parse(include_str!(
        "../../../../tests/fixtures/float_environment.jk"
    ))
    .unwrap();
    {
        let _restore = Guard::enter();
        testing::hostile();
        let saved = testing::state();
        compiler
            .compile_module(&ast)
            .unwrap_or_else(|e| panic!("{}", e.message()));
        assert_eq!(testing::state(), saved);
    }
    let slots = compiler.cps_frame_slots("flowing").unwrap();
    assert!(compiler.requires_runtime());
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include <stdint.h>
        #include <stdio.h>
        #include <string.h>
        #include "joky_cps.h"
        #include "joky_runtime.h"
        #if defined(__aarch64__)
        static uint64_t env(void) {
            uint64_t c, s;
            __asm__ volatile("mrs %0, fpcr" : "=r"(c));
            __asm__ volatile("mrs %0, fpsr" : "=r"(s));
            return c | (s << 32);
        }
        static void set_env(uint64_t v) {
            uint64_t c = v & UINT64_C(0xffffffff), s = v >> 32;
            __asm__ volatile("msr fpcr, %0" :: "r"(c));
            __asm__ volatile("msr fpsr, %0" :: "r"(s));
        }
        #define HOSTILE ((UINT64_C(1) << 24) | (UINT64_C(1) << 22) | (UINT64_C(16) << 32))
        #elif defined(__x86_64__)
        static uint64_t env(void) { uint32_t v; __asm__ volatile("stmxcsr %0" : "=m"(v)); return v; }
        static void set_env(uint64_t v) { uint32_t c = (uint32_t)v; __asm__ volatile("ldmxcsr %0" :: "m"(c)); }
        #define HOSTILE UINT64_C(0xdfE0)
        #else
        #error Unsupported floating-point environment test target
        #endif
        static uint64_t bits(double v) { uint64_t b; memcpy(&b, &v, 8); return b; }
        static uint32_t bits32(float v) { uint32_t b; memcpy(&b, &v, 4); return b; }
        static double value(uint64_t b) { double v; memcpy(&v, &b, 8); return v; }
        static float value32(uint32_t b) { float v; memcpy(&v, &b, 4); return v; }
        extern double folded64(void), folded_tiny(void), add64(double, double), half(double), indirect(double, double), fail(double);
        extern float folded32(void), add32(float, float), half32(float);
        extern double closure_borrowed64(double, double);
        extern float closure_move32(float, float);
        extern int32_t integer_result(void), flowing(JokyCpsFrame *);
        static uint64_t observed;
        static int drops, stops, reentry_failed;
        void dirty(void) {
            set_env(HOSTILE);
            if (bits(folded64()) != UINT64_C(0x3ff0000000000000) || env() != HOSTILE) reentry_failed = 1;
        }
        void observe(double v) { observed = bits(v); ++drops; }
        static int32_t pause_poll(JokyCpsFrame *f) {
            dirty();
            if (!joky_cps_state(f)) { joky_cps_set_state(f, 1); return JOKY_CPS_PENDING; }
            joky_cps_set_state(f, JOKY_CPS_DONE); return JOKY_CPS_COMPLETED;
        }
        static void pause_stop(JokyCpsFrame *f, uint32_t reason) { (void)f; (void)reason; dirty(); ++stops; }
        int32_t pause(JokyCpsFrame *f) {
            static const JokyBorrowingHostV1 operation = {1, pause_poll, pause_stop};
            return joky_cps_poll_borrowing_v1(f, &operation);
        }
        #define CHECK(x) do { if (!(x)) { set_env(original); fprintf(stderr, "float environment check failed at %d\n", __LINE__); return 1; } } while(0)
        int main(void) {
            uint64_t original = env();
            set_env(HOSTILE);
            CHECK(bits(folded64()) == UINT64_C(0x3ff0000000000000));
            CHECK(bits32(folded32()) == UINT32_C(0x3f800000));
            CHECK(bits(folded_tiny()) == UINT64_C(0x8000000000000));
            CHECK(env() == HOSTILE);
            CHECK(bits(add64(1.0, value(UINT64_C(0x3ca0000000000000)))) == UINT64_C(0x3ff0000000000000));
            CHECK(env() == HOSTILE);
            CHECK(bits(indirect(1.0, value(UINT64_C(0x3ca0000000000000)))) == UINT64_C(0x3ff0000000000000));
            CHECK(bits(closure_borrowed64(1.0, value(UINT64_C(0x3ca0000000000000)))) == UINT64_C(0x3ff0000000000000));
            CHECK(env() == HOSTILE);
            CHECK(bits32(closure_move32(1.0f, value32(UINT32_C(0x33800000)))) == UINT32_C(0x3f800000));
            CHECK(env() == HOSTILE);
            CHECK(bits32(add32(1.0f, value32(UINT32_C(0x33800000)))) == UINT32_C(0x3f800000));
            CHECK(bits(half(value(UINT64_C(0x10000000000000)))) == UINT64_C(0x8000000000000));
            CHECK(bits(half(value(2))) == 1);
            CHECK(bits32(half32(value32(UINT32_C(0x800000)))) == UINT32_C(0x400000));
            CHECK(integer_result() == 1 && env() == HOSTILE);
            (void)fail(1.0);
            CHECK(joky_runtime_error_code() != 0 && env() == HOSTILE);
            joky_runtime_error_clear();
            for (int cancel = 0; cancel < 2; ++cancel) {
                drops = 0; stops = 0;
                JokyCpsFrame *f = joky_cps_frame_new(SLOTS);
                __joky_cps_slots(f)[0] = UINT64_C(0x10000000000000);
                __joky_cps_slots(f)[1] = 0;
                CHECK(flowing(f) == JOKY_CPS_PENDING && env() == HOSTILE);
                if (!cancel) {
                    CHECK(flowing(f) == JOKY_CPS_COMPLETED && env() == HOSTILE);
                    CHECK(joky_cps_result(f) == UINT64_C(0x10000000000000));
                }
                joky_cps_frame_free(f);
                CHECK(stops == 1 && drops == !cancel);
                if (!cancel) CHECK(observed == UINT64_C(0x8000000000000));
                CHECK(env() == HOSTILE);
            }
            CHECK(!reentry_failed && !joky_runtime_error_code());
            set_env(original);
            return 0;
        }
    "#;
    std::fs::write(&host, harness.replace("SLOTS", &slots.to_string())).unwrap();
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

#[cfg(unix)]
#[test]
fn aot_float_externs_and_repr_c_match_real_c_code() {
    let source = r#"
        extern fn c_add(a: Float64, b: Float64) -> Float64
        extern fn c_scale(v: Float32) -> Float32
        extern fn c_mix(count: Int32, factor: Float64) -> Float64
        extern fn c_dot(x: Float64, y: Float64, z: Float64) -> Float64
        #repr(C) struct Vec2 { x: Float64, y: Float32 }
        unsafe extern "C" fn c_offset(v: &Vec2) -> Float64

        fn add(a: Float64, b: Float64) -> Float64 { c_add(a, b) }
        fn scale(v: Float32) -> Float32 { c_scale(v) }
        fn mix(count: Int32, factor: Float64) -> Float64 { c_mix(count, factor) }
        fn dot(x: Float64, y: Float64, z: Float64) -> Float64 { c_dot(x, y, z) }
        fn offset() -> Float64 {
            let v = Vec2 { x: 2.5, y: 0.5 }
            unsafe { c_offset(&v) }
        }
        fn nan_scale() -> Float32 { c_scale(0.0 / 0.0) }
    "#;
    let ast = crate::parser::parse(source).unwrap();
    let fixture = ScratchDir::new("float-externs");
    let dir = fixture.path();
    let object = dir.join("floats.o");
    let host = dir.join("host.c");
    let executable = dir.join("floats");
    let mut compiler = AotCompiler::new("float-externs").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include <stdint.h>
        #include <stddef.h>
        #include <math.h>
        #include "joky_runtime.h"
        // The mirrored layout of the #repr(C) Vec2 record above.
        typedef struct { double x; float y; } CVec2;
        double c_add(double a, double b) { return a + b; }
        float c_scale(float v) { return v * 2.0f; }
        double c_mix(int32_t count, double factor) { return factor * (double)count; }
        double c_dot(double x, double y, double z) { return x * x + y * y + z * z; }
        double c_offset(const CVec2 *v) { return v->x + (double)v->y; }
        extern double add(double, double), mix(int32_t, double), dot(double, double, double), offset(void);
        extern float scale(float), nan_scale(void);
        int main(void) {
            joky_runtime_error_clear();
            if (sizeof(CVec2) != 16 || offsetof(CVec2, x) != 0 || offsetof(CVec2, y) != 8) return 1;
            if (add(0.1, 0.2) != 0.1 + 0.2) return 2;
            if (add(1.0e300, 1.0e300) != 1.0e300 + 1.0e300) return 3;
            if (!isinf(add(INFINITY, 1.0)) || !signbit(add(-INFINITY, 1.0))) return 4;
            if (add(0.0, -0.0) != 0.0 || signbit(add(-0.0, -0.0)) == 0) return 5;
            if (scale(0.25f) != 0.5f || scale(-1.5f) != -3.0f) return 6;
            if (!isnan(nan_scale())) return 7;
            if (mix(3, 2.5) != 7.5 || mix(-2, 0.5) != -1.0) return 8;
            if (dot(1.0, 2.0, 3.0) != 14.0) return 9;
            if (offset() != 3.0) return 10;
            if (joky_runtime_error_code()) return 11;
            return 0;
        }
    "#;
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let status = Command::new(executable).status().unwrap();
    assert!(status.success(), "float externs: {status}");
}

/// Suspending float entries cross C polls as raw bit patterns in the shared
/// CPS frame; Float32 stays zero-extended in its slot.
#[cfg(unix)]
#[test]
fn aot_suspending_floats_keep_bits_across_cps_polls() {
    let source = r#"
        import joky/task
        fn square(v: Float64) @suspends -> Float64 { task.yield_now(); v * v }
        fn bump(v: Float32) @suspends -> Float32 { task.yield_now(); v + 0.25 }
    "#;
    let ast = crate::parser::parse(source).unwrap();
    let fixture = ScratchDir::new("float-cps");
    let dir = fixture.path();
    let object = dir.join("float-cps.o");
    let host = dir.join("host.c");
    let executable = dir.join("float-cps");
    let mut compiler = AotCompiler::new("float-cps").unwrap();
    compiler.compile_module(&ast).unwrap();
    let wide_slots = compiler.cps_frame_slots("square").unwrap();
    let narrow_slots = compiler.cps_frame_slots("bump").unwrap();
    compiler.write_object(&object).unwrap();
    let harness = format!(
        r#"
        #include <stdint.h>
        #include <string.h>
        #include "joky_cps.h"
        #include "joky_runtime.h"
        extern int32_t square(JokyCpsFrame *), bump(JokyCpsFrame *);
        static int poll_done(JokyCpsEntry entry, JokyCpsFrame *frame) {{
            int status = JOKY_CPS_PENDING;
            for (int i = 0; i < 8 && status == JOKY_CPS_PENDING; ++i) status = entry(frame);
            return status == JOKY_CPS_COMPLETED && joky_cps_state(frame) == JOKY_CPS_DONE;
        }}
        static double wide_result(JokyCpsFrame *frame) {{
            uint64_t bits = joky_cps_result(frame);
            double out; memcpy(&out, &bits, 8); return out;
        }}
        int main(void) {{
            const double wide[] = {{ 2.0, -0.5, 1.0e150 }};
            for (int i = 0; i < 3; ++i) {{
                joky_runtime_error_clear();
                JokyCpsFrame *frame = joky_cps_frame_new({wide_slots});
                uint64_t bits; memcpy(&bits, &wide[i], 8);
                __joky_cps_slots(frame)[0] = bits;
                if (!poll_done(square, frame)) return 10 + i;
                if (wide_result(frame) != wide[i] * wide[i]) return 20 + i;
                if (joky_runtime_error_code()) return 30 + i;
                joky_cps_frame_free(frame);
            }}
            const float narrow[] = {{ 0.5f, -2.5f }};
            for (int i = 0; i < 2; ++i) {{
                joky_runtime_error_clear();
                JokyCpsFrame *frame = joky_cps_frame_new({narrow_slots});
                uint32_t bits; memcpy(&bits, &narrow[i], 4);
                __joky_cps_slots(frame)[0] = (uint64_t)bits;
                if (!poll_done(bump, frame)) return 40 + i;
                uint64_t result = joky_cps_result(frame);
                uint32_t out_bits = (uint32_t)result;
                float out; memcpy(&out, &out_bits, 4);
                if (out != narrow[i] + 0.25f) return 50 + i;
                if (joky_runtime_error_code()) return 60 + i;
                joky_cps_frame_free(frame);
            }}
            return 0;
        }}
    "#
    );
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let status = Command::new(executable).status().unwrap();
    assert!(status.success(), "float cps: {status}");
}

#[cfg(unix)]
#[test]
fn aot_mixed_object_host_signatures_match_real_c_registers_and_text_descriptors() {
    let fixture = ScratchDir::new("object-host-abi");
    let object = fixture.path().join("hosts.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("hosts");
    let mut compiler = AotCompiler::new("object-host-abi").unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(include_str!(
                "../../../../tests/fixtures/mixed_object_hosts.jk"
            ))
            .unwrap(),
        )
        .unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
#include <stdint.h>
#include <math.h>
#include <string.h>
#include "joky_vm.h"
#include "joky_runtime.h"
double host_coordinate(int32_t token) { return token == -7 ? -0.25 : 0.0; }
int32_t host_position(int32_t token, double x, double y) { return token == -7 && x == 0.0 && signbit(x) && y == 0.125 ? 42 : 0; }
const JokyText *host_text(int32_t token) {
    static const uint8_t bytes[] = {0xe4, 0xb8, 0xad, 0xf0, 0x9f, 0x99, 0x82};
    static const JokyText text = {bytes, sizeof(bytes)};
    return token == -7 ? &text : NULL;
}
int32_t host_lookup(int32_t token, const JokyText *text) {
    const JokyText *expected = host_text(token);
    return expected && text->byte_length == expected->byte_length && memcmp(text->data, expected->data, text->byte_length) == 0 ? 42 : 0;
}
extern int32_t check_object_hosts(void);
int main(void) { return check_object_hosts() != 42 || joky_runtime_error_code(); }
"#).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}
