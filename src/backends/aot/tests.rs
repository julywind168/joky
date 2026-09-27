use super::*;
use crate::{FunctionSignature, Type};
use std::{
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Cleanup(std::path::PathBuf);

#[cfg(unix)]
fn link_runtime(command: &mut Command) -> &mut Command {
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let archive = std::fs::read_dir(deps)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("libjoky-")
                && path.extension().is_some_and(|ext| ext == "a")
        })
        .max_by_key(|path| path.metadata().unwrap().modified().unwrap())
        .expect("Cargo must build the runtime archive");
    command.arg(archive);
    if cfg!(target_os = "macos") {
        command.args(["-liconv", "-lSystem"]);
    } else {
        command.args([
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]);
    }
    command
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Create a unique scratch directory removed when the returned guard drops.
fn scratch_dir(label: &str) -> (std::path::PathBuf, Cleanup) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "joky-aot-{label}-{}-{unique}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cleanup = Cleanup(dir.clone());
    (dir, cleanup)
}

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
        fn left(x: Int32) @suspends { wait_value(x) {} }
        fn right(x: Int32) @suspends { wait_value(x) {} }
        fn all() @suspends -> Int32 {
            scope {
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
fn aot_division_reports_failures_through_the_shared_c_runtime() {
    let mut source = include_str!("../../../tests/fixtures/division.jk").to_owned();
    source.push_str(
        r#"
        extern fn record(x: Int32) -> Int32
        fn effects(n: Int32) -> Int32 { record(1); divide(7, n); record(99) }
        fn cps_divide(n: Int32) @suspends -> Int32 { return divide(7, n) }
    "#,
    );
    let (dir, _cleanup) = scratch_dir("division");
    let object = dir.join("division.o");
    let host = dir.join("host.c");
    let program = dir.join("division");
    let mut compiler = AotCompiler::new("division").unwrap();
    compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    assert!(compiler.requires_runtime());
    let slots = compiler.cps_frame_slots("cps_divide").unwrap();
    compiler.write_object(&object).unwrap();
    let start = source.find("a / b").unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_cps.h"
        #include <stdbool.h>
        #include <limits.h>
        static int effects_seen;
        int32_t record(int32_t x) { effects_seen += x; return x; }
        extern int32_t divide(int32_t, int32_t), divide_constant(void), divide_nested(int32_t, int32_t);
        extern int32_t divide_recurse(int32_t), effects(int32_t), divide_skipped(int32_t);
        extern void divide_unit(int32_t);
        extern bool divide_bool(int32_t);
        extern const uint8_t *divide_text(int32_t);
        extern int32_t cps_divide(JokyCpsFrame *);
        int main(void) {
            const int32_t values[] = { INT32_MIN, -7, -1, 0, 1, 7, INT32_MAX };
            for (int i = 0; i < 7; i++) for (int j = 0; j < 7; j++) {
                joky_runtime_error_clear();
                int32_t value = divide(values[i], values[j]);
                if (values[j] == 0) {
                    if (!joky_runtime_error_code() || joky_runtime_error_start() != START || joky_runtime_error_end() != END) return 1;
                } else {
                    int32_t expected = values[i] == INT32_MIN && values[j] == -1 ? INT32_MIN : values[i] / values[j];
                    if (joky_runtime_error_code() || value != expected) return 2;
                }
            }
            joky_runtime_error_clear();
            if (divide_constant() != INT32_MIN || joky_runtime_error_code()) return 3;
            (void)divide_nested(7, 0);
            if (!joky_runtime_error_code() || joky_runtime_error_start() != START) return 4;
            joky_runtime_error_clear(); (void)divide_bool(0);
            if (!joky_runtime_error_code()) return 5;
            joky_runtime_error_clear(); (void)divide_text(0);
            if (!joky_runtime_error_code()) return 6;
            joky_runtime_error_clear(); divide_unit(0);
            if (!joky_runtime_error_code()) return 7;
            joky_runtime_error_clear(); (void)divide_recurse(5);
            if (!joky_runtime_error_code()) return 8;
            joky_runtime_error_clear(); (void)effects(0);
            if (!joky_runtime_error_code() || effects_seen != 1) return 9;
            joky_runtime_error_clear();
            if (divide_skipped(0) != 42 || joky_runtime_error_code()) return 10;
            JokyCpsFrame *frame = joky_cps_frame_new(SLOTS);
            if (cps_divide(frame) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 11;
            if (joky_cps_state(frame) != JOKY_CPS_FAILED) return 12;
            joky_runtime_error_clear();
            if (cps_divide(frame) != JOKY_CPS_INVALID_STATE || joky_runtime_error_code()) return 13;
            joky_cps_frame_free(frame);
            frame = joky_cps_frame_new(SLOTS);
            __joky_cps_slots(frame)[0] = 2;
            if (cps_divide(frame) != JOKY_CPS_COMPLETED || joky_cps_result(frame) != 3 || joky_runtime_error_code()) return 14;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#.replace("START", &start.to_string()).replace("END", &(start + 5).to_string()).replace("SLOTS", &slots.to_string());
    std::fs::write(&host, harness).unwrap();
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
            .arg(&host)
            .arg(&object),
    )
    .arg("-o")
    .arg(&program)
    .output()
    .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let status = Command::new(program).status().unwrap();
    assert!(
        status.success(),
        "native division test exited with {status}"
    );
}

#[test]
#[cfg(unix)]
fn transparent_aliases_use_the_existing_native_and_cps_abis() {
    let mut source = include_str!("../../../tests/fixtures/aliases.jk").to_owned();
    source.push_str(
        r#"
        type Context = Host.Context
        extern fn context() -> Context
        extern fn context_value(value: Context) -> Count
        extern fn wait_value(value: Count) @suspends -> Count
        extern fn inspect_alias_text(value: Text) -> Count
        fn alias_opaque() -> Count { let value: Context = context() context_value(value) }
        fn alias_suspended(value: Count) @suspends -> Count {
            let saved: Text = alias_string()
            let result: Count = wait_value(value)
            let text: Text = alias_text(saved)
            alias_add(result, inspect_alias_text(text))
        }
    "#,
    );
    let (dir, _cleanup) = scratch_dir("aliases");
    let object = dir.join("aliases.o");
    let host = dir.join("host.c");
    let program = dir.join("aliases");
    let mut compiler = AotCompiler::new("aliases").unwrap();
    compiler.register_opaque_type("Host.Context").unwrap();
    compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("alias_suspended").unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_cps.h"
        #include <stdbool.h>
        #include <string.h>
        static int ready, starts;
        uint64_t context(void) { return 42; }
        int32_t context_value(uint64_t value) { return (int32_t)value; }
        int32_t inspect_alias_text(const uint8_t *text) {
            const uint8_t expected[] = { 'h', 0xc3, 0xa9, 0, 'e', 'n', 'd' };
            uint64_t length;
            memcpy(&length, text, sizeof length);
            return length == sizeof expected && !memcmp(text + 8, expected, sizeof expected) ? 10 : -100;
        }
        int32_t wait_value(JokyCpsFrame *frame) {
            if (joky_cps_state(frame) == 0) {
                starts++;
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0] + 1);
            return JOKY_CPS_COMPLETED;
        }
        extern int32_t alias_add(int32_t, int32_t), alias_compute(void), alias_choose(int32_t);
        extern int32_t alias_mixed(int32_t), alias_opaque(void), alias_suspended(JokyCpsFrame *);
        extern bool alias_flag(bool);
        extern void alias_unit(void);
        extern const uint8_t *alias_string(void), *alias_text(const uint8_t *);
        int main(void) {
            joky_runtime_error_clear();
            if (alias_add(20, 22) != 42 || alias_compute() != 7 || alias_choose(0) != 42) return 1;
            if (!alias_flag(false) || alias_flag(true) || alias_mixed(42) != 42) return 2;
            alias_unit();
            const uint8_t *text = alias_text(alias_string());
            const uint8_t expected[] = { 'h', 0xc3, 0xa9, 0, 'e', 'n', 'd' };
            uint64_t length;
            memcpy(&length, text, sizeof length);
            if (length != sizeof expected || memcmp(text + 8, expected, sizeof expected)) return 3;
            if (alias_opaque() != 42 || joky_runtime_error_code()) return 4;
            JokyCpsFrame *frame = joky_cps_frame_new(SLOTS);
            __joky_cps_slots(frame)[0] = 31;
            for (int i = 0; i < 4; i++) {
                joky_runtime_error_clear();
                if (alias_suspended(frame) != JOKY_CPS_PENDING || joky_runtime_error_code()) return 5;
            }
            ready = 1;
            joky_runtime_error_clear();
            if (alias_suspended(frame) != JOKY_CPS_COMPLETED || joky_runtime_error_code()) return 6;
            if (joky_cps_result(frame) != 42 || starts != 1) return 7;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#.replace("SLOTS", &slots.to_string());
    std::fs::write(&host, harness).unwrap();
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
            .arg(&host)
            .arg(&object),
    )
    .arg("-o")
    .arg(&program)
    .output()
    .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let status = Command::new(program).status().unwrap();
    assert!(status.success(), "native alias test exited with {status}");
}

#[test]
#[cfg(unix)]
fn escaped_strings_preserve_bytes_in_native_direct_and_cps_calls() {
    let mut source = include_str!("../../../tests/fixtures/strings.jk").to_owned();
    source.push_str(
        r#"
        extern fn wait_ready() @suspends
        extern fn inspect_text(value: String,) -> Int32
        fn direct() -> Int32 {
            inspect_text(identity_text(escaped_text(),),) * 100
                + inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
        fn suspended() @suspends -> Int32 {
            let saved = escaped_text()
            wait_ready()
            inspect_text(identity_text(saved,),) * 100
                + inspect_text(literal_escape_text(),) * 10 + inspect_text(empty_text(),)
        }
    "#,
    );
    let (dir, _cleanup) = scratch_dir("strings");
    let object = dir.join("strings.o");
    let host = dir.join("host.c");
    let program = dir.join("strings");
    let mut compiler = AotCompiler::new("strings").unwrap();
    compiler
        .compile_module(&crate::parser::parse(&source).unwrap())
        .unwrap();
    let slots = compiler.cps_frame_slots("suspended").unwrap();
    compiler.write_object(&object).unwrap();
    let harness = r#"
        #include "joky_runtime.h"
        #include "joky_cps.h"
        #include <string.h>
        static int ready, starts, inspections;
        static const uint8_t escaped[] = {
            0x68, 0xc3, 0xa9, 0xf0, 0x9f, 0x99, 0x82, '{', 'v', 'a', 'l', 'u', 'e', '}',
            10, 13, 9, 0, '"', '\\', 'e', 'n', 'd'
        };
        static const uint8_t literal[] = { '\\', 'n', '\\', 'r', '\\', 't', '\\', '0', '\\', '"', '\\', '\\' };
        int32_t inspect_text(const uint8_t *text) {
            uint64_t length;
            memcpy(&length, text, sizeof length);
            inspections++;
            if (length == sizeof escaped && !memcmp(text + 8, escaped, sizeof escaped)) return 1;
            if (length == sizeof literal && !memcmp(text + 8, literal, sizeof literal)) return 2;
            return length == 0 ? 3 : -1;
        }
        int32_t wait_ready(JokyCpsFrame *frame) {
            if (joky_cps_state(frame) == 0) {
                starts++;
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }
            return ready ? JOKY_CPS_COMPLETED : JOKY_CPS_PENDING;
        }
        extern int32_t direct(void), suspended(JokyCpsFrame *);
        extern const uint8_t *escaped_text(void), *literal_escape_text(void), *empty_text(void);
        int main(void) {
            joky_runtime_error_clear();
            if (inspect_text(escaped_text()) != 1 || inspect_text(literal_escape_text()) != 2 || inspect_text(empty_text()) != 3) return 1;
            if (direct() != 123 || joky_runtime_error_code()) return 2;
            inspections = 0;
            JokyCpsFrame *frame = joky_cps_frame_new(SLOTS);
            for (int i = 0; i < 4; i++) {
                joky_runtime_error_clear();
                if (suspended(frame) != JOKY_CPS_PENDING || joky_runtime_error_code()) return 3;
            }
            if (starts != 1 || inspections != 0) return 4;
            ready = 1;
            joky_runtime_error_clear();
            if (suspended(frame) != JOKY_CPS_COMPLETED || joky_runtime_error_code()) return 5;
            if (joky_cps_result(frame) != 123 || inspections != 3 || starts != 1) return 6;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#.replace("SLOTS", &slots.to_string());
    std::fs::write(&host, harness).unwrap();
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
            .arg(&host)
            .arg(&object),
    )
    .arg("-o")
    .arg(&program)
    .output()
    .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let status = Command::new(program).status().unwrap();
    assert!(status.success(), "native string test exited with {status}");
}

#[test]
#[cfg(unix)]
fn direct_aot_loops_assignments_and_comparisons_execute_in_a_native_process() {
    let mut source = include_str!("../../../tests/fixtures/loops.jk").to_owned();
    source.push_str(include_str!("../../../tests/fixtures/sequences.jk"));
    source.push_str(include_str!("../../../tests/fixtures/expressions.jk"));
    source.push_str(include_str!("../../../tests/fixtures/conditionals.jk"));
    source.push_str(include_str!("../../../tests/fixtures/jumps.jk"));
    for (name, operator) in [
        ("eq", "=="),
        ("ne", "!="),
        ("lt", "<"),
        ("le", "<="),
        ("gt", ">"),
        ("ge", ">="),
    ] {
        source.push_str(&format!(
            "\nfn cmp_{name}(a: Int32, b: Int32) -> Bool {{ a {operator} b }}"
        ));
    }
    let ast = crate::parser::parse(&source).unwrap();
    let (dir, _cleanup) = scratch_dir("loops");
    let object = dir.join("loops.o");
    let host = dir.join("host.c");
    let executable = dir.join("loops");
    let mut compiler = AotCompiler::new("loops").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdbool.h>
        #include <stdint.h>
        #include <string.h>
        extern int32_t sum(int32_t), nested(int32_t);
        extern int32_t snapshots(void), shadowing(void), condition_effects(void);
        extern int32_t bool_loop(void), comparison_snapshot(void), precedence(void);
        extern void unit_values(void);
        extern int32_t sequence_condition(void), sequence_bindings(void), sequence_scope(void);
        extern int32_t sequence_arguments(void), sequence_precedence(void), sequence_mixed_separators(void);
        extern void sequence_trailing(void);
        extern const uint8_t *string_value(void);
        extern bool cmp_eq(int32_t, int32_t), cmp_ne(int32_t, int32_t);
        extern bool cmp_lt(int32_t, int32_t), cmp_le(int32_t, int32_t);
        extern bool cmp_gt(int32_t, int32_t), cmp_ge(int32_t, int32_t);
        extern int32_t expression_precedence(void), expression_grouping(void);
        extern int32_t expression_associativity(void), expression_unary_precedence(void);
        extern int32_t expression_minimum(void), expression_wrapped_negation(void);
        extern int32_t expression_snapshots(void), expression_sequences(void);
        extern int32_t expression_newline(void), expression_sequence_arguments(void);
        extern int32_t expression_negate(int32_t), expression_not_chain(int32_t);
        extern int32_t expression_truth(int32_t, int32_t), expression_bool_loops(int32_t, int32_t);
        extern bool expression_equal(bool, bool), expression_unequal(bool, bool), expression_not(bool);
        extern int32_t choose(int32_t), conditional_scopes(void), conditional_nested(int32_t, int32_t);
        extern int32_t logical_values(int32_t, int32_t), logical_precedence(void), logical_effects(void);
        extern int32_t logical_snapshots(void), conditional_loop(int32_t), conditional_grouping(void);
        extern int32_t conditional_safe_division(int32_t);
        extern void conditional_unit(bool), conditional_empty(void);
        extern bool conditional_flag(bool);
        extern const uint8_t *conditional_text(bool);
        extern int32_t return_value(int32_t), return_both(int32_t), return_mixed(int32_t), return_else(int32_t);
        extern int32_t return_argument(int32_t), return_operand(int32_t), return_short_circuit(int32_t);
        extern int32_t return_multiline(void), return_sequence(void), return_initializer(void), return_assignment(void);
        extern int32_t return_unary(void), return_nested(void), return_trailing(void), return_condition(void);
        extern int32_t break_only_condition(void), diverging_condition(int32_t), break_or_return(int32_t);
        extern int32_t break_in_return(void), continue_in_return(void), return_or_diverge(int32_t), nested_condition_loops(void);
        extern int32_t break_continue(int32_t), return_loop(int32_t), nested_breaks(void), break_join(void);
        extern int32_t continue_condition(void), break_condition(void), continue_and_break_paths(void), short_circuit_jumps(void);
        extern void return_unit(void);
        extern bool return_bool(bool);
        extern const uint8_t *return_text(bool);
        int main(void) {
            return_unit();
            if (return_multiline() != 42 || return_sequence() != 3) return 31;
            if (return_initializer() != 42 || return_assignment() != 42 || return_unary() != 42) return 32;
            if (return_nested() != 42 || return_trailing() != 42 || return_condition() != 42) return 33;
            if (break_only_condition() != 42 || nested_breaks() != 66 || break_join() != 6) return 34;
            if (break_in_return() != 42 || continue_in_return() != 3 || return_or_diverge(0) != 42 || nested_condition_loops() != 3) return 44;
            if (continue_condition() != 8 || break_condition() != 3 || continue_and_break_paths() != 3 || short_circuit_jumps() != 8) return 35;
            for (int32_t x = -1; x <= 10; x++) {
                if (return_value(x) != x + 1 || return_both(x) != (x < 0 ? -1 : 1)) return 36;
                if (return_mixed(x) != (x < 0 ? -1 : (x + 1) * 2) || return_else(x) != return_mixed(x)) return 37;
                if (return_argument(x) != (x < 0 ? 9 : x + 1) || return_operand(x) != (x < 0 ? 9 : x * 2)) return 38;
                if (return_short_circuit(x) != (x > 0) - (x < 0)) return 39;
                if (diverging_condition(x) != (x > 1 ? x : 1) || break_or_return(x) != (x < 0 ? -1 : 42)) return 40;
                int32_t expected = 0;
                for (int32_t i = 1; i <= x && i <= 5; i++) if (i != 2) expected += i;
                if (break_continue(x) != expected || return_loop(x) != (x >= 3 ? 30 : x > 0 ? x : 0)) return 41;
            }
            if (sum(0) != 0 || sum(1) != 0 || sum(10) != 45 || sum(-1) != 0) return 1;
            if (nested(4) != 10) return 2;
            if (snapshots() != 20123 || shadowing() != 47) return 3;
            if (condition_effects() != 53 || bool_loop() != 3) return 4;
            if (comparison_snapshot() != 2 || precedence() != 9) return 5;
            unit_values();
            if (sequence_condition() != 43 || sequence_bindings() != 45) return 10;
            if (sequence_scope() != 33 || sequence_arguments() != 23) return 11;
            if (sequence_precedence() != 13 || sequence_mixed_separators() != 15) return 12;
            sequence_trailing();
            if (expression_precedence() != 7 || expression_grouping() != 9) return 13;
            if (expression_associativity() != 14 || expression_unary_precedence() != -2) return 14;
            if (expression_minimum() != INT32_MIN || expression_wrapped_negation() != INT32_MIN) return 15;
            if (expression_snapshots() != 89 || expression_sequences() != -5) return 16;
            if (expression_newline() != 3 || expression_sequence_arguments() != -23) return 17;
            if (conditional_scopes() != 34 || logical_precedence() != 1 || logical_effects() != 3) return 22;
            if (logical_snapshots() != 7 || conditional_grouping() != 11) return 23;
            if (conditional_loop(0) != 0 || conditional_loop(3) != 12 || conditional_loop(10) != 32) return 24;
            if (conditional_safe_division(0) != 0 || conditional_safe_division(3) != 4 || conditional_safe_division(-1) != 0) return 25;
            conditional_empty();
            for (int a = 0; a < 2; a++) {
                if (return_bool(a) != !a) return 42;
                const uint8_t *returned = return_text(a);
                uint64_t returned_length;
                memcpy(&returned_length, returned, sizeof returned_length);
                if (returned_length != (a ? 3 : 2) || memcmp(returned + 8, a ? "yes" : "no", returned_length)) return 43;
                if (expression_not(a) != !a) return 18;
                conditional_unit(a);
                if (conditional_flag(a) != !a) return 26;
                const uint8_t *selected = conditional_text(a);
                uint64_t selected_length;
                memcpy(&selected_length, selected, sizeof selected_length);
                if (selected_length != (a ? 3 : 2) || memcmp(selected + 8, a ? "yes" : "no", selected_length)) return 27;
                for (int b = 0; b < 2; b++) {
                    if (expression_equal(a,b) != (a == b) || expression_unequal(a,b) != (a != b)) return 19;
                    int expected = a == b ? 10 : 1;
                    if (expression_truth(a,b) != expected || expression_bool_loops(a,b) != expected) return 20;
                    if (logical_values(a,b) != (a && b) * 10 + (a || b)) return 28;
                    if (conditional_nested(a,b) != (a ? (b ? 11 : 12) : 20)) return 29;
                }
            }
            const uint8_t *text = string_value();
            uint64_t length;
            memcpy(&length, text, sizeof length);
            if (length != 5 || memcmp(text + 8, "after", 5)) return 6;
            const int32_t values[] = { INT32_MIN, -1, 0, 1, INT32_MAX };
            for (int i = 0; i < 5; i++) {
                int32_t value = values[i];
                int32_t negated = value == INT32_MIN ? INT32_MIN : -value;
                if (choose(value) != (value < 0 ? -10 : value > 0 ? 10 : 0)) return 30;
                if (expression_negate(value) != negated || expression_not_chain(value) != (value == 0)) return 21;
                for (int j = 0; j < 5; j++) {
                    int32_t a = values[i], b = values[j];
                    if (cmp_eq(a,b) != (a == b) || cmp_ne(a,b) != (a != b)) return 7;
                    if (cmp_lt(a,b) != (a < b) || cmp_le(a,b) != (a <= b)) return 8;
                    if (cmp_gt(a,b) != (a > b) || cmp_ge(a,b) != (a >= b)) return 9;
                }
            }
            return 0;
        }
    "#,
    )
    .unwrap();
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror"])
            .arg(&host)
            .arg(&object),
    )
    .arg("-o")
    .arg(&executable)
    .output()
    .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let status = Command::new(&executable).status().unwrap();
    assert!(status.success(), "native loop test exited with {status}");
}

#[test]
#[cfg(unix)]
fn direct_aot_object_links_and_executes_unit_and_bool_abis() {
    let ast = crate::parser::parse(
        r#"
        extern fn record(x: Int32) -> Int32
        fn identity(value: Unit) { value }
        fn pick(before: Unit, value: Int32, after: Unit) -> Int32 {
            before
            after
            value
        }
        fn bool_identity(value: Bool) -> Bool { let saved: Bool = value saved }
        fn compute(x: Int32) -> Int32 {
            let unit: Unit = {}
            identity(unit)
            let saved: Int32 = { let x = x + 1 x }
            pick({ record(1) unit }, record(saved + x), { record(2) unit })
        }
    "#,
    )
    .unwrap();
    let (dir, _cleanup) = scratch_dir("direct");
    let object = dir.join("direct.o");
    let host = dir.join("host.c");
    let executable = dir.join("direct");
    let mut compiler = AotCompiler::new("direct").unwrap();
    compiler.compile_module(&ast).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdbool.h>
        #include <stdint.h>
        static int32_t events[3];
        static int count;
        int32_t record(int32_t value) {
            if (count < 3) events[count] = value;
            count++;
            return value;
        }
        extern void identity(void);
        extern int32_t pick(int32_t);
        extern bool bool_identity(bool);
        extern int32_t compute(int32_t);
        int main(void) {
            identity();
            if (pick(7) != 7) return 1;
            if (!bool_identity(true) || bool_identity(false)) return 2;
            if (compute(20) != 41) return 3;
            if (count != 3 || events[0] != 1 || events[1] != 41 || events[2] != 2) return 4;
            return 0;
        }
    "#,
    )
    .unwrap();
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror"])
            .arg(&host)
            .arg(&object),
    )
    .arg("-o")
    .arg(&executable)
    .output()
    .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    assert!(Command::new(&executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn injected_module_symbols_link_without_inheriting_the_export_prefix() {
    use crate::{
        core::resolver::BindingSpec,
        modules::{ModuleRegistry, NativeBinding},
    };
    use std::rc::Rc;

    let mut registry = ModuleRegistry::default();
    // Link-only declarations contain no callable in-process pointers.
    unsafe {
        registry.register_module(
            "user/math",
            [NativeBinding {
                spec: BindingSpec::with_symbol(
                    "math.next",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    "native_increment",
                ),
                entry: None,
                owner: None,
            }],
        )
    }
    .unwrap();
    let mut compiler = AotCompiler::with_symbol_prefix_and_registry(
        "custom_registry",
        "export_",
        Rc::new(registry),
    )
    .unwrap();
    let ast = crate::parser::parse(
        "import user/math extern fn legacy(x: Int32) -> Int32 fn answer() -> Int32 { math.next(legacy(40)) }").unwrap();
    compiler.compile_module(&ast).unwrap();
    let (dir, _cleanup) = scratch_dir("custom-registry");
    let object = dir.join("custom.o");
    let host = dir.join("host.c");
    let executable = dir.join("custom");
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdint.h>
        int32_t native_increment(int32_t x) { return x + 1; }
        int32_t export_legacy(int32_t x) { return x + 1; }
        extern int32_t export_answer(void);
        int main(void) { return export_answer() == 42 ? 0 : 1; }
    "#,
    )
    .unwrap();
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror"])
            .arg(&host)
            .arg(&object),
    )
    .arg("-o")
    .arg(&executable)
    .output()
    .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    assert!(Command::new(&executable).status().unwrap().success());
}

#[test]
fn registry_symbols_cannot_alias_source_exports() {
    use crate::{
        core::resolver::BindingSpec,
        modules::{ModuleRegistry, NativeBinding},
    };
    use std::rc::Rc;

    let mut registry = ModuleRegistry::default();
    unsafe {
        registry.register_module(
            "user/math",
            [NativeBinding {
                spec: BindingSpec::with_symbol(
                    "math.next",
                    FunctionSignature::new(vec![], Type::Int32),
                    "export_answer",
                ),
                entry: None,
                owner: None,
            }],
        )
    }
    .unwrap();
    let mut compiler =
        AotCompiler::with_symbol_prefix_and_registry("collision", "export_", Rc::new(registry))
            .unwrap();
    let source = "import user/math fn answer() -> Int32 { math.next() }";
    let error = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_err();
    assert_eq!(
        (error.phase(), error.code()),
        (ErrorPhase::Abi, ErrorCode::AbiError)
    );
    assert_eq!(
        error.span().unwrap().slice(source),
        Some("import user/math")
    );
}

#[test]
#[cfg(unix)]
fn jit_calls_generated_aot_with_shared_runtime_and_keeps_code_alive() {
    use crate::runtime::{
        cps,
        scheduler::{Scheduler, SchedulerError},
        suspension::{ContinuationStatus, ResumeError},
    };
    use crate::vm::VmEngine;
    use libloading::Library;
    use std::{cell::Cell, rc::Rc};

    thread_local! {
        static READY: Cell<bool> = const { Cell::new(false) };
        static STARTS: Cell<u32> = const { Cell::new(0) };
        static FAIL: Cell<bool> = const { Cell::new(false) };
    }
    unsafe extern "C" fn wait_value(frame: *mut cps::CpsFrame) -> i32 {
        let frame = &mut *frame;
        if frame.state == 0 {
            STARTS.with(|v| v.set(v.get() + 1));
            frame.state = 1;
            return 1;
        }
        if !READY.with(Cell::get) {
            return 1;
        }
        if FAIL.with(Cell::get) {
            return 7;
        }
        frame.result = (frame.slots[0] as i32 + 1) as u32 as u64;
        0
    }
    let ast = crate::parser::parse(
        r#"
        extern fn wait_value(x: Int32) @suspends -> Int32
        fn inner(x: Int32) @suspends -> Int32 {
            let saved = x + 10
            saved + wait_value(x)
        }
        fn aot_work(x: Int32) @suspends -> Int32 {
            let saved = inner(x)
            saved + wait_value(20)
        }
        fn aot_divide(a: Int32, b: Int32) -> Int32 { a / b }
        fn aot_division(x: Int32) @suspends -> Int32 {
            let divisor = wait_value(x)
            return aot_divide(7, divisor) + wait_value(99)
        }
        fn aot_failure(x: Int32) @suspends { 7 / wait_value(x); }
        fn aot_division_scope(x: Int32) @suspends -> Int32 {
            scope { | aot_failure(x) | loop_branch(99) }
            wait_value(100)
        }
        fn aot_expression(x: Int32) @suspends -> Int32 {
            var active = (x < 0) == !(wait_value(x) > 0)
            var result = -x
            while active { result = result + -wait_value(2) * 2 active = false }
            result
        }
        fn aot_conditional(x: Int32) @suspends -> Int32 {
            let skipped = false && wait_value(99) > 0
            let selected = if x == 99 || wait_value(x) > 0 {
                wait_value(10)
            } else { wait_value(20) }
            selected + wait_value(30)
        }
        fn loop_branch(x: Int32) @suspends { wait_value(x) {} }
        fn aot_jumps(n: Int32) @suspends -> Int32 {
            var i = 0 var total = 0
            while wait_value(i) <= n {
                i = i + 1
                if i == 2 { continue }
                if i == 4 { break }
                scope { | loop_branch(i) | loop_branch(i + 10) }
                total = total + wait_value(i)
            }
            return total + wait_value(40)
        }
        fn aot_loop(n: Int32) @suspends -> Int32 {
            var i = 0
            var total = 0
            var checks = 0;
            while (checks = checks + 1; wait_value(i) <= n) {
                scope { | loop_branch(i) | loop_branch(i + 10) }
                total = total + wait_value({ i = i + 1 i })
            }
            total + (checks - (n + 1))
        }
    "#,
    )
    .unwrap();
    let (dir, _cleanup) = scratch_dir("jit");
    let object = dir.join("aot.o");
    let bridge = dir.join("bridge.c");
    let library_path = dir.join(if cfg!(target_os = "macos") {
        "aot.dylib"
    } else {
        "aot.so"
    });
    let mut compiler = AotCompiler::new("aot_callbacks").unwrap();
    compiler.compile_module(&ast).unwrap();
    let slots = compiler.cps_frame_slots("aot_work").unwrap();
    let loop_slots = compiler.cps_frame_slots("aot_loop").unwrap();
    let expression_slots = compiler.cps_frame_slots("aot_expression").unwrap();
    let conditional_slots = compiler.cps_frame_slots("aot_conditional").unwrap();
    let jump_slots = compiler.cps_frame_slots("aot_jumps").unwrap();
    let division_slots = compiler.cps_frame_slots("aot_division").unwrap();
    let division_scope_slots = compiler.cps_frame_slots("aot_division_scope").unwrap();
    assert!(slots > 1); // Parameters alone cannot hold AOT temporaries.
    compiler.write_object(&object).unwrap();
    // Forward helper calls to the SAME Rust runtime that owns the JIT frames.
    // This avoids a second runtime or a dependency on executable symbol export.
    std::fs::write(
        &bridge,
        r#"
        #include "joky_cps.h"
        #include "joky_runtime.h"
        static int32_t (*error_code_fn)(void);
        static void (*division_fn)(size_t, size_t);
        void bind_error_runtime(int32_t (*e)(void), void (*d)(size_t, size_t)) {
            error_code_fn = e; division_fn = d;
        }
        int32_t joky_runtime_error_code(void) { return error_code_fn(); }
        void __joky_runtime_division_by_zero(size_t start, size_t end) { division_fn(start, end); }
        static uint64_t *(*slots_fn)(JokyCpsFrame *);
        static JokyCpsFrame *(*child_fn)(JokyCpsFrame *, size_t);
        static void (*release_fn)(JokyCpsFrame *);
        static JokyCpsFrame *(*child_at_fn)(JokyCpsFrame *, size_t, size_t);
        static void (*release_children_fn)(JokyCpsFrame *);
        static JokyCpsEntry wait_fn;
        void bind_runtime(uint64_t *(*s)(JokyCpsFrame *),
                          JokyCpsFrame *(*c)(JokyCpsFrame *, size_t),
                          void (*r)(JokyCpsFrame *), JokyCpsEntry w) {
            slots_fn = s; child_fn = c; release_fn = r; wait_fn = w;
        }
        uint64_t *__joky_cps_slots(JokyCpsFrame *f) { return slots_fn(f); }
        JokyCpsFrame *__joky_cps_child(JokyCpsFrame *f, size_t n) { return child_fn(f, n); }
        void __joky_cps_release_child(JokyCpsFrame *f) { release_fn(f); }
        int32_t wait_value(JokyCpsFrame *f) { return wait_fn(f); }
        void bind_scope_runtime(JokyCpsFrame *(*c)(JokyCpsFrame *, size_t, size_t),
                                void (*r)(JokyCpsFrame *)) {
            child_at_fn = c; release_children_fn = r;
        }
        JokyCpsFrame *__joky_cps_child_at(JokyCpsFrame *f, size_t i, size_t n) {
            return child_at_fn(f, i, n);
        }
        void __joky_cps_release_children(JokyCpsFrame *f) { release_children_fn(f); }
    "#,
    )
    .unwrap();
    let link = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .args(["-fPIC", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
        .arg(&bridge)
        .arg(&object)
        .arg("-o")
        .arg(&library_path)
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    type Bind = unsafe extern "C" fn(
        unsafe extern "C" fn(*mut cps::CpsFrame) -> *mut u64,
        unsafe extern "C" fn(*mut cps::CpsFrame, usize) -> *mut cps::CpsFrame,
        unsafe extern "C" fn(*mut cps::CpsFrame),
        cps::CpsEntry,
    );
    let library = Rc::new(unsafe { Library::new(&library_path).unwrap() });
    type BindScope = unsafe extern "C" fn(
        unsafe extern "C" fn(*mut cps::CpsFrame, usize, usize) -> *mut cps::CpsFrame,
        unsafe extern "C" fn(*mut cps::CpsFrame),
    );
    unsafe {
        type BindError = unsafe extern "C" fn(extern "C" fn() -> i32, extern "C" fn(usize, usize));
        library.get::<BindError>(b"bind_error_runtime").unwrap()(
            crate::runtime::error::joky_runtime_error_code,
            crate::runtime::error::division_by_zero,
        );
        library.get::<Bind>(b"bind_runtime").unwrap()(
            cps::slots,
            cps::child,
            cps::release_child,
            wait_value,
        );
        library.get::<BindScope>(b"bind_scope_runtime").unwrap()(
            cps::child_at,
            cps::release_children,
        );
    }
    let entry = unsafe { *library.get::<cps::CpsEntry>(b"aot_work").unwrap() };
    let weak = Rc::downgrade(&library);
    let mut vm = VmEngine::new();
    unsafe {
        for (name, slots) in [
            ("aot_division", division_slots),
            ("aot_division_scope", division_scope_slots),
        ] {
            vm.register_aot_suspending_fn(
                name,
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                *library.get::<cps::CpsEntry>(name.as_bytes()).unwrap(),
                slots,
                library.clone(),
            )
            .unwrap();
        }
        vm.register_aot_suspending_fn(
            "aot_work",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            entry,
            slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_loop",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_loop").unwrap(),
            loop_slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_expression",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_expression").unwrap(),
            expression_slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_conditional",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_conditional").unwrap(),
            conditional_slots,
            library.clone(),
        )
        .unwrap();
        vm.register_aot_suspending_fn(
            "aot_jumps",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            *library.get::<cps::CpsEntry>(b"aot_jumps").unwrap(),
            jump_slots,
            library.clone(),
        )
        .unwrap();
    }
    vm.load_source(
        r#"
        extern fn aot_work(x: Int32) @suspends -> Int32
        extern fn aot_loop(n: Int32) @suspends -> Int32
        extern fn aot_expression(x: Int32) @suspends -> Int32
        extern fn aot_conditional(x: Int32) @suspends -> Int32
        extern fn aot_jumps(n: Int32) @suspends -> Int32
        extern fn aot_division(x: Int32) @suspends -> Int32
        extern fn aot_division_scope(x: Int32) @suspends -> Int32
        fn compute(x: Int32) @suspends -> Int32 { 100 + aot_work(x) }
        fn compute_loop(n: Int32) @suspends -> Int32 { aot_loop(n) }
        fn compute_expression(x: Int32) @suspends -> Int32 { aot_expression(x) }
        fn compute_conditional(x: Int32) @suspends -> Int32 { aot_conditional(x) }
        fn compute_jumps(n: Int32) @suspends -> Int32 { return aot_jumps(n) }
        fn compute_division(x: Int32) @suspends -> Int32 { return aot_division(x) }
        fn compute_division_scope(x: Int32) @suspends -> Int32 { return aot_division_scope(x) }
    "#,
    )
    .unwrap();
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("compute", &[2]).unwrap());
    scheduler.resume(id).unwrap(); // Still waiting: no repeated side effects.
    assert_eq!(STARTS.with(Cell::get), 1);
    READY.with(|v| v.set(true));
    scheduler.resume(id).unwrap();
    assert_eq!(scheduler.suspended_count(), 1);
    scheduler.resume(id).unwrap();
    let task = scheduler.take_completed(id).unwrap();
    assert_eq!(task.cps_result(), Some(136));
    assert_eq!(STARTS.with(Cell::get), 2);
    drop(task);

    READY.with(|v| v.set(false));
    let starts = STARTS.with(Cell::get);
    let mut task = vm.call_async("compute_loop", &[3]).unwrap();
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(STARTS.with(Cell::get), starts + 1);
    READY.with(|v| v.set(true));
    for _ in 0..20 {
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert!(task.is_finished());
    assert_eq!(task.cps_result(), Some(9));
    // Four condition calls, six scope branches, and three body calls.
    assert_eq!(STARTS.with(Cell::get), starts + 13);
    drop(task);

    READY.with(|v| v.set(false));
    let starts = STARTS.with(Cell::get);
    let mut task = vm.call_async("compute_expression", &[4]).unwrap();
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(STARTS.with(Cell::get), starts + 1);
    READY.with(|v| v.set(true));
    task.resume().unwrap();
    assert!(!task.is_finished());
    READY.with(|v| v.set(false));
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(STARTS.with(Cell::get), starts + 2);
    READY.with(|v| v.set(true));
    task.resume().unwrap();
    assert_eq!(task.cps_result().unwrap() as i32, -10);
    assert_eq!(STARTS.with(Cell::get), starts + 2);
    drop(task);

    for (input, expected, calls) in [(-2, 52, 3), (1, 42, 3), (99, 42, 2)] {
        READY.with(|v| v.set(false));
        let starts = STARTS.with(Cell::get);
        let mut task = vm.call_async("compute_conditional", &[input]).unwrap();
        for call in 1..=calls {
            assert!(!task.is_finished());
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert_eq!(STARTS.with(Cell::get), starts + call);
            READY.with(|v| v.set(true));
            task.resume().unwrap();
            READY.with(|v| v.set(false));
        }
        assert_eq!(task.cps_result(), Some(expected));
        assert_eq!(STARTS.with(Cell::get), starts + calls);
    }
    for (input, expected, calls) in [(0, 41, 2), (1, 43, 6), (3, 47, 11), (10, 47, 11)] {
        READY.with(|v| v.set(false));
        let starts = STARTS.with(Cell::get);
        let mut task = vm.call_async("compute_jumps", &[input]).unwrap();
        for _ in 0..20 {
            if task.is_finished() {
                break;
            }
            let pending_starts = STARTS.with(Cell::get);
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert_eq!(STARTS.with(Cell::get), pending_starts);
            READY.with(|v| v.set(true));
            task.resume().unwrap();
            READY.with(|v| v.set(false));
        }
        assert_eq!(task.cps_result(), Some(expected));
        assert_eq!(STARTS.with(Cell::get), starts + calls);
    }
    READY.with(|v| v.set(true));

    // Fail while computing an explicit return, after leaving the loop.
    let mut failed_return = vm.call_async("compute_jumps", &[0]).unwrap();
    failed_return.resume().unwrap();
    assert!(!failed_return.is_finished());
    FAIL.with(|v| v.set(true));
    assert_eq!(
        failed_return.resume(),
        Err(ResumeError::InvalidCpsStatus(7))
    );
    assert_eq!(failed_return.cps_result(), None);
    drop(failed_return);
    FAIL.with(|v| v.set(false));

    let failed_id = scheduler.suspend(vm.call_async("compute_conditional", &[1]).unwrap());
    FAIL.with(|v| v.set(true));
    assert_eq!(
        scheduler.resume(failed_id),
        Err(SchedulerError::Resume(ResumeError::InvalidCpsStatus(7)))
    );
    assert_eq!(
        scheduler.take_completed(failed_id).unwrap_err(),
        SchedulerError::Resume(ResumeError::InvalidCpsStatus(7))
    );
    let failed = scheduler.take_failed(failed_id).unwrap();
    assert_eq!(
        failed.status(),
        ContinuationStatus::Failed(ResumeError::InvalidCpsStatus(7))
    );
    assert_eq!(failed.cps_result(), None);
    drop(failed);
    FAIL.with(|v| v.set(false));
    for name in ["compute_division", "compute_division_scope"] {
        READY.with(|v| v.set(false));
        let starts = STARTS.with(Cell::get);
        let mut task = vm.call_async(name, &[-1]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        let calls = if name.ends_with("scope") { 2 } else { 1 };
        assert_eq!(STARTS.with(Cell::get), starts + calls);
        READY.with(|v| v.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected structured AOT failure")
        };
        assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
        assert!(error.span().is_some());
        assert_eq!(error.message(), "Division by zero");
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(STARTS.with(Cell::get), starts + calls);
    }
    // The interpreter must also preserve failures from generated AOT entries.
    for name in ["aot_division", "aot_division_scope"] {
        let mut interpreted = VmEngine::new_interpreter();
        unsafe {
            interpreted
                .register_aot_suspending_fn(
                    name,
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    *library.get::<cps::CpsEntry>(name.as_bytes()).unwrap(),
                    if name.ends_with("scope") {
                        division_scope_slots
                    } else {
                        division_slots
                    },
                    library.clone(),
                )
                .unwrap();
        }
        interpreted.load_source(&format!("extern fn {name}(x: Int32) @suspends -> Int32 fn run(x: Int32) @suspends -> Int32 {{ {name}(x) }}")).unwrap();
        READY.with(|v| v.set(false));
        let mut task = interpreted.call_async("run", &[-1]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        READY.with(|v| v.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected structured AOT failure")
        };
        assert_eq!(error.message(), "Division by zero");
        assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
    }
    let cancelled_id = scheduler.suspend(vm.call_async("compute_conditional", &[3]).unwrap());
    let survivor = scheduler.suspend(vm.call_async("compute_conditional", &[4]).unwrap());
    let cancelled_jump = scheduler.suspend(vm.call_async("compute_jumps", &[0]).unwrap());
    scheduler.resume(cancelled_jump).unwrap(); // Pending inside the return expression.
    let surviving_jump = scheduler.suspend(vm.call_async("compute_jumps", &[10]).unwrap());
    READY.with(|v| v.set(false));
    let cancelled_division = scheduler.suspend(vm.call_async("compute_division", &[-1]).unwrap());
    let failed_division = scheduler.suspend(vm.call_async("compute_division", &[-1]).unwrap());
    vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
    drop(vm);
    drop(library);
    assert!(weak.upgrade().is_some());
    scheduler.cancel(cancelled_id).unwrap();
    scheduler.cancel(cancelled_jump).unwrap();
    scheduler.cancel(cancelled_division).unwrap();
    READY.with(|v| v.set(true));
    let SchedulerError::Resume(ResumeError::Failed(error)) =
        scheduler.resume(failed_division).unwrap_err()
    else {
        panic!("expected division failure after reload")
    };
    assert_eq!(error.message(), "Division by zero");
    drop(scheduler.take_failed(failed_division).unwrap());
    scheduler.resume(survivor).unwrap();
    scheduler.resume(survivor).unwrap();
    scheduler.resume(survivor).unwrap();
    let task = scheduler.take_completed(survivor).unwrap();
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    for _ in 0..12 {
        if scheduler
            .continuation(surviving_jump)
            .unwrap()
            .is_finished()
        {
            break;
        }
        scheduler.resume(surviving_jump).unwrap();
    }
    let task = scheduler.take_completed(surviving_jump).unwrap();
    assert_eq!(task.cps_result(), Some(47));
    drop(task);
    assert!(
        weak.upgrade().is_none(),
        "AOT library must unload after its last task"
    );
}

#[test]
#[cfg(unix)]
fn records_execute_in_native_objects_and_cps_with_the_shared_runtime() {
    let source = include_str!("../../../tests/fixtures/records.jk");
    for suspends in [false, true] {
        let mut source = source
            .lines()
            .map(|line| {
                if suspends && line.starts_with("fn ") {
                    line.replacen(')', ") @suspends", 1)
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        source.push_str(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            fn direct_record(x: Int32) -> Point { Point { x: x y: x + 1 } }
            fn cps_record(p: Point) @suspends -> Point {
                Point { y: delayed_value(record(p.y)) x: p.x }
            }
            fn pending() @suspends -> Int32 {
                let p = cps_record(direct_record(10))
                let q = direct_record(p.y)
                p.x * 100 + q.y
            }
            fn bad_record(n: Int32) -> Point { Point { x: 1 / n y: record(99) } }
            fn failing() @suspends -> Int32 {
                let p = cps_record(direct_record(-2))
                bad_record(p.y).x
            }
        "#,
        );
        let (dir, _cleanup) = scratch_dir("records");
        let object = dir.join("records.o");
        let program = dir.join("records");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("records").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let pending_slots = compiler.cps_frame_slots("pending").unwrap();
        let failing_slots = compiler.cps_frame_slots("failing").unwrap();
        let compute_slots = compiler.cps_frame_slots("compute").unwrap_or(0);
        compiler.write_object(&object).unwrap();
        let compute = if suspends {
            format!(
                r#"
            JokyCpsFrame *compute_frame = joky_cps_frame_new({compute_slots});
            if (compute(compute_frame) != 0 || joky_cps_result(compute_frame) != 87915) return 1;
            joky_cps_frame_free(compute_frame);
        "#
            )
        } else {
            "if (compute() != 87915) return 1;".to_owned()
        };
        let harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            int32_t record(int32_t x) { events++; return x; }
            int32_t delayed_value(JokyCpsFrame *frame) {
                if (joky_cps_state(frame) == 0) {
                    events++;
                    joky_cps_set_state(frame, 1);
                    return 1;
                }
                if (!ready) return 1;
                joky_cps_set_result(frame, (uint32_t)((int32_t)__joky_cps_slots(frame)[0] + 1));
                return 0;
            }
            extern int32_t compute(COMPUTE_ARGS);
            extern int32_t pending(JokyCpsFrame *), failing(JokyCpsFrame *);
            int main(void) {
                COMPUTE_BODY
                JokyCpsFrame *p = joky_cps_frame_new(PENDING_SLOTS);
                JokyCpsFrame *f = joky_cps_frame_new(FAILING_SLOTS);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || failing(f) != 1 || pending(cancelled) != 1) return 2;
                }
                if (events != 6) return 3;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 0 || joky_cps_result(p) != 1013) return 4;
                if (failing(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 5;
                if (joky_cps_state(f) != JOKY_CPS_FAILED || events != 6) return 6;
                joky_cps_frame_free(p);
                joky_cps_frame_free(f);
                return 0;
            }
        "#
        .replace(
            "COMPUTE_ARGS",
            if suspends { "JokyCpsFrame *" } else { "void" },
        )
        .replace("COMPUTE_BODY", &compute)
        .replace("PENDING_SLOTS", &pending_slots.to_string())
        .replace("FAILING_SLOTS", &failing_slots.to_string());
        std::fs::write(&host, harness).unwrap();
        let output = link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .arg("-I")
                .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
        )
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}
