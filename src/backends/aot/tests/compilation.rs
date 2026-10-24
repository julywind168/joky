//! Compilation AOT fixtures.
use super::*;

#[test]
fn object_emission_requires_a_successful_single_compilation() {
    let invalid = crate::parser::parse("fn f() -> Bool { 1 }").unwrap();
    let valid = crate::parser::parse("fn f() -> Int32 { 42 }").unwrap();
    let mut compiler = AotCompiler::new("failed").unwrap();
    assert!(compiler.compile_module(&invalid).is_err());
    assert!(compiler.compile_module(&valid).is_err());
    // An empty path also fails at the filesystem layer; require the state error.
    for compiler in [compiler, AotCompiler::new("new").unwrap()] {
        assert!(compiler
            .write_object(std::path::Path::new(""))
            .unwrap_err()
            .message()
            .contains("before successful compilation"));
    }
    let mut compiler = AotCompiler::new("compiled").unwrap();
    compiler.compile_module(&valid).unwrap();
    assert!(compiler.compile_module(&valid).is_err());
}

#[test]
fn scope_aot_lowering_emits_all_branch_frames() {
    let ast = crate::parser::parse(
        r#"
        extern fn wait_value(x: Int32) @suspends -> Int32
        fn left(x: Int32) @suspends { wait_value(x); {} }
        fn right(x: Int32) @suspends { wait_value(x); {} }
        fn all() @suspends -> Int32 {
            join {
                | left(1)
                | right(2)
            }
            42
        }
    "#,
    )
    .unwrap();
    let mut compiler = AotCompiler::new("scope").unwrap();
    compiler.compile_module(&ast).unwrap();
    assert!(compiler.cps_frame_slots("all").unwrap() >= 2);
}

#[test]
#[cfg(unix)]
fn parser_boundaries_execute_in_direct_and_cps_code_in_all_backends() {
    use crate::vm::{VmEngine, VmMode};

    let source = include_str!("../../../../tests/fixtures/parser_boundaries.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42, "{mode:?}");
        assert_eq!(
            vm.call_async("check_cps", &[])
                .unwrap()
                .cps_result()
                .unwrap(),
            42,
            "{mode:?}",
        );
    }

    let fixture = ScratchDir::new("parser-boundaries");
    let object = fixture.path().join("parser.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("parser");
    let mut compiler = AotCompiler::new("parser-boundaries").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("check_cps").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        format!(
            r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            extern int32_t check(void), check_cps(JokyCpsFrame *);
            int main(void) {{
                if (check() != 42 || joky_runtime_error_code()) return 1;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                int32_t status = check_cps(frame);
                int64_t result = joky_cps_result(frame);
                joky_cps_frame_free(frame);
                return status != JOKY_CPS_COMPLETED || result != 42 || joky_runtime_error_code();
            }}
            "#,
        ),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}
