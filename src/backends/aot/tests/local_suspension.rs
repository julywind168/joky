use super::*;

#[test]
fn local_suspension_segments_execute_in_standalone_aot() {
    let source = include_str!("../../../../tests/fixtures/local_suspension_segments.jk");
    let tmp = ScratchDir::new("local-suspension");
    let mut compiler = AotCompiler::new("local-suspension").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_or_else(|error| panic!("{error}"));
    let mut declarations = String::new();
    let mut cases = String::new();
    for name in [
        "local_closures",
        "local_owners",
        "local_consumed_owners",
        "local_control_flow",
        "local_pipelines",
        "local_without_suspension",
    ] {
        declarations.push_str(&format!("int32_t {name}(JokyCpsFrame *);\n"));
        let slots = compiler.cps_frame_slots(name).unwrap();
        cases.push_str(&format!("{{{name}, {slots}}},\n"));
    }
    let object = tmp.path().join("program.o");
    compiler.write_object(&object).unwrap();
    let host = tmp.path().join("host.c");
    std::fs::write(&host, format!(r#"
#include "joky_cps.h"
#include "joky_runtime.h"
{declarations}
int main(void) {{
    struct {{ int32_t (*call)(JokyCpsFrame *); size_t slots; }} cases[] = {{ {cases} }};
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {{
        for (int cancel = 0; cancel < 2; ++cancel) {{
            JokyCpsFrame *frame = joky_cps_frame_new(cases[i].slots);
            int status = cases[i].call(frame);
            if (!cancel) {{
                for (int poll = 0; poll < 16 && status == JOKY_CPS_PENDING; ++poll) status = cases[i].call(frame);
                if (status != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42) return 1;
            }}
            joky_cps_frame_free(frame);
            if (joky_runtime_error_code()) return 2;
        }}
    }}
    return 0;
}}
"#)).unwrap();
    let output = tmp.path().join("program");
    link(link_runtime(c_compiler().arg(host).arg(object)), &output);
    let output = Command::new(output).output().unwrap();
    assert!(output.status.success(), "{output:?}");
}
