//! Arrays AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn buffer_storage_executes_and_reports_failures_in_standalone_native_code() {
    let source = format!(
        "{} {}",
        crate::core::ir::BUFFER_TEST_SOURCE,
        r#"
        fn bounds_fail() -> Int32 { let b = allocate(2); load(&b, 0) }
        fn overflow_fail() -> Int32 { let b = allocate(18446744073709551615); 1 }
        fn budget_fail() -> Int32 { let b = allocate(16777216); 1 }
    "#
    );
    let ast = crate::core::ir::buffer_test_module(&source);
    let fixture = ScratchDir::new("buffer-storage");
    let dir = fixture.path();
    let object = dir.join("buffers.o");
    let host = dir.join("host.c");
    let executable = dir.join("buffers");
    let mut compiler = AotCompiler::new("buffers").unwrap();
    compiler.compile_module(&ast).unwrap();
    assert!(compiler.requires_runtime());
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        extern int32_t check(void), empty(void), bounds_fail(void), overflow_fail(void), budget_fail(void);
        int main(void) {
            for (int i = 0; i < 3; i++) {
                if (check() != 42 || empty() != 42 || joky_runtime_error_code()) return 1;
                (void)bounds_fail();
                if (!joky_runtime_error_code()) return 2;
                joky_runtime_error_clear();
                (void)overflow_fail();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)budget_fail();
                if (!joky_runtime_error_code()) return 4;
                joky_runtime_error_clear();
            }
            return 0;
        }
    "#).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn arrays_source_library_executes_in_standalone_native_code() {
    let source = format!(
        "{} fn task() @suspends -> Int32 {{ array_growth() }}",
        include_str!("../../../../tests/fixtures/arrays.jk")
    );
    let ast = crate::parser::parse(&source).unwrap();
    let fixture = ScratchDir::new("arrays");
    let dir = fixture.path();
    let object = dir.join("arrays.o");
    let host = dir.join("host.c");
    let executable = dir.join("arrays");
    let mut compiler = AotCompiler::new("arrays").unwrap();
    let names = compiler.compile_module(&ast).unwrap();
    assert!(names.iter().all(|name| !name.starts_with("joky/")
        && name != "array_make"
        && name != "array_identity"
        && name != "array_choose"
        && name != "array_read"
        && name != "array_change"));
    let slots = compiler.cps_frame_slots("task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t array_empty(void), array_growth(void), array_access(void), array_moves(void), array_branches(void), array_reserve_existing(void);
        extern int32_t array_capacity_overflow(void), array_reserve_overflow(void), array_budget_failure(void);
        extern int32_t task(JokyCpsFrame *);
        int main(void) {{
            for (int i = 0; i < 3; i++) {{
                if (array_empty() != 42 || array_growth() != 42 || array_access() != 42 || array_moves() != 42 || array_branches() != 42 || array_reserve_existing() != 42) return 1;
                if (joky_runtime_error_code()) return 2;
                (void)array_capacity_overflow();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)array_reserve_overflow();
                if (!joky_runtime_error_code()) return 4;
                joky_runtime_error_clear();
                (void)array_budget_failure();
                if (!joky_runtime_error_code()) return 5;
                joky_runtime_error_clear();
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (task(frame) != 0 || joky_cps_result(frame) != 42) return 6;
                joky_cps_frame_free(frame);
            }}
            return 0;
        }}
    "#)).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn owning_arrays_cross_suspending_calls_in_standalone_native_code() {
    let source = r#"
        import joky/array as array.{type Array}
        import joky/task
        extern fn pause() @suspends

        fn make_values() -> Array<Int32> {
            var values = array.new::<Int32>()
            array.push(&mut values, 7)
            array.push(&mut values, 35)
            values
        }
        fn relay(values: Array<Int32>) @suspends -> Array<Int32> {
            pause()
            values
        }
        fn inspect(values: Array<Int32>) -> Int32 {
            let result = int32(array.len(&values)) + array.get(&values, 0).unwrap_or(0) + array.get(&values, 1).unwrap_or(0)
            values.release()
            result
        }
        fn ordinary_task() @suspends -> Int32 {
            let values = make_values()
            let moved = relay(values)
            inspect(moved)
        }

        fn hold(values: Array<Int32>) @suspends {
            task.yield_now()
            values.release()
        }
        fn finish() @suspends {}
        fn scope_run() @suspends -> Int32 {
            let values = array.new::<Int32>()
            parallel { | hold(values) }
            42
        }
        fn race_run() @suspends -> Int32 {
            let values = array.new::<Int32>()
            race { | hold(values) | finish() }
            42
        }
    "#;
    let ast = crate::parser::parse(source).unwrap();
    let fixture = ScratchDir::new("owning-array-suspension");
    let dir = fixture.path();
    let object = dir.join("arrays.o");
    let host = dir.join("host.c");
    let executable = dir.join("arrays");
    let mut compiler = AotCompiler::new("owning-array-suspension").unwrap();
    compiler.compile_module(&ast).unwrap();
    let slots = compiler.cps_frame_slots("ordinary_task").unwrap();
    let scope_slots = compiler.cps_frame_slots("scope_run").unwrap();
    let race_slots = compiler.cps_frame_slots("race_run").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        format!(
            r#"
            #include <stdint.h>
            #include "joky_runtime.h"
            #include "joky_cps.h"
            int32_t pause(JokyCpsFrame *frame) {{
                if (joky_cps_state(frame) == 0) {{
                    joky_cps_set_state(frame, 1);
                    return JOKY_CPS_PENDING;
                }}
                return JOKY_CPS_COMPLETED;
            }}
            extern int32_t ordinary_task(JokyCpsFrame *);
            extern int32_t scope_run(JokyCpsFrame *);
            extern int32_t race_run(JokyCpsFrame *);
            int main(void) {{
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (ordinary_task(frame) != JOKY_CPS_PENDING) return 1;
                if (ordinary_task(frame) != JOKY_CPS_COMPLETED) return 2;
                if (joky_cps_result(frame) != 44 || joky_runtime_error_code()) return 3;
                joky_cps_frame_free(frame);

                frame = joky_cps_frame_new({scope_slots});
                int32_t status = scope_run(frame);
                if (status == JOKY_CPS_PENDING) status = scope_run(frame);
                if (status != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42 || joky_runtime_error_code()) return 4;
                joky_cps_frame_free(frame);

                frame = joky_cps_frame_new({race_slots});
                status = race_run(frame);
                if (status == JOKY_CPS_PENDING) status = race_run(frame);
                if (status != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 42 || joky_runtime_error_code()) return 5;
                joky_cps_frame_free(frame);
                return 0;
            }}
            "#
        ),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(&host).arg(&object)),
        &executable,
    );
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn generic_arrays_execute_layouts_and_errors_in_standalone_native_code() {
    let source = format!(
        "{} fn task() @suspends -> Int32 {{ generic_array_records() }}",
        include_str!("../../../../tests/fixtures/generic_arrays.jk")
    );
    let fixture = ScratchDir::new("generic-arrays");
    let dir = fixture.path();
    let object = dir.join("arrays.o");
    let host = dir.join("host.c");
    let executable = dir.join("arrays");
    let mut compiler = AotCompiler::new("generic-arrays").unwrap();
    let names = compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    assert!(!names
        .iter()
        .any(|name| name.starts_with("array_") || name.starts_with("__joky_generic_")));
    let slots = compiler.cps_frame_slots("task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t generic_array_scalars(void), generic_array_records(void), generic_array_tuples(void), generic_array_enums(void), generic_array_zero_size(void);
        extern int32_t generic_array_failure(void), generic_array_zero_failure(void);
        extern int32_t task(JokyCpsFrame *);
        int main(void) {{
            for (int i = 0; i < 3; i++) {{
                if (generic_array_scalars() != 42 || generic_array_records() != 42 || generic_array_tuples() != 42 || generic_array_enums() != 42 || generic_array_zero_size() != 42) return 1;
                if (joky_runtime_error_code()) return 2;
                (void)generic_array_failure();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)generic_array_zero_failure();
                if (!joky_runtime_error_code()) return 4;
                joky_runtime_error_clear();
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (task(frame) != 0 || joky_cps_result(frame) != 42) return 5;
                joky_cps_frame_free(frame);
            }}
            return 0;
        }}
    "#)).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn reference_arrays_preserve_referents_in_standalone_native_code() {
    let source = format!(
        "{} fn task() @suspends -> Int32 {{ reference_array_growth() }}",
        include_str!("../../../../tests/fixtures/reference_arrays.jk")
    );
    let fixture = ScratchDir::new("reference-arrays");
    let dir = fixture.path();
    let object = dir.join("arrays.o");
    let host = dir.join("host.c");
    let executable = dir.join("arrays");
    let mut compiler = AotCompiler::new("reference-arrays").unwrap();
    let names = compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    assert!(names
        .iter()
        .all(|name| name.starts_with("reference_array_") || name == "task"));
    let slots = compiler.cps_frame_slots("task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t reference_array_growth(void), reference_array_owner_end(void), reference_array_records(void), reference_array_tuples(void), reference_array_enums(void), reference_array_graphs(void), reference_array_numeric_contract(void), reference_array_returned(void);
        extern int32_t task(JokyCpsFrame *);
        int main(void) {{
            for (int i = 0; i < 3; i++) {{
                if (reference_array_growth() != 42 || reference_array_owner_end() != 42 || reference_array_records() != 42 || reference_array_tuples() != 42 || reference_array_enums() != 42 || reference_array_graphs() != 42 || reference_array_numeric_contract() != 42 || reference_array_returned() != 42) return 1;
                if (joky_runtime_error_code()) return 2;
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (task(frame) != 0 || joky_cps_result(frame) != 42) return 5;
                joky_cps_frame_free(frame);
            }}
            return 0;
        }}
    "#)).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn borrowed_arrays_execute_views_and_bounds_in_standalone_native_code() {
    let source = format!(
        "{} fn task() @suspends -> Int32 {{ borrowed_array_ranges() }}",
        include_str!("../../../../tests/fixtures/borrowed_arrays.jk")
    );
    let fixture = ScratchDir::new("borrowed-arrays");
    let dir = fixture.path();
    let object = dir.join("arrays.o");
    let host = dir.join("host.c");
    let executable = dir.join("arrays");
    let mut compiler = AotCompiler::new("borrowed-arrays").unwrap();
    let names = compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    assert!(names
        .iter()
        .all(|name| name.starts_with("borrowed_array_") || name == "task"));
    let slots = compiler.cps_frame_slots("task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t borrowed_array_elements(void), borrowed_array_ranges(void), borrowed_array_empty(void), borrowed_array_references(void), borrowed_array_aggregates(void), borrowed_array_enums(void), borrowed_array_zero_size(void), borrowed_array_loop(void), borrowed_array_bad_element(void), borrowed_array_bad_range(void), borrowed_array_bad_end(void), borrowed_array_bad_index_max(void), borrowed_array_bad_subslice(void), borrowed_array_bad_slice_element(void);
        extern int32_t task(JokyCpsFrame *);
        int main(void) {{
            for (int i = 0; i < 3; i++) {{
                if (borrowed_array_elements() != 42 || borrowed_array_ranges() != 42 || borrowed_array_empty() != 42 || borrowed_array_references() != 42 || borrowed_array_aggregates() != 42 || borrowed_array_enums() != 42 || borrowed_array_zero_size() != 42 || borrowed_array_loop() != 42) return 1;
                if (joky_runtime_error_code()) return 2;
                (void)borrowed_array_bad_element();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)borrowed_array_bad_range();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)borrowed_array_bad_end();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)borrowed_array_bad_index_max();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)borrowed_array_bad_subslice();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)borrowed_array_bad_slice_element();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (task(frame) != 0 || joky_cps_result(frame) != 42) return 5;
                joky_cps_frame_free(frame);
            }}
            return 0;
        }}
    "#)).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn released_arrays_reuse_budget_and_preserve_values_in_standalone_native_code() {
    let source = format!(
        "{} fn task() @suspends -> Int32 {{ released_array_references() }}",
        include_str!("../../../../tests/fixtures/released_arrays.jk")
    );
    let fixture = ScratchDir::new("released-arrays");
    let dir = fixture.path();
    let object = dir.join("arrays.o");
    let host = dir.join("host.c");
    let executable = dir.join("arrays");
    let mut compiler = AotCompiler::new("released-arrays").unwrap();
    let names = compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    assert!(!names
        .iter()
        .any(|name| name == "released_dispose" || name == "released_local"));
    let slots = compiler.cps_frame_slots("task").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        #include "joky_cps.h"
        extern int32_t released_array_reuse(void), released_array_growth(void), released_array_references(void), released_array_zero_size(void), released_array_local_effect(void), released_array_branches(void);
        extern int32_t released_array_failure(void), released_array_growth_failure(void);
        extern int32_t task(JokyCpsFrame *);
        int main(void) {{
            for (int i = 0; i < 3; i++) {{
                if (released_array_reuse() != 42 || released_array_growth() != 42 || released_array_references() != 42 || released_array_zero_size() != 42 || released_array_local_effect() != 42 || released_array_branches() != 42) return 1;
                if (joky_runtime_error_code()) return 2;
                (void)released_array_failure();
                if (!joky_runtime_error_code()) return 3;
                joky_runtime_error_clear();
                (void)released_array_growth_failure();
                if (!joky_runtime_error_code()) return 4;
                joky_runtime_error_clear();
                JokyCpsFrame *frame = joky_cps_frame_new({slots});
                if (task(frame) != 0 || joky_cps_result(frame) != 42) return 5;
                joky_cps_frame_free(frame);
            }}
            return 0;
        }}
    "#)).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn owning_slices_execute_in_standalone_native_code() {
    let fixture = ScratchDir::new("owning-slices");
    let object = fixture.path().join("slices.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("slices");
    let mut compiler = AotCompiler::new("owning-slices").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "slice_library",
            include_str!("../../../../tests/fixtures/slice_library.jk"),
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(
            &crate::parser::parse(include_str!("../../../../tests/fixtures/owning_slices.jk"))
                .unwrap(),
            &modules,
        )
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        include_str!("../../../../tests/fixtures/owning_slices.c"),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn swap_remove_executes_in_standalone_native_code() {
    let fixture = ScratchDir::new("swap-remove");
    let object = fixture.path().join("remove.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("remove");
    let mut compiler = AotCompiler::new("swap-remove").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "remove_library",
            include_str!("../../../../tests/fixtures/swap_remove_library.jk"),
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(
            &crate::parser::parse(include_str!("../../../../tests/fixtures/swap_remove.jk"))
                .unwrap(),
            &modules,
        )
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        include_str!("../../../../tests/fixtures/swap_remove.c"),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn ordered_arrays_executes_in_standalone_native_code() {
    let fixture = ScratchDir::new("ordered-arrays");
    let object = fixture.path().join("remove.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("remove");
    let mut compiler = AotCompiler::new("ordered-arrays").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "ordered_library",
            include_str!("../../../../tests/fixtures/ordered_array_library.jk"),
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(
            &crate::parser::parse(include_str!("../../../../tests/fixtures/ordered_arrays.jk"))
                .unwrap(),
            &modules,
        )
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        include_str!("../../../../tests/fixtures/ordered_arrays.c"),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn array_management_executes_in_standalone_native_code() {
    let fixture = ScratchDir::new("array-management");
    let object = fixture.path().join("remove.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("remove");
    let mut compiler = AotCompiler::new("array-management").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "management_library",
            include_str!("../../../../tests/fixtures/array_management_library.jk"),
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(
            &crate::parser::parse(include_str!(
                "../../../../tests/fixtures/array_management.jk"
            ))
            .unwrap(),
            &modules,
        )
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        include_str!("../../../../tests/fixtures/array_management.c"),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}
