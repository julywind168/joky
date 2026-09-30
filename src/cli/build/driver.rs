//! Render the C entry and CLI hosts without depending on installed headers.
use super::build_error;
use crate::cli::{hosts, EntryPoint};
use joky::{
    parser::ast::{Item, Module},
    Error, ErrorCode, Type,
};

pub(super) fn render(
    ast: &Module,
    entry: &EntryPoint,
    main_slots: Option<usize>,
    requires_runtime: bool,
) -> Result<String, Error> {
    let main = if entry.suspends {
        async_main(entry, main_slots)?
    } else {
        sync_main(entry, requires_runtime)
    };
    let host = hosts::all()
        .into_iter()
        .filter(|host| {
            ast.items
                .iter()
                .any(|item| matches!(item, Item::ExternFn(f) if f.name == host.name))
        })
        .map(|host| host.c_definition())
        .collect::<Vec<_>>()
        .join("\n");
    let runtime = if requires_runtime {
        "extern void joky_runtime_error_clear(void);\nextern int32_t joky_runtime_error_code(void);\nextern void joky_runtime_print_error(void);"
    } else {
        ""
    };
    Ok(format!(
        "#include <stdint.h>\n#include <inttypes.h>\n#include <stdio.h>\n{runtime}\n{host}\n{main}\n"
    ))
}

fn sync_main(entry: &EntryPoint, requires_runtime: bool) -> String {
    let (result_type, call, result) = if entry.result_type == Type::Int32 {
        (
            "int32_t",
            "int32_t result = __joky_user_main();",
            "(uint8_t)result",
        )
    } else {
        ("void", "__joky_user_main();", "0")
    };
    let (clear, check) = if requires_runtime {
        (
            "joky_runtime_error_clear();",
            "if (joky_runtime_error_code()) { joky_runtime_print_error(); return 1; }",
        )
    } else {
        ("", "")
    };
    format!(
        "extern {result_type} __joky_user_main(void);\n\
         int main(void) {{ {clear} {call} {check} return {result}; }}"
    )
}

fn async_main(entry: &EntryPoint, main_slots: Option<usize>) -> Result<String, Error> {
    let slots = main_slots
        .ok_or_else(|| build_error(ErrorCode::InvalidState, "Missing main CPS frame layout"))?;
    let result = if entry.result_type == Type::Int32 {
        "(uint8_t)joky_cps_result(frame)"
    } else {
        "0"
    };
    Ok(format!(
        "typedef struct JokyCpsFrame JokyCpsFrame;\n\
         extern JokyCpsFrame *joky_cps_frame_new(size_t);\n\
         extern void joky_cps_frame_free(JokyCpsFrame *);\n\
         extern uint64_t joky_cps_result(const JokyCpsFrame *);\n\
         extern int32_t joky_cps_block_on(int32_t (*)(JokyCpsFrame *), JokyCpsFrame *);\n\
         extern int32_t __joky_user_main(JokyCpsFrame *);\n\
         int main(void) {{\n\
             joky_runtime_error_clear();\n\
             JokyCpsFrame *frame = joky_cps_frame_new({slots});\n\
             int32_t status = joky_cps_block_on(__joky_user_main, frame);\n\
             int result = 1;\n\
             if (status == 0 && !joky_runtime_error_code()) result = {result};\n\
             joky_cps_frame_free(frame);\n\
             if (joky_runtime_error_code()) joky_runtime_print_error();\n\
             return result;\n\
         }}"
    ))
}
