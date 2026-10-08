//! Ownership AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn drop_hooks_execute_in_standalone_native_code() {
    let ast =
        crate::parser::parse(include_str!("../../../../tests/fixtures/dropped_owners.jk")).unwrap();
    let fixture = ScratchDir::new("dropped-owners");
    let object = fixture.path().join("drop.o");
    let host = fixture.path().join("host.c");
    let executable = fixture.path().join("drop");
    let mut compiler = AotCompiler::new("drop").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, r#"
        #include <stdint.h>
        #include "joky_runtime.h"
        static int32_t events[64];
        static int count;
        void record_drop(int32_t value) { if (count < 64) events[count++] = value; }
        extern int32_t drop_scope(void), drop_moves(void), drop_replace(void), drop_loops(void);
        extern int32_t drop_failure(void), drop_early(void), drop_nested(void), drop_generic(void);
        extern int32_t drop_discard(void), drop_partial(void), drop_failing_hook(void), drop_failed_return(void);
        extern int32_t drop_mutated_child(void), drop_reuse_budget(void), drop_leading_owner(void);
        int main(void) {
            struct Case { int32_t (*call)(void); int failure; int count; int32_t events[3]; } cases[] = {
                {drop_scope, 0, 2, {3, 1}}, {drop_moves, 0, 1, {1}},
                {drop_replace, 0, 2, {1, 3}}, {drop_loops, 0, 3, {0, 1, 2}},
                {drop_failure, 1, 2, {2, 1}}, {drop_early, 0, 2, {2, 1}},
                {drop_nested, 0, 2, {1, 2}}, {drop_generic, 0, 1, {4}},
                {drop_discard, 0, 1, {5}}, {drop_partial, 1, 1, {2}},
                {drop_failing_hook, 1, 2, {2, 1}}, {drop_failed_return, 1, 3, {-1, 2, 1}},
                {drop_mutated_child, 0, 2, {1, 9}}, {drop_reuse_budget, 0, 0, {0}},
                {drop_leading_owner, 0, 2, {1, 2}}
            };
            for (unsigned i = 0; i < sizeof(cases)/sizeof(cases[0]); i++) {
                count = 0;
                int32_t result = cases[i].call();
                if (cases[i].failure != !!joky_runtime_error_code()) return 1;
                if (!cases[i].failure && result != 42) return 2;
                if (count != cases[i].count) return 3;
                for (int j = 0; j < count; j++) if (events[j] != cases[i].events[j]) return 4;
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
                {multi_scope, 0, 6, {20, 21, 22, 10, 11, 12}},
                {multi_move, 0, 3, {30, 31, 32}},
                {multi_replace, 0, 6, {10, 11, 12, 20, 21, 22}},
                {multi_reinitialize, 0, 6, {10, 11, 12, 20, 21, 22}},
                {multi_reinitialize_failure, 1, 6, {10, 11, 12, 20, 21, 22}},
                {multi_call, 0, 6, {20, 21, 22, 10, 11, 12}},
                {multi_early, 0, 6, {20, 21, 22, 10, 11, 12}},
                {multi_loop, 0, 6, {10, 11, 12, 20, 21, 22}},
                {multi_nested, 0, 8, {1, 10, 11, 12, 20, 29, 22, 3}},
                {multi_partial, 1, 2, {1, 2}},
                {multi_partial_return, 0, 2, {1, 2}},
                {multi_partial_break, 0, 2, {2, 1}},
                {multi_partial_nested, 1, 4, {11, 20, 21, 22}},
                {multi_argument_failure, 1, 4, {22, 10, 11, 12}},
                {multi_failing_hook, 1, 6, {9, 1, 2, 10, 11, 12}},
                {multi_original_failure, 1, 3, {9, 1, 2}},
                {multi_failed_overwrite, 1, 6, {9, 1, 2, 9, 3, 4}},
                {multi_failed_return, 1, 6, {9, 1, 2, 10, 11, 12}},
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
                {fields_rhs_metadata, 0, 4, {22, 1, 23, 3}},
                {fields_extract_failure, 1, 3, {1, 2, 3}},
                {fields_extract_original_failure, 1, 3, {2, 1, 3}},
                {fields_reinitialize_failure, 1, 6, {2, 3, 4, 5, 6, 1}},
                {fields_first, 0, 3, {1, 2, 3}},
                {fields_middle, 0, 3, {2, 1, 3}},
                {fields_last, 0, 3, {3, 1, 2}},
                {fields_temporary, 0, 3, {2, 1, 3}},
                {fields_return, 0, 3, {1, 3, 2}},
                {fields_reinitialize, 0, 6, {1, 3, 2, 4, 5, 6}},
                {fields_replace, 0, 5, {2, 5, 1, 8, 3}},
                {fields_replace_edges, 0, 5, {1, 3, 4, 2, 6}},
                {fields_nested, 0, 5, {2, 9, 1, 3, 8}},
                {fields_nested_temporary, 0, 5, {2, 1, 3, 9, 8}},
                {fields_nested_replace, 0, 6, {2, 9, 1, 5, 3, 8}},
                {fields_subtree, 0, 6, {10, 1, 2, 3, 9, 8}},
                {fields_subtree_temporary, 0, 6, {10, 1, 2, 3, 9, 8}},
                {fields_rhs_failure, 1, 4, {4, 1, 2, 3}},
                {fields_drop_failure, 1, 4, {2, 4, 1, 3}},
                {fields_original_failure, 1, 3, {1, 2, 3}},
                {fields_loop, 0, 8, {11, 12, 10, 13, 21, 22, 20, 23}},
                {fields_branch_true, 0, 3, {1, 3, 2}},
                {fields_branch_false, 0, 3, {1, 3, 2}},
                {fields_early, 0, 3, {2, 1, 3}},
                {fields_transparent, 0, 0, {0}},
                {fields_references, 0, 0, {0}},
                {fields_replace_references, 0, 0, {0}},
                {fields_subtree_replace, 0, 10, {10, 1, 2, 3, 9, 20, 4, 5, 6, 8}},
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
                {tuples_ctor_return, 0, 1, {1}},
                {tuples_loop, 0, 6, {1, 2, 3, 4, 6, 5}},
                {tuples_branch, 0, 2, {2, 1}},
                {tuples_phantom_reference, 0, 0, {0}},
                {tuples_nested, 0, 4, {10, 2, 3, 1}},
                {tuples_discard, 0, 3, {1, 3, 2}},
                {tuples_discard_all, 0, 5, {1, 10, 2, 3, 20}},
                {tuples_subtuple, 0, 3, {1, 3, 2}},
                {tuples_zip, 0, 2, {2, 1}},
                {tuples_zip_empty, 0, 2, {1, 2}},
                {tuples_overwrite, 0, 6, {1, 2, 3, 4, 6, 5}},
                {tuples_record, 0, 3, {10, 1, 2}},
                {tuples_array, 0, 6, {1, 2, 4, 3, 5, 6}},
                {tuples_enum, 0, 4, {1, 10, 2, 3}},
                {tuples_empty_lane, 0, 1, {1}},
                {tuples_rhs_failure, 1, 2, {2, 1}},
                {tuples_discard_failure, 1, 3, {2, 3, 1}},
                {tuples_early_return, 0, 2, {2, 1}},
                {elements_borrow_shared, 0, 3, {1, 2, 3}},
                {elements_borrow_nested, 0, 3, {10, 1, 2}},
                {elements_borrow_wrapper, 0, 0, {0}},
                {elements_borrow_array, 0, 1, {1}},
                {elements_borrow_bounds, 1, 2, {2, 1}},
                {elements_borrow_empty, 1, 0, {0}},
                {elements_borrow_failure, 1, 1, {1}},
                {elements_borrow_early_return, 0, 1, {1}},
                {elements_borrow_drop_failure, 1, 2, {3, 2}},
                {elements_growth, 0, 5, {5, 4, 3, 2, 1}},
                {elements_generic_metadata, 0, 1, {1}},
                {elements_set, 0, 5, {2, 5, 3, 4, 1}},
                {elements_release, 0, 2, {2, 1}},
                {elements_overwrite, 0, 3, {2, 1, 3}},
                {elements_nested, 0, 3, {3, 2, 1}},
                {elements_groups, 0, 9, {10, 1, 2, 20, 3, 4, 30, 5, 6}},
                {enums_borrow_boundary, 0, 1, {1}},
                {enums_phantom_reference, 0, 0, {0}},
                {enums_phantom, 0, 0, {0}},
                {enums_buffers, 0, 0, {0}},
                {enums_field, 0, 2, {1, 2}},
                {enums_array_set, 0, 6, {1, 5, 10, 2, 3, 4}},
                {enums_custom, 0, 4, {10, 2, 3, 1}},
                {enums_pop, 0, 3, {3, 1, 2}},
                {enums_result, 0, 6, {2, 3, 10, 4, 5, 1}},
                {enums_overwrite, 0, 4, {1, 10, 2, 3}},
                {enums_nested_array, 0, 5, {40, 30, 20, 10, 1}},
                {enums_array, 0, 4, {10, 2, 3, 1}},
                {enums_wildcard, 0, 3, {10, 1, 2}},
                {enums_early_exit, 0, 1, {1}},
                {enums_failure, 1, 1, {1}},
                {enums_cleanup_failure, 1, 3, {3, 2, 1}},
                {enums_empty_replacement_failure, 1, 4, {2, 4, 3, 1}},
                {enums_methods, 0, 7, {1, 2, 4, 5, 7, 6, 3}},
                {elements_empty, 1, 0, {0}},
                {elements_rhs_failure, 1, 3, {3, 2, 1}},
                {elements_drop_failure, 1, 4, {2, 4, 3, 1}},
                {elements_cleanup_failure, 1, 3, {3, 2, 1}},
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
