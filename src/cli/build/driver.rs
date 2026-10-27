//! Render the C entry without depending on installed headers.
use super::build_error;
use crate::cli::EntryPoint;
use joky::{Error, ErrorCode, Type};

pub(super) fn render(
    entry: &EntryPoint,
    main_slots: Option<usize>,
    requires_runtime: bool,
) -> Result<String, Error> {
    let main = if entry.suspends {
        async_main(entry, main_slots)?
    } else {
        sync_main(entry, requires_runtime)
    };
    let runtime = if requires_runtime {
        "extern void joky_runtime_error_clear(void);\nextern int32_t joky_runtime_error_code(void);\nextern void joky_runtime_print_error(void);"
    } else {
        ""
    };
    Ok(format!(
        "#include <stdint.h>\n#include <stddef.h>\n{runtime}\n{main}\n"
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
         extern int32_t joky_fs_block_on_native(int32_t (*)(JokyCpsFrame *), JokyCpsFrame *);\n\
         extern int32_t __joky_user_main(JokyCpsFrame *);\n\
         int main(void) {{\n\
             joky_runtime_error_clear();\n\
             JokyCpsFrame *frame = joky_cps_frame_new({slots});\n\
             int32_t status = joky_fs_block_on_native(__joky_user_main, frame);\n\
             int result = 1;\n\
             if (status == 0 && !joky_runtime_error_code()) result = {result};\n\
             joky_cps_frame_free(frame);\n\
             if (joky_runtime_error_code()) joky_runtime_print_error();\n\
             return result;\n\
         }}"
    ))
}

/// Startup policy belongs to CLI-generated executables, not the embedding ABI.
pub(super) fn startup(
    driver: String,
    mode: joky::runtime::parallel::ExecutionMode,
    project: bool,
) -> String {
    let (workers, call, driver) = match mode {
        joky::runtime::parallel::ExecutionMode::Local => (
            0,
            "joky_local_main()",
            driver.replacen("int main(void)", "static int joky_local_main(void)", 1),
        ),
        joky::runtime::parallel::ExecutionMode::Parallel { workers } => {
            (workers, "joky_parallel_main(workers)", driver)
        }
    };
    let (headers, enter, prepare) = if project {
        (
            include_str!("project_headers.c"),
            include_str!("project_main.c"),
            "if (joky_project_enter() != 0) return 1;",
        )
    } else {
        ("", "", "")
    };
    let parser = include_str!("startup.c");
    format!("{headers}\n{parser}\n{driver}\n{enter}\nint main(int argc, char **argv) {{\n    uint32_t workers = {workers};\n    if (joky_startup_workers(argc, argv, &workers) != 0) return 1;\n    {prepare}\n    return {call};\n}}\n")
}

#[cfg(test)]
mod tests;
