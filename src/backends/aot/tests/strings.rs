//! Strings AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn escaped_strings_preserve_bytes_in_native_direct_and_cps_calls() {
    let mut source = include_str!("../../../../tests/fixtures/strings.jk").to_owned();
    source.push_str(
        r#"
        extern fn wait_ready() @suspends
        extern fn inspect_text(value: String,) -> Int32
        fn direct() -> Int32 {
            inspect_text(identity_text(escaped_text(),),) * 100
                + inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
        fn suspended() @suspends -> Int32 {
            let saved = escaped_text()
            wait_ready()
            inspect_text(identity_text(saved,),) * 100
                + inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
    "#,
    );
    let fixture = ScratchDir::new("strings");
    let dir = fixture.path();
    let object = dir.join("strings.o");
    let host = dir.join("host.c");
    let program = dir.join("strings");
    let mut compiler = AotCompiler::new("strings").unwrap();
    compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("suspended").unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_cps.h"
        #include <string.h>
        static int ready, starts, inspections;
        static const uint8_t escaped[] = {
            0x68, 0xc3, 0xa9, 0xf0, 0x9f, 0x99, 0x82, '{', 'v', 'a', 'l', 'u', 'e', '}',
            10, 13, 9, 0, '"', '\\', 'e', 'n', 'd'
        };
        static const uint8_t literal[] = { '\\', 'n', '\\', 'r', '\\', 't', '\\', '0', '\\', '"', '\\', '\\' };
        int32_t inspect_text(const uint8_t *text) {
            uint64_t length;
            memcpy(&length, text, sizeof length);
            inspections++;
            if (length == sizeof escaped && !memcmp(text + 8, escaped, sizeof escaped)) return 1;
            if (length == sizeof literal && !memcmp(text + 8, literal, sizeof literal)) return 2;
            return length == 0 ? 3 : -1;
        }
        int32_t wait_ready(JokyCpsFrame *frame) {
            if (joky_cps_state(frame) == 0) {
                starts++;
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }
            return ready ? JOKY_CPS_COMPLETED : JOKY_CPS_PENDING;
        }
        extern int32_t direct(void), suspended(JokyCpsFrame *);
        extern const uint8_t *escaped_text(void), *literal_escape_text(void), *empty_text(void);
        int main(void) {
            joky_runtime_error_clear();
            if (inspect_text(escaped_text()) != 1 || inspect_text(literal_escape_text()) != 2 || inspect_text(empty_text()) != 3) return 1;
            if (direct() != 123 || joky_runtime_error_code()) return 2;
            inspections = 0;
            JokyCpsFrame *frame = joky_cps_frame_new(SLOTS);
            for (int i = 0; i < 4; i++) {
                joky_runtime_error_clear();
                if (suspended(frame) != JOKY_CPS_PENDING || joky_runtime_error_code()) return 3;
            }
            if (starts != 1 || inspections != 0) return 4;
            ready = 1;
            joky_runtime_error_clear();
            if (suspended(frame) != JOKY_CPS_COMPLETED || joky_runtime_error_code()) return 5;
            if (joky_cps_result(frame) != 123 || inspections != 3 || starts != 1) return 6;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#.replace("SLOTS", &slots.to_string());
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let status = Command::new(program).status().unwrap();
    assert!(status.success(), "native string test exited with {status}");
}

#[test]
#[cfg(unix)]
fn string_queries_link_and_run_in_aot_objects() {
    let source = r#"
        import joky/string
        fn compute() -> Int32 {
            let text = "héllo"
            string.byte_length(text) * 1000000
                + string.find(text, "llo") * 1000
                + string.starts_with(text, "hé") * 100
                + string.ends_with(text, "lo") * 10
                + string.equals(text, "héllo")
        }
    "#;
    let fixture = ScratchDir::new("string-queries");
    let dir = fixture.path();
    let object = dir.join("string-queries.o");
    let host = dir.join("host.c");
    let executable = dir.join("string-queries");
    let mut compiler = AotCompiler::new("string_queries").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdint.h>
        extern int32_t compute(void);
        int main(void) { return compute() == 6003111 ? 0 : 1; }
    "#,
    )
    .unwrap();
    link(
        link_runtime(
            Command::new("cc")
                .args(["-Wall", "-Wextra", "-Werror"])
                .arg(&host)
                .arg(&object),
        ),
        &executable,
    );
    assert!(Command::new(&executable).status().unwrap().success());
}
