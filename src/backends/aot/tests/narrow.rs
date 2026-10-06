//! Narrow-integer AOT fixtures linked against real C code.
use super::*;

#[cfg(unix)]
#[test]
fn aot_narrow_integers_match_real_c_layout_and_calls() {
    let source = r#"
        extern fn c_add8(a: Int8, b: Int8) -> Int8
        extern fn c_mul16(a: Int16, b: Int16) -> Int16
        extern fn c_wrap16(a: UInt16, b: UInt16) -> UInt16
        #repr(C) struct Narrow { a: Int8 ,b: Int16 ,c: UInt16 ,d: UInt8 }
        unsafe extern "C" fn c_bump(p: &mut Int8)
        unsafe extern "C" fn c_widen(p: &Int16) -> Int16
        unsafe extern "C" fn c_mix(p: &mut Narrow) -> UInt32

        fn add8(a: Int8, b: Int8) -> Int8 { c_add8(a, b) }
        fn mul16(a: Int16, b: Int16) -> Int16 { c_mul16(a, b) }
        fn wrap16(a: UInt16, b: UInt16) -> UInt16 { c_wrap16(a, b) }
        // A plain narrow return checks the boundary lane without references.
        fn constant() -> Int8 { -127 }
        fn bump() -> Int8 {
            var v: Int8 = -128
            unsafe { c_bump(&mut v) }
            v
        }
        fn widen() -> Int16 {
            let v: Int16 = -32768
            unsafe { c_widen(&v) }
        }
        fn mix() -> UInt32 {
            var n = Narrow { a: -1 ,b: -32768 ,c: 65535 ,d: 200 }
            unsafe { c_mix(&mut n) }
        }
    "#;
    let ast = crate::parser::parse(source).unwrap();
    let fixture = ScratchDir::new("narrow-externs");
    let dir = fixture.path();
    let object = dir.join("narrow.o");
    let host = dir.join("host.c");
    let executable = dir.join("narrow");
    let mut compiler = AotCompiler::new("narrow-externs").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include <stdint.h>
        #include <stddef.h>
        #include "joky_runtime.h"
        typedef struct { int8_t a; int16_t b; uint16_t c; uint8_t d; } CNarrow;
        int8_t c_add8(int8_t a, int8_t b) { return (int8_t)(a + b); }
        int16_t c_mul16(int16_t a, int16_t b) { return (int16_t)(a * b); }
        uint16_t c_wrap16(uint16_t a, uint16_t b) { return (uint16_t)(a + b); }
        void c_bump(int8_t *p) { *p = (int8_t)(*p + 1); }
        int16_t c_widen(const int16_t *p) { return *p; }
        uint32_t c_mix(CNarrow *p) {
            p->a = (int8_t)(p->a - 1);
            p->b = (int16_t)(p->b - 1);
            p->c = (uint16_t)(p->c - 1);
            p->d = (uint8_t)(p->d + 55);
            return (uint32_t)(uint8_t)p->a | ((uint32_t)(uint16_t)p->b << 8)
                 | ((uint32_t)(uint16_t)p->c << 24) | ((uint32_t)p->d << 16);
        }
        extern int8_t add8(int8_t, int8_t), bump(void), constant(void);
        extern int16_t mul16(int16_t, int16_t), widen(void);
        extern uint16_t wrap16(uint16_t, uint16_t);
        extern uint32_t mix(void);
        int main(void) {
            joky_runtime_error_clear();
            if (sizeof(CNarrow) != 8 || offsetof(CNarrow, a) != 0 || offsetof(CNarrow, b) != 2
                || offsetof(CNarrow, c) != 4 || offsetof(CNarrow, d) != 6) return 1;
            if (add8(-128, -1) != 127) return 2;
            if (add8(127, 1) != -128) return 3;
            if (mul16(-32768, -1) != -32768) return 4;
            if (mul16(-32768, 2) != 0) return 5;
            if (wrap16(65535, 2) != 1) return 6;
            if (constant() != -127) return 11;
            if (bump() != -127) return 7;
            if (widen() != -32768) return 8;
            // a|b<<8|d<<16|c<<24 after the C-side mutations: 0xFE|0x7FFF00|0xFF0000|0xFFFE0000.
            if (mix() != 0xFEFFFFFEu) return 9;
            if (joky_runtime_error_code()) return 10;
            return 0;
        }
    "#;
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let status = Command::new(executable).status().unwrap();
    assert!(status.success(), "narrow externs: {status}");
}
