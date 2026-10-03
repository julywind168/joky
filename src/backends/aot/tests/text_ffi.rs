use super::*;
use crate::text_ffi_model as model;

#[test]
#[cfg(unix)]
fn text_ffi_c_fixture_runs_in_independent_aot_process() {
    let fixture = ScratchDir::new("text-ffi-aot");
    let object = fixture.path().join("program.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("program");
    let mut compiler = AotCompiler::new("text-ffi").unwrap();
    for (name, signature) in model::signatures() {
        compiler.register_text_ffi_fn(&name, signature).unwrap();
    }
    compiler
        .compile_module(&crate::parser::parse(model::SOURCE).unwrap())
        .unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    let mut harness = format!("{}\n#include \"text.h\"\n#include \"joky_runtime.h\"\n#include <stdio.h>\nextern int32_t length_text(void), scalar_and_unit(void), four_texts(void), partial_input_failure(void);\nint main(void) {{\n", model::C_SOURCE);
    for &(name, expected, frees) in model::TEXTS {
        let bytes = if expected.is_empty() {
            "0".into()
        } else {
            expected
                .bytes()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        harness.push_str(&format!("{{ text_ffi_reset(); const uint8_t expected[] = {{{bytes}}}; JokyOwnedTextV2 output = {{0}}; if (__joky_user_text_v2_{name}(&output) || joky_runtime_error_code() || output.byte_length != {} || (output.byte_length && memcmp(output.data, expected, output.byte_length)) || calls != 1 || frees != {frees}) {{ fprintf(stderr, \"{name} failed\\n\"); return 1; }} joky_string_result_free_v2(&output); }}\n", expected.len()));
    }
    for &(name, _, calls, frees) in model::FAILURES {
        harness.push_str(&format!("{{ text_ffi_reset(); JokyOwnedTextV2 output = {{0}}; if (!__joky_user_text_v2_{name}(&output) || !joky_runtime_error_code() || output.data || calls != {calls} || frees != {frees}) {{ fprintf(stderr, \"{name} failed: calls=%d frees=%d\\n\", calls, frees); return 2; }} joky_runtime_error_clear(); }}\n"));
    }
    harness.push_str("text_ffi_reset(); partial_input_failure(); if (!joky_runtime_error_code() || calls || frees) return 4; joky_runtime_error_clear(); if (length_text() != 12 || scalar_and_unit() != 42 || four_texts() != 15 || joky_runtime_error_code()) return 3; return 0; }\n");
    std::fs::write(&host, harness).unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    assert!(Command::new(program).status().unwrap().success());
}

#[test]
fn text_ffi_aot_registration_checks_signatures_and_lifecycle() {
    let mut compiler = AotCompiler::new("text-ffi-invalid").unwrap();
    assert!(compiler
        .register_text_ffi_fn(
            "bad",
            FunctionSignature::new(vec![Type::Rune], Type::String)
        )
        .is_err());
    assert!(compiler
        .register_text_ffi_fn("bad;", FunctionSignature::new(vec![], Type::String))
        .is_err());
    compiler
        .register_text_ffi_fn("host", FunctionSignature::new(vec![], Type::String))
        .unwrap();
    assert!(compiler
        .compile_module(
            &crate::parser::parse(
                "extern fn host() @suspends -> String fn run() @suspends -> String { host() }"
            )
            .unwrap()
        )
        .is_err());
    assert!(compiler
        .register_text_ffi_fn("host", FunctionSignature::new(vec![], Type::String))
        .is_err());
}

#[test]
#[cfg(unix)]
fn text_ffi_aot_prefix_and_mismatched_linked_descriptor_are_checked() {
    let fixture = ScratchDir::new("text-ffi-prefix");
    let object = fixture.path().join("program.o");
    let host = fixture.path().join("host.c");
    let program = fixture.path().join("program");
    let mut compiler = AotCompiler::with_symbol_prefix("text-ffi", "prefixed_").unwrap();
    compiler
        .register_text_ffi_fn("host", FunctionSignature::new(vec![], Type::String))
        .unwrap();
    compiler
        .compile_module(
            &crate::parser::parse("extern fn host() -> String fn run() -> String { host() }")
                .unwrap(),
        )
        .unwrap();
    std::fs::write(
        fixture.path().join("imports.h"),
        compiler.text_ffi_header_v1(),
    )
    .unwrap();
    std::fs::write(fixture.path().join("exports.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    // A scalar descriptor must fail before its callback or invalid result can
    // be interpreted as an owning String handle by generated code.
    std::fs::write(
        &host,
        r#"
        #include "imports.h"
        #include "exports.h"
        #include "joky_runtime.h"
        #include <stdlib.h>
        static int32_t unexpected(void *ctx, const JokyText *in, size_t n,
            uint8_t *buf, size_t cap, JokyTextFfiOutputV1 *out) {
            (void)ctx; (void)in; (void)n; (void)buf; (void)cap; (void)out;
            abort();
        }
        const JokyTextFfiHostV1 __joky_text_ffi_v1_prefixed_host = {
            1, 0, 0, {0}, {0}, JOKY_TEXT_SCALAR, 0, NULL, unexpected, NULL
        };
        int main(void) {
            JokyOwnedTextV2 output = {0};
            return !__joky_user_text_v2_prefixed_run(&output) ||
                !joky_runtime_error_code() || output.data;
        }
    "#,
    )
    .unwrap();
    link(link_runtime(c_compiler().arg(&host).arg(&object)), &program);
    assert!(Command::new(program).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn text_ffi_c_embedding_example_links_and_runs() {
    let fixture = ScratchDir::new("text-ffi-example");
    let object = fixture.path().join("program.o");
    let program = fixture.path().join("program");
    let source = "extern fn c_echo(text: &String) -> String fn run() -> String { let text = \"Hello, 中🙂\"; c_echo(&text) }";
    let ast = crate::parser::parse(source).unwrap();
    let crate::parser::ast::Item::ExternFn(f) = &ast.items[0] else {
        unreachable!()
    };
    let mut compiler = AotCompiler::new("text-ffi-example").unwrap();
    compiler
        .register_text_ffi_fn(
            "c_echo",
            FunctionSignature::new(vec![f.params[0].ty.clone()], Type::String),
        )
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    std::fs::write(
        fixture.path().join("text_ffi_imports.h"),
        compiler.text_ffi_header_v1(),
    )
    .unwrap();
    std::fs::write(
        fixture.path().join("text_ffi_exports.h"),
        compiler.text_header_v2(),
    )
    .unwrap();
    compiler.write_object(&object).unwrap();
    link(
        link_runtime(
            c_compiler()
                .arg("-I")
                .arg(fixture.path())
                .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/text_ffi.c"))
                .arg(&object),
        ),
        &program,
    );
    assert!(Command::new(program).status().unwrap().success());
}
