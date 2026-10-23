use super::*;

#[test]
#[cfg(unix)]
fn aot_for_indexed_checks_u64_exhaustion_without_wrapping() {
    let ast = crate::parser::test_support::indexed_for_u64_boundary();
    let fixture = ScratchDir::new("indexed-u64");
    let object = fixture.path().join("indexed.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("indexed");
    let mut compiler = AotCompiler::new("indexed-u64").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        extern int32_t boundary(int32_t);
        int main(void) {
            joky_runtime_error_clear();
            if (boundary(0) != 0 || boundary(2) != 2 || joky_runtime_error_code()) return 1;
            boundary(3);
            if (!joky_runtime_error_code()) return 2;
            joky_runtime_error_clear();
            if (boundary(1) != 1 || joky_runtime_error_code()) return 3;
            return 0;
        }
    "#,
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}
