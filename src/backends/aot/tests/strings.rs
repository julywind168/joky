//! Strings AOT fixtures.
use super::*;

use crate::string_query_model as query_model;

#[test]
#[cfg(unix)]
fn string_s4_matches_models_in_independent_aot_objects() {
    let cases = query_model::cases();
    let fixture = ScratchDir::new("string-queries");
    let object = fixture.path().join("queries.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("queries");
    let mut compiler = AotCompiler::new("string-queries").unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    compiler
        .compile_module(&crate::parser::parse(&cases.source).unwrap())
        .unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let mut harness = String::from("#include \"text.h\"\n#include \"joky_runtime.h\"\n#include <string.h>\n#include <stdio.h>\n");
    for (name, _) in &cases.integers {
        harness.push_str(&format!("extern int32_t {name}(void);\n"));
    }
    harness.push_str(
        "extern int32_t bad_offset(void), huge_offset(void), overflow(void);\nint main(void) {\n",
    );
    for (name, expected) in &cases.integers {
        harness.push_str(&format!("if ({name}() != {expected} || joky_runtime_error_code()) {{ fprintf(stderr, \"{name} failed\\n\"); return 1; }}\n"));
    }
    for (index, (name, expected)) in cases.strings.iter().enumerate() {
        let mut bytes = expected
            .bytes()
            .map(|byte| byte.to_string())
            .collect::<Vec<_>>()
            .join(",");
        if bytes.is_empty() {
            bytes.push('0');
        }
        let length = expected.len();
        harness.push_str(&format!("{{ const uint8_t expected_{index}[] = {{{bytes}}}; JokyOwnedTextV2 output = {{0}}; if (__joky_user_text_v2_{name}(&output) || joky_runtime_error_code() || output.byte_length != {length} || ({length} && memcmp(output.data, expected_{index}, {length}))) {{ fprintf(stderr, \"{name} failed\\n\"); return 2; }} joky_string_result_free_v2(&output); }}\n"));
    }
    harness.push_str("if (bad_offset() != 0 || !joky_runtime_error_code()) return 3; joky_runtime_error_clear();\nif (huge_offset() != 0 || !joky_runtime_error_code()) return 4; joky_runtime_error_clear();\nif (overflow() != 0 || !joky_runtime_error_code()) return 5; joky_runtime_error_clear();\nreturn 0; }\n");
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    assert!(Command::new(program).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn string_builder_s3_runs_source_algorithms_in_independent_aot_objects() {
    let source = include_str!("../../../../tests/fixtures/string_builder.jk");
    let fixture = ScratchDir::new("string-builder");
    let object = fixture.path().join("builder.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("builder");
    let mut compiler = AotCompiler::new("string-builder").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let mut harness = String::from(
        r#"
        #include "text.h"
        #include "joky_runtime.h"
        #include "joky_vm.h"
        #include <string.h>
        extern int32_t operations(void), overflow(void), oversized_capacity(void);
        static int check(int32_t (*entry)(JokyOwnedTextV2 *), const uint8_t *expected, size_t length) {
            JokyOwnedTextV2 output = {0};
            if (entry(&output) || joky_runtime_error_code()) return 0;
            int matches = output.byte_length == length && (!length || !memcmp(output.data, expected, length));
            joky_string_result_free_v2(&output);
            return matches && !output.data && !output.byte_length;
        }
        int main(void) {
    "#,
    );
    let mut growth = String::new();
    for index in 0..100 {
        growth.push(char::from(b'A' + index % 26));
        if index % 10 == 0 {
            growth.push('中');
        }
    }
    let boundaries: String = [
        0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
    ]
    .into_iter()
    .map(|scalar| char::from_u32(scalar).unwrap())
    .collect();
    for (index, (name, expected)) in [
        ("build", "aé中🙂\0tail"),
        ("from_text", "中🙂\0tail!"),
        ("empty", ""),
        ("clear_reuse", "🙂\0"),
        ("growth", growth.as_str()),
        ("scalar_boundaries", boundaries.as_str()),
    ]
    .into_iter()
    .enumerate()
    {
        let mut bytes: Vec<_> = expected.bytes().map(|byte| byte.to_string()).collect();
        if bytes.is_empty() {
            bytes.push("0".into());
        }
        harness.push_str(&format!("const uint8_t expected_{index}[] = {{{}}};\nif (!check(__joky_user_text_v2_{name}, expected_{index}, {})) return {};\n", bytes.join(", "), expected.len(), index + 1));
    }
    harness.push_str(
        r#"
            if (operations() != 42 || joky_runtime_error_code()) return 7;
            if (overflow() != 0 || !joky_runtime_error_code()) return 8;
            joky_runtime_error_clear();
            if (oversized_capacity() != 0 || !joky_runtime_error_code()) return 9;
            joky_runtime_error_clear();
            return 0;
        }
    "#,
    );
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    assert!(Command::new(program).status().unwrap().success());
}

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
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_vm.h"
        typedef struct Arena Arena;
        typedef struct Buffer Buffer;
        extern Arena *__joky_arena_new(void);
        extern void __joky_arena_drop(Arena *);
        extern Buffer *__joky_string_from_view_v2(Arena *, const uint8_t *, uint64_t, size_t, size_t);
        extern Buffer *__joky_string_clone_v2(Arena *, const Buffer *, uint64_t, size_t, size_t);
        extern uint64_t __joky_buffer_len(const Buffer *);
        extern uint8_t *__joky_buffer_load(Buffer *, uint64_t, uint64_t, size_t, size_t);
        extern void __joky_buffer_release(Arena *, Buffer *, uint64_t, size_t, size_t);
        extern void __joky_runtime_require_failed_v2(const Buffer *, uint64_t, size_t, size_t);
        static Arena *arena;
        static Buffer *saved;
        static const uint8_t expected[] = { 0xe4, 0xb8, 0xad, 0xf0, 0x9f, 0x99, 0x82, 0, 't', 'a', 'i', 'l' };
        int32_t save_owned_text_v2(const uint8_t *source) {
            saved = __joky_string_from_view_v2(arena, source, 0, 1, 2);
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
            inspect_text(identity_text(escaped_text(),),) * 100 +
                inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
        fn suspended() @suspends -> Int32 {
            let saved = escaped_text()
            wait_ready()
            inspect_text(identity_text(saved,),) * 100 +
                inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
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
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_vm.h"
        #include "text.h"
        #include <string.h>
        static int ready, starts, inspections;
        static const uint8_t escaped[] = {
            0x68, 0xc3, 0xa9, 0xf0, 0x9f, 0x99, 0x82, '{', 'v', 'a', 'l', 'u', 'e', '}',
            10, 13, 9, 0, '"', '\\', 'e', 'n', 'd'
        };
        static const uint8_t literal[] = { '\\', 'n', '\\', 'r', '\\', 't', '\\', '0', '\\', '"', '\\', '\\' };
        int32_t inspect_text(const JokyText *text) {
            uint64_t length = text->byte_length;
            inspections++;
            if (length == sizeof escaped && !memcmp(text->data, escaped, sizeof escaped)) return 1;
            if (length == sizeof literal && !memcmp(text->data, literal, sizeof literal)) return 2;
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
        static int inspect_owned(int32_t (*entry)(JokyOwnedTextV2 *), const uint8_t *expected, uint64_t length) {
            JokyOwnedTextV2 text = {0};
            if (entry(&text) || text.byte_length != length) return 0;
            int matches = !length || !memcmp(text.data, expected, length);
            joky_string_result_free_v2(&text);
            return matches && !text.data && !text.byte_length;
        }
        int main(void) {
            joky_runtime_error_clear();
            if (!inspect_owned(__joky_user_text_v2_escaped_text, escaped, sizeof escaped)
                || !inspect_owned(__joky_user_text_v2_literal_escape_text, literal, sizeof literal)
                || !inspect_owned(__joky_user_text_v2_empty_text, NULL, 0)) return 1;
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
            let needle = "llo"
            let prefix = "hé"
            let suffix = "lo"
            let other = "héllo"
            int32(string.byte_length(&text)) * 1000000 +
                int32(string.find(&text, &needle).unwrap_or(999)) * 1000 +
                (if string.starts_with(&text, &prefix) { 100 } else { 0 }) +
                (if string.ends_with(&text, &suffix) { 10 } else { 0 }) +
                (if string.equals(&text, &other) { 1 } else { 0 })
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

#[test]
#[cfg(unix)]
fn owning_text_v2_c_roots_copy_inputs_results_and_pending_descriptors() {
    let source = r#"import joky/mem

        import joky/check
        import joky/task
        extern fn delayed_echo(text: String) @suspends -> String
        fn echo(text: String) -> String { let copy = text.clone() text.release() copy }
        fn pending(text: String) @suspends -> String { delayed_echo(text) }
        fn failure(text: String) -> String { let message = "Text root failure" check.require(false, &message) text }
        fn forward(text: String) @suspends -> String { task.yield_now() text }
        fn language(text: String) @suspends -> String { let step = forward step(text) }
        fn ready(text: String) @suspends { text.release() }
        fn forever(text: String) @suspends { while true { task.yield_now() } }
        fn compete() @suspends -> Int32 {
            race { | ready("winner") | forever("unpolled") }
            race { | forever("pending") | ready("winner") }
            parallel { | ready("left") | ready("right") }
            42
        }
        fn optional(flag: Bool) -> Option<String> { if flag { Some("中🙂") } else { None } }
        fn fallible(flag: Bool) -> Result<String, Int32> { if flag { Ok("tail") } else { Err(7) } }
        fn measure(text: &String) -> Int32 { int32(text.byte_length()) }
        fn replace(text: &mut String, next: String) -> String { mem.replace(text, next) }
        fn owners() -> Int32 {
            var text = "first"
            let initial = text.clone()
            let old = replace(&mut text, "second")
            let size = measure
            let a = match optional(true) { Some(value) => size(&value), None => 0 }
            let b = match optional(false) { Some(value) => size(&value), None => 1 }
            let c = match fallible(true) { Ok(value) => size(&value), Err(_) => 0 }
            let d = match fallible(false) { Ok(value) => size(&value), Err(code) => code }
            a + b + c + d + size(&initial) + size(&old) + size(&text)
        }
    "#;
    let fixture = ScratchDir::new("text-root-v2");
    let dir = fixture.path();
    let object = dir.join("text.o");
    let host = dir.join("host.c");
    let program = dir.join("text");
    let mut compiler = AotCompiler::with_symbol_prefix("text", "test_").unwrap();
    compiler
        .compile_with_import_layouts(
            &crate::parser::parse(source).unwrap(),
            &std::collections::HashMap::from([("delayed_echo".into(), 3)]),
        )
        .unwrap();
    let slots = compiler.cps_frame_slots("pending").unwrap();
    let language_slots = compiler.cps_frame_slots("language").unwrap();
    let compete_slots = compiler.cps_frame_slots("compete").unwrap();
    let exports = compiler.text_exports_v2();
    assert!(exports
        .iter()
        .any(|entry| entry.name == "pending" && entry.frame_slots == Some(slots)));
    assert_eq!(
        compiler.function_entry_abi("echo"),
        Some(crate::EntryAbi::OwnedTextV2)
    );
    std::fs::write(dir.join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "text.h"
        #include "joky_runtime.h"
        #include "joky_vm.h"
        #include <string.h>
        extern uint64_t *__joky_cps_slots(JokyCpsFrame *);
        extern int32_t test_owners(void), test_compete(JokyCpsFrame *);
        static int starts, polls;
        int32_t test_delayed_echo(JokyCpsFrame *frame) {
            polls++;
            if (!joky_cps_state(frame)) { starts++; joky_cps_set_state(frame, 1); }
            const JokyText *text = (const JokyText *)(uintptr_t)__joky_cps_slots(frame)[0];
            uint64_t length = text->byte_length;
            if (length != 7 || memcmp(text->data, "A\0hello", 7)) return 2;
            if (polls < 4) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, (uint64_t)(uintptr_t)text);
            return JOKY_CPS_COMPLETED;
        }
        int main(void) {
            if (test_owners() != 35 || joky_runtime_error_code()) return 16;
            uint8_t input[] = "A\0hello";
            JokyOwnedTextV2 output = {0};
            if (__joky_user_text_v2_test_echo(input, 7, &output)) return 1;
            memset(input, 'x', 7);
            if (output.byte_length != 7 || memcmp(output.data, "A\0hello", 7)) return 2;
            joky_string_result_free_v2(&output);
            joky_string_result_free_v2(&output);
            if (__joky_user_text_v2_test_echo(NULL, 0, &output) || output.data || output.byte_length) return 3;
            uint8_t invalid[] = {0xc0, 0x80};
            if (!__joky_user_text_v2_test_echo(invalid, 2, &output) || output.data || output.byte_length) return 4;
            joky_runtime_error_clear();
            if (!__joky_user_text_v2_test_echo(NULL, UINT64_MAX, &output) || output.data || output.byte_length) return 5;
            joky_runtime_error_clear();
            if (!__joky_user_text_v2_test_failure(NULL, 0, &output) || output.data || output.byte_length) return 6;
            joky_runtime_error_clear();
            JokyCpsFrame *frame = joky_cps_frame_new(SLOTS);
            memcpy(input, "A\0hello", 7);
            if (joky_cps_set_text_input_v2(frame, 0, input, 7)) return 7;
            memset(input, 'x', 7);
            for (int i = 0; i < 3; i++) {
                if (__joky_user_text_v2_test_pending(frame) != JOKY_CPS_PENDING) return 8;
                if (!joky_cps_take_text_result_v2(frame, &output) || output.data || output.byte_length) return 9;
            }
            if (__joky_user_text_v2_test_pending(frame) != JOKY_CPS_COMPLETED || starts != 1) return 10;
            if (joky_cps_result(frame) != 0 || joky_cps_take_text_result_v2(frame, &output)) return 11;
            joky_cps_frame_free(frame);
            if (output.byte_length != 7 || memcmp(output.data, "A\0hello", 7)) return 12;
            joky_string_result_free_v2(&output);
            frame = joky_cps_frame_new(SLOTS);
            if (joky_cps_set_text_input_v2(frame, 0, (const uint8_t *)"A\0hello", 7)) return 13;
            polls = 0;
            if (__joky_user_text_v2_test_pending(frame) != JOKY_CPS_PENDING) return 14;
            joky_cps_frame_free(frame);
            frame = joky_cps_frame_new(LANGUAGE_SLOTS);
            if (joky_cps_set_text_input_v2(frame, 0, (const uint8_t *)"中🙂", 7)) return 17;
            if (joky_cps_block_on(__joky_user_text_v2_test_language, frame)
                || joky_cps_take_text_result_v2(frame, &output)) return 18;
            joky_cps_frame_free(frame);
            if (output.byte_length != 7 || memcmp(output.data, "中🙂", 7)) return 19;
            joky_string_result_free_v2(&output);
            frame = joky_cps_frame_new(COMPETE_SLOTS);
            if (joky_cps_block_on(test_compete, frame) || joky_cps_result(frame) != 42) return 20;
            joky_cps_frame_free(frame);
            return joky_runtime_error_code() ? 15 : 0;
        }
    "#
    .replace("LANGUAGE_SLOTS", &language_slots.to_string())
    .replace("COMPETE_SLOTS", &compete_slots.to_string())
    .replace("SLOTS", &slots.to_string());
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    assert!(Command::new(program).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn text_host_descriptors_reuse_storage_and_support_wide_cps_in_aot() {
    let fixture = ScratchDir::new("text-host");
    let object = fixture.path().join("text.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("text");
    let source = format!(
        "{}\n{}",
        crate::text_host_model::LOOPS,
        crate::text_host_model::wide_source()
    );
    let mut compiler = AotCompiler::new("text-host").unwrap();
    compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    let pending_slots = compiler.cps_frame_slots("pending").unwrap();
    let wide_slots = compiler.cps_frame_slots("run").unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "text.h"
        #include "joky_runtime.h"
        #include <stdlib.h>
        #include <string.h>
        int32_t inspect(const JokyText *text) { return (int32_t)text->byte_length; }
        const JokyText *echo(const JokyText *text) { return text; }
        int32_t delayed_echo(JokyCpsFrame *frame) {
            if (joky_cps_state(frame) < 2) {
                joky_cps_set_state(frame, joky_cps_state(frame) + 1);
                return JOKY_CPS_PENDING;
            }
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0]);
            return JOKY_CPS_COMPLETED;
        }
        int32_t wide(JokyCpsFrame *frame) {
            const JokyText *left = (const JokyText *)(uintptr_t)__joky_cps_slots(frame)[1];
            const JokyText *right = (const JokyText *)(uintptr_t)__joky_cps_slots(frame)[65];
            const uint8_t expected[] = {0xe4,0xb8,0xad,0xf0,0x9f,0x99,0x82,0,'t','a','i','l'};
            if (left->byte_length != 4 || memcmp(left->data, "left", 4) ||
                right->byte_length != sizeof expected || memcmp(right->data, expected, sizeof expected) ||
                __joky_cps_slots(frame)[64] != 64 || __joky_cps_slots(frame)[66] != 66) return 2;
            if (!joky_cps_state(frame)) { joky_cps_set_state(frame, 1); return JOKY_CPS_PENDING; }
            joky_cps_set_result(frame, __joky_cps_slots(frame)[65]);
            return JOKY_CPS_COMPLETED;
        }
        int main(void) {
            const size_t length = 1024 * 1024;
            uint8_t *input = malloc(length);
            if (!input) return 1;
            memset(input, 'x', length); input[7] = 0;
            JokyOwnedTextV2 output = {0};
            if (__joky_user_text_v2_borrowed(input, length, &output) || output.byte_length != length || memcmp(output.data, input, length)) return 2;
            joky_string_result_free_v2(&output);
            if (__joky_user_text_v2_copied(input, length, &output) || output.byte_length != length || memcmp(output.data, input, length)) return 3;
            joky_string_result_free_v2(&output);
            JokyCpsFrame *frame = joky_cps_frame_new(PENDING_SLOTS);
            if (joky_cps_set_text_input_v2(frame, 0, input, length)) return 4;
            int status;
            do { status = __joky_user_text_v2_pending(frame); } while (status == JOKY_CPS_PENDING);
            if (status || joky_cps_take_text_result_v2(frame, &output) || output.byte_length != length || memcmp(output.data, input, length)) return 5;
            joky_cps_frame_free(frame);
            free(input);
            joky_string_result_free_v2(&output);
            frame = joky_cps_frame_new(WIDE_SLOTS);
            do { status = __joky_user_text_v2_run(frame); } while (status == JOKY_CPS_PENDING);
            if (status || joky_cps_take_text_result_v2(frame, &output) || output.byte_length != 12) return 6;
            joky_cps_frame_free(frame);
            joky_string_result_free_v2(&output);
            return joky_runtime_error_code() != 0;
        }
    "#.replace("PENDING_SLOTS", &pending_slots.to_string()).replace("WIDE_SLOTS", &wide_slots.to_string());
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    assert!(Command::new(program).status().unwrap().success());
}
