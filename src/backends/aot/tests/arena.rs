//! Arena AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn arena_objects_use_task_wrappers_and_cleanup_in_native_objects() {
    let source = include_str!("../../../../tests/fixtures/arena.jk");
    let fixture = ScratchDir::new("arena");
    let dir = fixture.path();
    let object = dir.join("arena.o");
    let host = dir.join("host.c");
    let executable = dir.join("arena");
    let mut compiler = AotCompiler::new("arena").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert!(compiler.requires_runtime());
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include "joky_runtime.h"
        extern int32_t arena_compute(void), arena_scalars(void), arena_nested(void);
        extern int32_t arena_loop(void), arena_return(void), arena_fail(int32_t);
        extern int32_t arena_copy_sum(void), arena_unsigned(void);
        extern void arena_copy(int32_t, int32_t, uint64_t *);
        int main(void) {
            for (int i = 0; i < 100; ++i) {
                if (arena_compute() != 25 || arena_scalars() != 42 || arena_nested() != 22) return 1;
                if (arena_loop() != 33 || arena_return() != 42) return 2;
                if (joky_runtime_error_code()) return 3;
                uint64_t result[2] = { 0, 0 };
                arena_copy(20, 22, result);
                if (result[0] != 20 || result[1] != 22 || arena_copy_sum() != 42 || arena_unsigned() != 42) return 6;
                (void)arena_fail(0);
                if (!joky_runtime_error_code()) return 4;
                joky_runtime_error_clear();
                if (arena_fail(2) != 21 || joky_runtime_error_code()) return 5;
            }
            return 0;
        }
    "#).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn arena_graphs_execute_recursive_and_shared_objects_in_native_code() {
    let source = include_str!("../../../../tests/fixtures/arena_graphs.jk");
    let fixture = ScratchDir::new("arena-graphs");
    let dir = fixture.path();
    let object = dir.join("graphs.o");
    let host = dir.join("host.c");
    let executable = dir.join("graphs");
    let mut compiler = AotCompiler::new("graphs").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    compiler.write_object(&object).unwrap();
    let names = [
        "graph_list",
        "graph_shared",
        "graph_tuple",
        "graph_generic",
        "graph_loop",
        "graph_tree",
        "graph_mutual",
        "graph_generic_node",
        "graph_construct",
        "graph_aggregate",
        "graph_local_borrow",
        "graph_alias",
        "graph_tuple_target",
        "graph_outer_reference",
        "graph_option_method",
        "graph_read_copy",
        "graph_inferred_record",
        "graph_inferred_enum",
        "graph_read_tuple",
        "graph_read_recursive",
        "graph_read_enum",
        "graph_read_nested",
        "graph_forward_nested",
        "graph_forward_enum",
    ];
    let mut harness =
        String::from("#include \"joky_runtime.h\"\nextern int32_t graph_failure(int32_t);\n");
    for name in names {
        harness.push_str(&format!("extern int32_t {name}(void);\n"));
    }
    harness.push_str("int main(void) { for (int i = 0; i < 20; ++i) {\n");
    for name in names {
        harness.push_str(&format!(
            "if ({name}() != 42 || joky_runtime_error_code()) return 1;\n"
        ));
    }
    harness.push_str("(void)graph_failure(0); if (!joky_runtime_error_code()) return 2;\n");
    harness.push_str("joky_runtime_error_clear(); if (graph_failure(1) != 42 || joky_runtime_error_code()) return 3; } return 0; }\n");
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn arena_cycles_execute_sealed_graphs_in_native_code() {
    let source = include_str!("../../../../tests/fixtures/arena_cycles.jk");
    let fixture = ScratchDir::new("arena-cycles");
    let dir = fixture.path();
    let object = dir.join("graphs.o");
    let host = dir.join("host.c");
    let executable = dir.join("graphs");
    let mut compiler = AotCompiler::new("graphs").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    compiler.write_object(&object).unwrap();
    let names = [
        "cycle_self",
        "cycle_pair",
        "cycle_shared",
        "cycle_mutual",
        "cycle_generic",
        "cycle_loop",
        "cycle_data_control",
        "cycle_external",
        "cycle_relink",
        "cycle_unlinked",
        "cycle_hint",
        "cycle_evaluation_order",
        "cycle_contextual_name",
    ];
    let mut harness =
        String::from("#include \"joky_runtime.h\"\nextern int32_t cycle_failure(int32_t);\n");
    for name in names {
        harness.push_str(&format!("extern int32_t {name}(void);\n"));
    }
    harness.push_str("int main(void) { for (int i = 0; i < 20; ++i) {\n");
    for name in names {
        harness.push_str(&format!(
            "if ({name}() != 42 || joky_runtime_error_code()) return 1;\n"
        ));
    }
    harness.push_str("(void)cycle_failure(0); if (!joky_runtime_error_code()) return 2;\n");
    harness.push_str("joky_runtime_error_clear(); if (cycle_failure(1) != 42 || joky_runtime_error_code()) return 3; } return 0; }\n");
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn local_borrows_execute_in_standalone_native_code() {
    let source = include_str!("../../../../tests/fixtures/local_borrows.jk");
    let fixture = ScratchDir::new("local-borrows");
    let dir = fixture.path();
    let object = dir.join("locals.o");
    let host = dir.join("host.c");
    let executable = dir.join("locals");
    let mut compiler = AotCompiler::new("local-borrows").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let mut harness = String::from("#include <stdint.h>\n#include \"joky_runtime.h\"\n");
    let cases = [
        "local_record",
        "local_wide",
        "local_nested",
        "local_enum_tuple",
        "local_snapshot",
        "local_shadow",
        "local_reborrow",
        "local_branches",
        "local_loop",
        "local_jumps",
        "local_early_return",
        "local_recursion",
        "local_empty",
        "local_no_arena_contract",
    ];
    for name in cases {
        harness.push_str(&format!("extern int32_t {name}(void);\n"));
    }
    harness.push_str("extern int32_t local_failure(int32_t);\nextern int32_t local_mixed_failure(int32_t);\nint main(void) { for (int run = 0; run < 20; ++run) {\n");
    for (index, name) in cases.iter().enumerate() {
        harness.push_str(&format!(
            "if ({name}() != 42 || joky_runtime_error_code()) return {};\n",
            index + 1
        ));
    }
    harness.push_str(
        r#"
        (void)local_failure(0);
        if (!joky_runtime_error_code()) return 30;
        joky_runtime_error_clear();
        if (local_failure(1) != 42 || joky_runtime_error_code()) return 31;
        (void)local_mixed_failure(0);
        if (!joky_runtime_error_code()) return 32;
        joky_runtime_error_clear();
        if (local_record() != 42 || joky_runtime_error_code()) return 33;
        } return 0; }
    "#,
    );
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn local_field_borrows_execute_in_standalone_native_code() {
    let source = include_str!("../../../../tests/fixtures/local_fields.jk");
    let fixture = ScratchDir::new("local-fields");
    let dir = fixture.path();
    let object = dir.join("locals.o");
    let host = dir.join("host.c");
    let executable = dir.join("locals");
    let mut compiler = AotCompiler::new("local-fields").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let mut harness = String::from("#include <stdint.h>\n#include \"joky_runtime.h\"\n");
    let cases = [
        "field_siblings",
        "field_nested",
        "field_wide",
        "field_zero_size",
        "field_aggregates",
        "field_snapshot",
        "field_reservation",
        "field_reborrow",
        "field_branches",
        "field_loop",
        "field_loop_merge",
        "field_jumps",
        "field_shadow",
        "field_parameters",
        "field_pattern",
        "field_early_return",
        "field_recursion",
    ];
    for name in cases {
        harness.push_str(&format!("extern int32_t {name}(void);\n"));
    }
    harness.push_str("extern int32_t field_failure(int32_t);\nextern int32_t field_mixed_failure(int32_t);\nint main(void) { for (int run = 0; run < 20; ++run) {\n");
    for (index, name) in cases.iter().enumerate() {
        harness.push_str(&format!(
            "if ({name}() != 42 || joky_runtime_error_code()) return {};\n",
            index + 1
        ));
    }
    harness.push_str(
        r#"
        (void)field_failure(0);
        if (!joky_runtime_error_code()) return 30;
        joky_runtime_error_clear();
        if (field_failure(1) != 42 || joky_runtime_error_code()) return 31;
        (void)field_mixed_failure(0);
        if (!joky_runtime_error_code()) return 32;
        joky_runtime_error_clear();
        if (field_siblings() != 42 || joky_runtime_error_code()) return 33;
        } return 0; }
    "#,
    );
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, harness).unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn call_region_refunds_the_shared_budget_in_native_objects() {
    check_arena_cps_fixture(
        "call-region",
        include_str!("../../../../tests/fixtures/call_region.jk"),
        &[],
        |compiler| {
            let slots = compiler.cps_frame_slots("region_loop_task").unwrap();
            format!(
                r#"
                extern int32_t region_loop_task(JokyCpsFrame *);
                extern void *__joky_arena_alloc(void *, size_t, size_t, uint64_t, size_t, size_t);
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                // Leave room for the live point and a few temporaries, but not twenty.
                if (!__joky_arena_alloc(__joky_cps_arena(frame), (64 << 20) - 192, 1, 0, 0, 0)) return 1;
                extern void __joky_instance_entry_region_probe(const uint64_t *, uint64_t *, void *);
                int32_t point = 0;
                uint64_t args[] = {{(uintptr_t)&point, 0}}, result = 0;
                // Reuse the live parent after each failed child, before the root finishes.
                for (int i = 0; i < 20; ++i) {{
                    __joky_instance_entry_region_probe(args, &result, __joky_cps_arena(frame));
                    if (!joky_runtime_error_code()) return 3;
                    joky_runtime_error_clear();
                }}
                args[1] = 1;
                __joky_instance_entry_region_probe(args, &result, __joky_cps_arena(frame));
                if (result != 42 || joky_runtime_error_code()) return 4;
                if (region_loop_task(frame) != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42) return 2;
                joky_cps_frame_free(frame);
                (void)check;
            "#
            )
        },
    );
}
