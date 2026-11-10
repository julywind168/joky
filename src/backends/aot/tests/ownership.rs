//! Ownership AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn multi_owners_execute_in_standalone_native_code() {
    let ast =
        crate::parser::parse(include_str!("../../../../tests/fixtures/multi_owners.jk")).unwrap();
    let fixture = ScratchDir::new("multi-owners");
    let object = fixture.path().join("owners.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("owners");
    let mut compiler = AotCompiler::new("multi-owners").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        #include <stdio.h>
        #include "joky_runtime.h"
        static int32_t events[64];
        static int count;
        void record_drop(int32_t value) { if (count < 64) events[count++] = value; }
        extern int32_t multi_scope(void), multi_move(void), multi_replace(void), multi_reinitialize(void);
        extern int32_t multi_reinitialize_failure(void), multi_call(void), multi_early(void), multi_loop(void);
        extern int32_t multi_nested(void), multi_partial(void), multi_partial_return(void), multi_partial_break(void);
        extern int32_t multi_partial_nested(void), multi_argument_failure(void), multi_failing_hook(void);
        extern int32_t multi_original_failure(void), multi_failed_overwrite(void), multi_failed_return(void);
        extern int32_t multi_references(void), multi_reuse_budget(void);
        int main(void) {
            struct Case { int32_t (*call)(void); int failure; int count; int32_t events[8]; } cases[] = {
                {multi_scope, 0, 0, {0}},
                {multi_move, 0, 0, {0}},
                {multi_replace, 0, 0, {0}},
                {multi_reinitialize, 0, 0, {0}},
                {multi_reinitialize_failure, 1, 0, {0}},
                {multi_call, 0, 0, {0}},
                {multi_early, 0, 0, {0}},
                {multi_loop, 0, 0, {0}},
                {multi_nested, 0, 0, {0}},
                {multi_partial, 1, 0, {0}},
                {multi_partial_return, 0, 0, {0}},
                {multi_partial_break, 0, 0, {0}},
                {multi_partial_nested, 1, 0, {0}},
                {multi_argument_failure, 1, 0, {0}},
                {multi_original_failure, 1, 0, {0}},

                {multi_references, 0, 0, {0}}, {multi_reuse_budget, 0, 0, {0}}
            };
            for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); i++) {
                count = 0;
                int32_t result = cases[i].call();
                if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
                    fprintf(stderr, "Multi-owner case %u: result or runtime error mismatch\n", i);
                    return 1;
                }
                if (count != cases[i].count) {
                    fprintf(stderr, "Multi-owner case %u: expected %d drops, got %d\n", i, cases[i].count, count);
                    return 2;
                }
                for (int j = 0; j < count; j++) {
                    if (events[j] != cases[i].events[j]) {
                        fprintf(stderr, "Multi-owner case %u: event %d expected %d, got %d\n", i, j, cases[i].events[j], events[j]);
                        return 3;
                    }
                }
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
fn owner_fields_execute_in_standalone_native_code() {
    let ast =
        crate::parser::parse(include_str!("../../../../tests/fixtures/owner_fields.jk")).unwrap();
    let fixture = ScratchDir::new("owner-fields");
    let object = fixture.path().join("fields.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("fields");
    let mut compiler = AotCompiler::new("owner-fields").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        #include <stdio.h>
        #include "joky_runtime.h"
        static int32_t events[64];
        static int count;
        void record_drop(int32_t value) { if (count < 64) events[count++] = value; }
        extern int32_t fields_rhs_metadata(void);
        extern int32_t fields_extract_failure(void);
        extern int32_t fields_extract_original_failure(void);
        extern int32_t fields_reinitialize_failure(void);
        extern int32_t fields_nested_generic_drop(void);
        extern int32_t fields_generic_drop(void);
        extern int32_t fields_first(void);
        extern int32_t fields_middle(void);
        extern int32_t fields_last(void);
        extern int32_t fields_temporary(void);
        extern int32_t fields_return(void);
        extern int32_t fields_reinitialize(void);
        extern int32_t fields_replace(void);
        extern int32_t fields_replace_edges(void);
        extern int32_t fields_nested(void);
        extern int32_t fields_nested_temporary(void);
        extern int32_t fields_nested_replace(void);
        extern int32_t fields_subtree(void);
        extern int32_t fields_subtree_temporary(void);
        extern int32_t fields_rhs_failure(void);
        extern int32_t fields_drop_failure(void);
        extern int32_t fields_original_failure(void);
        extern int32_t fields_loop(void);
        extern int32_t fields_branch_true(void);
        extern int32_t fields_branch_false(void);
        extern int32_t fields_early(void);
        extern int32_t fields_transparent(void);
        extern int32_t fields_references(void);
        extern int32_t fields_replace_references(void);
        extern int32_t fields_subtree_replace(void);

        int main(void) {
            struct Case { int32_t (*call)(void); int failure; int count; int32_t events[16]; } cases[] = {
                {fields_rhs_metadata, 0, 0, {0}},
                {fields_extract_original_failure, 1, 0, {0}},
                {fields_nested_generic_drop, 0, 1, {0}},
                {fields_generic_drop, 0, 1, {0}},
                {fields_first, 0, 0, {0}},
                {fields_middle, 0, 0, {0}},
                {fields_last, 0, 0, {0}},
                {fields_temporary, 0, 0, {0}},
                {fields_return, 0, 0, {0}},
                {fields_reinitialize, 0, 0, {0}},
                {fields_replace, 0, 0, {0}},
                {fields_replace_edges, 0, 0, {0}},
                {fields_nested, 0, 0, {0}},
                {fields_nested_temporary, 0, 0, {0}},
                {fields_nested_replace, 0, 0, {0}},
                {fields_subtree, 0, 0, {0}},
                {fields_subtree_temporary, 0, 0, {0}},
                {fields_rhs_failure, 1, 0, {0}},
                {fields_original_failure, 1, 0, {0}},
                {fields_loop, 0, 0, {0}},
                {fields_branch_true, 0, 0, {0}},
                {fields_branch_false, 0, 0, {0}},
                {fields_early, 0, 0, {0}},
                {fields_transparent, 0, 0, {0}},
                {fields_references, 0, 0, {0}},
                {fields_replace_references, 0, 0, {0}},
                {fields_subtree_replace, 0, 0, {0}},
            };
            for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); i++) {
                count = 0;
                int32_t result = cases[i].call();
                if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) {
                    fprintf(stderr, "Owner-field case %u: result or runtime error mismatch\n", i);
                    return 1;
                }
                if (count != cases[i].count) {
                    fprintf(stderr, "Owner-field case %u: expected %d drops, got %d\n", i, cases[i].count, count);
                    return 2;
                }
                for (int j = 0; j < count; j++) {
                    if (events[j] != cases[i].events[j]) {
                        fprintf(stderr, "Owner-field case %u: event %d expected %d, got %d\n", i, j, cases[i].events[j], events[j]);
                        return 3;
                    }
                }
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
fn owning_elements_execute_in_standalone_native_code() {
    let ast = crate::parser::parse(include_str!(
        "../../../../tests/fixtures/owning_elements.jk"
    ))
    .unwrap();
    let fixture = ScratchDir::new("owning-elements");
    let object = fixture.path().join("elements.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("elements");
    let mut compiler = AotCompiler::new("owning-elements").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        #include <stdio.h>
        #include "joky_runtime.h"
        static int32_t events[32];
        static int count;
        void record_drop(int32_t value) { if (count < 32) events[count++] = value; }
        extern int32_t tuples_ctor_return(void), tuples_loop(void), tuples_branch(void), tuples_phantom_reference(void);
        extern int32_t tuples_nested(void), tuples_discard(void), tuples_discard_all(void), tuples_subtuple(void), tuples_zip(void), tuples_zip_empty(void), tuples_overwrite(void), tuples_record(void), tuples_array(void), tuples_enum(void), tuples_empty_lane(void), tuples_rhs_failure(void), tuples_discard_failure(void), tuples_early_return(void);
        extern int32_t elements_borrow_shared(void), elements_borrow_nested(void), elements_borrow_wrapper(void), elements_borrow_array(void);
        extern int32_t elements_borrow_bounds(void), elements_borrow_empty(void), elements_borrow_failure(void), elements_borrow_early_return(void), elements_borrow_drop_failure(void);
        extern int32_t elements_growth(void), elements_set(void), elements_release(void);
        extern int32_t elements_overwrite(void), elements_nested(void), elements_groups(void);
        extern int32_t elements_empty(void), elements_rhs_failure(void), elements_drop_failure(void);
        extern int32_t elements_cleanup_failure(void), elements_generic_metadata(void);
        extern int32_t enums_borrow_boundary(void), enums_phantom_reference(void);
        extern int32_t enums_phantom(void), enums_buffers(void), enums_field(void), enums_array_set(void);
        extern int32_t enums_custom(void), enums_pop(void), enums_result(void), enums_overwrite(void);
        extern int32_t enums_nested_array(void), enums_array(void), enums_wildcard(void), enums_early_exit(void);
        extern int32_t enums_failure(void), enums_cleanup_failure(void), enums_empty_replacement_failure(void), enums_methods(void);
        int main(void) {
            struct Case { int32_t (*call)(void); int failure; int count; int32_t events[16]; } cases[] = {
                {tuples_ctor_return, 0, 0, {0}},
                {tuples_loop, 0, 0, {0}},
                {tuples_branch, 0, 0, {0}},
                {tuples_phantom_reference, 0, 0, {0}},
                {tuples_nested, 0, 0, {0}},
                {tuples_discard, 0, 0, {0}},
                {tuples_discard_all, 0, 1, {20}},
                {tuples_subtuple, 0, 0, {0}},
                {tuples_zip, 0, 0, {0}},
                {tuples_zip_empty, 0, 0, {0}},
                {tuples_overwrite, 0, 0, {0}},
                {tuples_record, 0, 0, {0}},
                {tuples_array, 0, 0, {0}},
                {tuples_enum, 0, 0, {0}},
                {tuples_empty_lane, 0, 0, {0}},
                {tuples_rhs_failure, 1, 0, {0}},
                {tuples_early_return, 0, 0, {0}},
                {elements_borrow_shared, 0, 0, {0}},
                {elements_borrow_nested, 0, 0, {0}},
                {elements_borrow_wrapper, 0, 0, {0}},
                {elements_borrow_array, 0, 0, {0}},
                {elements_borrow_bounds, 1, 0, {0}},
                {elements_borrow_empty, 1, 0, {0}},
                {elements_borrow_failure, 1, 0, {0}},
                {elements_borrow_early_return, 0, 0, {0}},
                {elements_growth, 0, 0, {0}},
                {elements_generic_metadata, 0, 0, {0}},
                {elements_set, 0, 0, {0}},
                {elements_release, 0, 0, {0}},
                {elements_overwrite, 0, 0, {0}},
                {elements_nested, 0, 0, {0}},
                {elements_groups, 0, 0, {0}},
                {enums_borrow_boundary, 0, 0, {0}},
                {enums_phantom_reference, 0, 0, {0}},
                {enums_phantom, 0, 0, {0}},
                {enums_buffers, 0, 0, {0}},
                {enums_field, 0, 0, {0}},
                {enums_array_set, 0, 0, {0}},
                {enums_custom, 0, 0, {0}},
                {enums_pop, 0, 0, {0}},
                {enums_result, 0, 0, {0}},
                {enums_overwrite, 0, 0, {0}},
                {enums_nested_array, 0, 0, {0}},
                {enums_array, 0, 0, {0}},
                {enums_wildcard, 0, 0, {0}},
                {enums_early_exit, 0, 0, {0}},
                {enums_failure, 1, 0, {0}},

                {enums_methods, 0, 0, {0}},
                {elements_empty, 1, 0, {0}},
                {elements_rhs_failure, 1, 0, {0}},

            };
            for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); i++) {
                count = 0;
                int32_t result = cases[i].call();
                if (cases[i].failure != !!joky_runtime_error_code() || (!cases[i].failure && result != 42)) return 1;
                if (count != cases[i].count) return 2;
                for (int j = 0; j < count; j++) {
                    if (events[j] != cases[i].events[j]) {
                        fprintf(stderr, "Owning element case %u: event %d expected %d, got %d\n", i, j, cases[i].events[j], events[j]);
                        return 3;
                    }
                }
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
fn borrowed_matches_execute_in_standalone_native_code() {
    let source = format!(
        "{}\n{}",
        include_str!("../../../../tests/fixtures/borrowed_matches.jk"),
        include_str!("../../../../tests/fixtures/borrowed_match_suspend.jk")
    );
    let fixture = ScratchDir::new("borrowed-matches");
    let object = fixture.path().join("matches.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("matches");
    let mut compiler = AotCompiler::new("borrowed-matches").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "match_library",
            include_str!("../../../../tests/fixtures/borrowed_match_library.jk"),
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let slots = compiler.cps_frame_slots("shared_match_pending").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        include_str!("../../../../tests/fixtures/borrowed_matches.c")
            .replace("PENDING_SLOTS", &slots.to_string()),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn mutable_borrows_execute_in_standalone_native_code() {
    let source = format!(
        "{}\n{}",
        include_str!("../../../../tests/fixtures/mutable_borrows.jk"),
        include_str!("../../../../tests/fixtures/mutable_borrow_suspend.jk")
    );
    let fixture = ScratchDir::new("mutable-borrows");
    let object = fixture.path().join("borrows.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("borrows");
    let mut compiler = AotCompiler::new("mutable-borrows").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "mutable_library",
            include_str!("../../../../tests/fixtures/mutable_borrow_library.jk"),
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let slots = compiler.cps_frame_slots("mutable_pending").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        include_str!("../../../../tests/fixtures/mutable_borrows.c")
            .replace("PENDING_SLOTS", &slots.to_string()),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn memory_replace_executes_in_standalone_native_code() {
    let source = format!(
        "{}\n{}",
        include_str!("../../../../tests/fixtures/memory_replace.jk"),
        include_str!("../../../../tests/fixtures/memory_replace_suspend.jk").replacen(
            "import joky/mem\n",
            "",
            1
        )
    );
    let fixture = ScratchDir::new("memory-replace");
    let object = fixture.path().join("replace.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("replace");
    let mut compiler = AotCompiler::new("memory-replace").unwrap();
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "replace_library",
            include_str!("../../../../tests/fixtures/memory_replace_library.jk"),
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
        .unwrap();
    compiler.compile_module(&ast).unwrap();
    let slots = compiler.cps_frame_slots("replace_pending").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        include_str!("../../../../tests/fixtures/memory_replace.c")
            .replace("PENDING_SLOTS", &slots.to_string()),
    )
    .unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn mem_operations_execute_in_standalone_native_code() {
    let ast = crate::parser::parse(include_str!("../../../../tests/fixtures/mem.jk")).unwrap();
    let fixture = ScratchDir::new("mem-operations");
    let object = fixture.path().join("mem.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("mem");
    let mut compiler = AotCompiler::new("mem").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, include_str!("../../../../tests/fixtures/mem.c")).unwrap();
    link(
        link_runtime(c_compiler().arg(host).arg(object)),
        &executable,
    );
    assert!(Command::new(executable).status().unwrap().success());
}
