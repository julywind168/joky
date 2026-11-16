use super::*;

#[test]
fn generic_borrows_execute_in_standalone_aot() {
    let source = include_str!("../../../../tests/fixtures/generic_borrows.jk");
    let tmp = ScratchDir::new("generic-borrows");
    let mut compiler = AotCompiler::new("generic-borrows").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("borrow_pending").unwrap();
    let object = tmp.path().join("program.o");
    compiler.write_object(&object).unwrap();
    let host = tmp.path().join("host.c");
    std::fs::write(&host, format!(r#"
#include "joky_cps.h"
#include "joky_runtime.h"
int32_t borrow_shared(void), borrow_mutable(void), borrow_slices(void), discard_explicit(void);
int32_t borrow_pending(JokyCpsFrame *);
int main(void) {{
    if (borrow_shared() != 42 || borrow_mutable() != 42 || borrow_slices() != 42 || discard_explicit() != 42) return 1;
    for (int cancel = 0; cancel < 2; ++cancel) {{
        JokyCpsFrame *frame = joky_cps_frame_new({slots});
        int status = borrow_pending(frame);
        if (status != JOKY_CPS_PENDING) return 2;
        if (!cancel) {{
            for (int i = 0; i < 8 && status == JOKY_CPS_PENDING; ++i) status = borrow_pending(frame);
            if (status != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42) return 3;
        }}
        joky_cps_frame_free(frame);
        if (joky_runtime_error_code()) return 4;
    }}
    return 0;
}}
"#)).unwrap();
    let output = tmp.path().join("program");
    link(link_runtime(c_compiler().arg(host).arg(object)), &output);
    let output = Command::new(output).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}
