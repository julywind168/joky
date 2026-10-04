//! Float AOT fixtures linked against real C code.
use super::*;

#[cfg(unix)]
#[test]
fn aot_float_externs_and_repr_c_match_real_c_code() {
    let source = r#"
        extern fn c_add(a: Float64, b: Float64) -> Float64
        extern fn c_scale(v: Float32) -> Float32
        extern fn c_mix(count: Int32, factor: Float64) -> Float64
        extern fn c_dot(x: Float64, y: Float64, z: Float64) -> Float64
        #repr(C) struct Vec2 { x: Float64 y: Float32 }
        unsafe extern "C" fn c_offset(v: &Vec2) -> Float64

        fn add(a: Float64, b: Float64) -> Float64 { c_add(a, b) }
        fn scale(v: Float32) -> Float32 { c_scale(v) }
        fn mix(count: Int32, factor: Float64) -> Float64 { c_mix(count, factor) }
        fn dot(x: Float64, y: Float64, z: Float64) -> Float64 { c_dot(x, y, z) }
        fn offset() -> Float64 {
            let v = Vec2 { x: 2.5 y: 0.5 }
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
        fn square(v: Float64) @suspends -> Float64 { task.yield_now() v * v }
        fn bump(v: Float32) @suspends -> Float32 { task.yield_now() v + 0.25 }
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
