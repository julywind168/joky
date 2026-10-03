//! Strings AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn owned_text_v2_runtime_helpers_link_with_relocatable_aot_literal_sources() {
    let fixture = ScratchDir::new("owned-text-v2");
    let object = fixture.path().join("text.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("text");
    let source = r#"extern fn save_owned_text_v2(source: String) -> Int32
        fn snapshot() -> Int32 { save_owned_text_v2("中🙂\0tail") }"#;
    let mut compiler = AotCompiler::new("owned-text-v2").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        typedef struct Arena Arena;
        typedef struct Buffer Buffer;
        extern Arena *__joky_arena_new(void);
        extern void __joky_arena_drop(Arena *);
        extern Buffer *__joky_string_literal_v2(Arena *, const uint8_t *, uint64_t, size_t, size_t);
        extern Buffer *__joky_string_clone_v2(Arena *, const Buffer *, uint64_t, size_t, size_t);
        extern uint64_t __joky_buffer_len(const Buffer *);
        extern uint8_t *__joky_buffer_load(Buffer *, uint64_t, uint64_t, size_t, size_t);
        extern void __joky_buffer_release(Arena *, Buffer *, uint64_t, size_t, size_t);
        extern void __joky_runtime_require_failed_v2(const Buffer *, uint64_t, size_t, size_t);
        static Arena *arena;
        static Buffer *saved;
        static const uint8_t expected[] = { 0xe4, 0xb8, 0xad, 0xf0, 0x9f, 0x99, 0x82, 0, 't', 'a', 'i', 'l' };
        int32_t save_owned_text_v2(const uint8_t *source) {
            saved = __joky_string_literal_v2(arena, source, 0, 1, 2);
            return saved ? 42 : 0;
        }
        extern int32_t snapshot(void);
        int main(void) {
            arena = __joky_arena_new();
            if (!arena || snapshot() != 42 || joky_runtime_error_code()) return 1;
            Buffer *copy = __joky_string_clone_v2(arena, saved, 0, 3, 4);
            if (!copy || copy == saved || __joky_buffer_len(copy) != sizeof expected) return 2;
            __joky_buffer_release(arena, saved, 0, 5, 6);
            for (uint64_t i = 0; i < sizeof expected; i++) {
                uint8_t *byte = __joky_buffer_load(copy, i, 0, 7, 8);
                if (!byte || *byte != expected[i]) return 3;
            }
            __joky_runtime_require_failed_v2(copy, 0, 9, 12);
            __joky_buffer_release(arena, copy, 0, 13, 15);
            __joky_arena_drop(arena);
            if (!joky_runtime_error_code() || joky_runtime_error_start() != 9 || joky_runtime_error_end() != 12) return 4;
            joky_runtime_print_error();
            joky_runtime_error_clear();
            return 0;
        }
    "#;
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    let output = Command::new(program).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output
            .stderr
            .windows("中🙂\0tail".len())
            .any(|bytes| bytes == "中🙂\0tail".as_bytes()),
        "message must survive arena release: {output:?}"
    );
}

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
