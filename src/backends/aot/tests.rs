use super::*;
use crate::parser::test_support::{with_suspends, Suspends};
use crate::{FunctionSignature, Type};
use std::{
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Cleanup(std::path::PathBuf);

#[cfg(unix)]
fn link_runtime(command: &mut Command) -> &mut Command {
    // All links in this test process use the same Cargo runtime artifact.
    static ARCHIVE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let archive = ARCHIVE.get_or_init(|| {
        let deps = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        std::fs::read_dir(deps)
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
            .expect("Cargo must build the runtime archive")
    });
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
fn string_queries_link_and_run_in_aot_objects() {
    let source = r#"
        import joky/string
        fn compute() -> Int32 {
            let text = "héllo"
            string.byte_length(text) * 1000000
                + string.find(text, "llo") * 1000
                + string.starts_with(text, "hé") * 100
                + string.ends_with(text, "lo") * 10
                + string.equals(text, "héllo")
        }
    "#;
    let (dir, _cleanup) = scratch_dir("string-queries");
    let object = dir.join("string-queries.o");
    let host = dir.join("host.c");
    let executable = dir.join("string-queries");
    let mut compiler = AotCompiler::new("string_queries").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include <stdint.h>
        extern int32_t compute(void);
        int main(void) { return compute() == 6003111 ? 0 : 1; }
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
        fn aot_shift(x: Int32) @suspends -> Int32 { 1 << wait_value(x) | wait_value(99) }
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
    let shift_slots = compiler.cps_frame_slots("aot_shift").unwrap();
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
        static void (*shift_fn)(size_t, size_t);
        void bind_shift_runtime(void (*s)(size_t, size_t)) { shift_fn = s; }
        void __joky_runtime_invalid_shift(size_t start, size_t end) { shift_fn(start, end); }
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
        static void (*finish_fn)(JokyCpsFrame *);
        void bind_finish_runtime(void (*f)(JokyCpsFrame *)) { finish_fn = f; }
        void __joky_cps_finish(JokyCpsFrame *f) { finish_fn(f); }
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
        type BindShift = unsafe extern "C" fn(extern "C" fn(usize, usize));
        type BindFinish = unsafe extern "C" fn(unsafe extern "C" fn(*mut cps::CpsFrame));
        library.get::<BindFinish>(b"bind_finish_runtime").unwrap()(cps::finish);
        library.get::<BindShift>(b"bind_shift_runtime").unwrap()(
            crate::runtime::error::invalid_shift,
        );
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
    for mode in [crate::vm::VmMode::Jit, crate::vm::VmMode::Interpreter] {
        let mut shifted = VmEngine::with_mode(mode);
        unsafe {
            shifted
                .register_aot_suspending_fn(
                    "aot_shift",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    *library.get::<cps::CpsEntry>(b"aot_shift").unwrap(),
                    shift_slots,
                    library.clone(),
                )
                .unwrap();
        }
        shifted.load_source("extern fn aot_shift(n: Int32) @suspends -> Int32 fn run(n: Int32) @suspends -> Int32 { aot_shift(n) }").unwrap();
        for count in [-2, 31] {
            READY.with(|v| v.set(false));
            let starts = STARTS.with(Cell::get);
            let mut task = shifted.call_async("run", &[count]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert_eq!(STARTS.with(Cell::get), starts + 1);
            READY.with(|v| v.set(true));
            let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                panic!("expected shared shift failure")
            };
            assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
            assert!(error.message().contains("Shift count"));
            assert_eq!(task.cps_result(), None);
            assert_eq!(STARTS.with(Cell::get), starts + 1);
        }
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
        let mut source = with_suspends(source, suspends, Suspends::All);
        source.push_str(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            fn direct_record(x: Int32) -> Point { Point { x: x y: x + 1 } }
            struct Deferred<T> { value: T stamp: Int32 }
            fn defer<T>(value: T, n: Int32) @suspends -> Deferred<T> {
                Deferred { stamp: delayed_value(record(n)) value: value }
            }
            fn cps_record(p: Point) @suspends -> Point {
                let saved = defer(p, p.y)
                Point { y: saved.stamp x: saved.value.x }
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

#[test]
#[cfg(unix)]
fn enums_execute_in_native_objects_and_cps_with_the_shared_runtime() {
    let source = include_str!("../../../tests/fixtures/enums.jk");
    for suspends in [false, true] {
        let mut source = with_suspends(source, suspends, Suspends::All);
        source.push_str(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            enum AsyncValue { Empty Value(Int32) }
            fn direct_enum(x: Int32) -> AsyncValue { AsyncValue.Value(x) }
            fn read_enum(e: AsyncValue) -> Int32 { match e { | Value(x) => x | Empty => 0 } }
            fn cps_enum(e: AsyncValue) @suspends -> AsyncValue {
                match e {
                    | Value(x) => AsyncValue.Value(delayed_value(record(x)))
                    | Empty => AsyncValue.Empty
                }
            }
            fn pending() @suspends -> Int32 {
                let e = cps_enum(direct_enum(11))
                match e { | Value(x) => delayed_value(record(x)) | Empty => record(99) }
            }
            fn bad_enum(n: Int32) -> AsyncValue { AsyncValue.Value(1 / n; record(99)) }
            fn failing() @suspends -> Int32 {
                let e = cps_enum(direct_enum(-1))
                read_enum(bad_enum(read_enum(e)))
            }
        "#,
        );
        let (dir, _cleanup) = scratch_dir("enums");
        let object = dir.join("enums.o");
        let program = dir.join("enums");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("enums").unwrap();
        compiler.set_prelude_enabled(false).unwrap();
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
            if (compute(compute_frame) != 0 || joky_cps_result(compute_frame) != 2498) return 1;
            joky_cps_frame_free(compute_frame);
        "#
            )
        } else {
            "if (compute() != 2498) return 1;".to_owned()
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
                if (pending(p) != 1 || events != 8) return 4;
                if (pending(p) != 0 || joky_cps_result(p) != 13) return 7;
                if (failing(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 5;
                if (joky_cps_state(f) != JOKY_CPS_FAILED || events != 8) return 6;
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

#[test]
#[cfg(unix)]
fn tuples_execute_in_native_objects_and_cps_with_the_shared_runtime() {
    let source = include_str!("../../../tests/fixtures/tuples.jk");
    for suspends in [false, true] {
        let mut source = with_suspends(source, suspends, Suspends::Typed);
        source.push_str(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            fn direct_tuple(x: Int32) -> (Int32, Bool) { (x, true) }
            fn cps_tuple(pair: (Int32, Bool)) @suspends -> (Int32, Bool) {
                let (x, flag) = pair;
                (delayed_value(record(x)), flag)
            }
            fn pending() @suspends -> Int32 {
                let (x, flag) = cps_tuple(direct_tuple(11))
                if flag { delayed_value(record(x)) } else { record(99) }
            }
            fn bad_tuple(n: Int32) -> (Int32, Bool) { (1 / n; record(99), true) }
            fn failing() @suspends -> Int32 {
                let (x, _) = cps_tuple(direct_tuple(-1))
                let (y, _) = bad_tuple(x)
                y
            }
        "#,
        );
        let (dir, _cleanup) = scratch_dir("tuples");
        let object = dir.join("tuples.o");
        let program = dir.join("tuples");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("tuples").unwrap();
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
            if (compute(compute_frame) != 0 || joky_cps_result(compute_frame) != 22813) return 1;
            joky_cps_frame_free(compute_frame);
        "#
            )
        } else {
            "if (compute() != 22813) return 1;".to_owned()
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
                if (pending(p) != 1 || events != 8) return 4;
                if (pending(p) != 0 || joky_cps_result(p) != 13) return 7;
                if (failing(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code()) return 5;
                if (joky_cps_state(f) != JOKY_CPS_FAILED || events != 8) return 6;
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

#[test]
#[cfg(unix)]
fn int64_native_objects_preserve_direct_cps_and_host_full_width_values() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../tests/fixtures/int64.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(
            r#"
            extern fn delayed64(value: Int64) @suspends -> Int64
            extern fn record64(value: Int64) -> Int64
            fn direct64(value: Int64) -> Int64 { value + 1 }
            fn pending64(value: Int64) @suspends -> Int64 { direct64(delayed64(record64(value))) }
            fn failed64() @suspends -> Int64 { let n = delayed64(-1) 3000000000 / n }
        "#,
        );
        let (dir, _cleanup) = scratch_dir("int64");
        let object = dir.join("int64.o");
        let program = dir.join("int64");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("int64").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let pending_slots = compiler.cps_frame_slots("pending64").unwrap();
        let failed_slots = compiler.cps_frame_slots("failed64").unwrap();
        let check = if suspends {
            let check_slots = compiler.cps_frame_slots("check").unwrap();
            let min_slots = compiler.cps_frame_slots("min").unwrap();
            format!("JokyCpsFrame *c = joky_cps_frame_new({check_slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c); c = joky_cps_frame_new({min_slots}); if (min(c) != 0 || joky_cps_result(c) != (UINT64_C(1) << 63)) return 2; joky_cps_frame_free(c);")
        } else {
            "if (check() != 42 || min() != INT64_MIN || divide(INT64_MIN, -1) != INT64_MIN || add(INT64_MAX, 1) != INT64_MIN || negate(INT64_MIN) != INT64_MIN) return 1;".to_owned()
        };
        compiler.write_object(&object).unwrap();
        let declarations = if suspends {
            "extern int32_t check(JokyCpsFrame *), min(JokyCpsFrame *);"
        } else {
            "extern int32_t check(void); extern int64_t min(void), divide(int64_t, int64_t), add(int64_t, int64_t), negate(int64_t);"
        };
        let harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            int64_t record64(int64_t value) { events++; return value; }
            int32_t delayed64(JokyCpsFrame *frame) {
                if (!joky_cps_state(frame)) { events++; joky_cps_set_state(frame, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(frame, __joky_cps_slots(frame)[0] + UINT64_C(1)); return 0;
            }
            DECLARATIONS
            extern int32_t pending64(JokyCpsFrame *), failed64(JokyCpsFrame *);
            int main(void) {
                CHECK
                JokyCpsFrame *p = joky_cps_frame_new(PENDING_SLOTS);
                JokyCpsFrame *f = joky_cps_frame_new(FAILED_SLOTS);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING_SLOTS);
                __joky_cps_slots(p)[0] = (uint64_t)-INT64_C(4000000000);
                __joky_cps_slots(cancelled)[0] = INT64_C(3000000000);
                for (int i = 0; i < 3; i++) { if (pending64(p) != 1 || failed64(f) != 1 || pending64(cancelled) != 1) return 3; }
                if (events != 5) return 4;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending64(p) != 0 || joky_cps_result(p) != (uint64_t)-INT64_C(3999999998)) return 5;
                if (failed64(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED || events != 5) return 6;
                joky_cps_frame_free(p); joky_cps_frame_free(f);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check).replace("PENDING_SLOTS", &pending_slots.to_string()).replace("FAILED_SLOTS", &failed_slots.to_string());
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

#[test]
#[cfg(unix)]
fn unsigned_native_objects_preserve_arithmetic_abi_and_pending_values() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../tests/fixtures/unsigned.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn host32(value: UInt32) -> UInt32
            extern fn host64(value: UInt64) -> UInt64
            extern fn delay32(unit: Unit, value: UInt32, end: Unit) @suspends -> UInt32
            extern fn delay64(unit: Unit, value: UInt64, end: Unit) @suspends -> UInt64
            fn native32(n: UInt32) -> UInt32 { host32(n) / 2 }
            fn native64(n: UInt64) -> UInt64 { host64(n) / 2 }
            fn pending32(n: UInt32) @suspends -> UInt32 { host32(delay32({}, host32(n), {})) }
            fn pending64(n: UInt64) @suspends -> UInt64 { host64(delay64({}, host64(n), {})) }
            fn fail32() @suspends -> UInt32 { let d = delay32({}, 4294967295, {}) 4294967295 / d }
            fn fail64() @suspends -> UInt64 { let d = delay64({}, 18446744073709551615, {}) 18446744073709551615 / d }
        "#);
        let (dir, _cleanup) = scratch_dir("unsigned");
        let object = dir.join("unsigned.o");
        let program = dir.join("unsigned");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("unsigned").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let check = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!("JokyCpsFrame *check_frame = joky_cps_frame_new({slots}); if (check(check_frame) != 0 || joky_cps_result(check_frame) != 42) return 1; joky_cps_frame_free(check_frame);")
        } else {
            "if (check() != 42 || max32() != UINT32_MAX || max64() != UINT64_MAX || div32(UINT32_MAX, UINT32_C(2147483648)) != 1 || div64(UINT64_MAX, UINT64_C(9223372036854775808)) != 1) return 1;".to_owned()
        };
        let declarations = if suspends {
            "extern int32_t check(JokyCpsFrame *);"
        } else {
            "extern int32_t check(void); extern uint32_t max32(void), div32(uint32_t, uint32_t); extern uint64_t max64(void), div64(uint64_t, uint64_t);"
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint32_t host32(uint32_t n) { events++; return n; }
            uint64_t host64(uint64_t n) { events++; return n; }
            static int32_t delayed(JokyCpsFrame *f, int narrow) {
                uint64_t *args = __joky_cps_slots(f);
                if (args[0] != 0 || args[2] != 0) return -1;
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, narrow ? (uint32_t)(args[1] + UINT64_C(1)) : args[1] + UINT64_C(1));
                return 0;
            }
            int32_t delay32(JokyCpsFrame *f) { return delayed(f, 1); }
            int32_t delay64(JokyCpsFrame *f) { return delayed(f, 0); }
            DECLARATIONS
            extern uint32_t native32(uint32_t);
            extern uint64_t native64(uint64_t);
            extern int32_t pending32(JokyCpsFrame *), pending64(JokyCpsFrame *), fail32(JokyCpsFrame *), fail64(JokyCpsFrame *);
            int main(void) {
                CHECK
                if (native32(UINT32_MAX) != UINT32_C(2147483647) || native64(UINT64_MAX) != UINT64_C(9223372036854775807)) return 2;
                events = 0;
                JokyCpsFrame *p32 = joky_cps_frame_new(PENDING32);
                JokyCpsFrame *p64 = joky_cps_frame_new(PENDING64);
                JokyCpsFrame *f32 = joky_cps_frame_new(FAILED32);
                JokyCpsFrame *f64 = joky_cps_frame_new(FAILED64);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING64);
                __joky_cps_slots(p32)[0] = UINT32_C(2147483648);
                __joky_cps_slots(p64)[0] = UINT64_C(9223372036854775808);
                __joky_cps_slots(cancelled)[0] = UINT64_MAX;
                for (int i = 0; i < 3; i++) {
                    if (pending32(p32) != 1 || pending64(p64) != 1 || fail32(f32) != 1 || fail64(f64) != 1 || pending64(cancelled) != 1) return 3;
                }
                if (events != 8) return 4;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending32(p32) != 0 || joky_cps_result(p32) != UINT32_C(2147483649)) return 5;
                if (pending64(p64) != 0 || joky_cps_result(p64) != UINT64_C(9223372036854775809)) return 6;
                if (events != 10) return 7;
                if (fail32(f32) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f32) != JOKY_CPS_FAILED) return 8;
                joky_runtime_error_clear();
                if (fail64(f64) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f64) != JOKY_CPS_FAILED) return 9;
                joky_cps_frame_free(p32); joky_cps_frame_free(p64); joky_cps_frame_free(f32); joky_cps_frame_free(f64);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING32", "pending32"),
            ("PENDING64", "pending64"),
            ("FAILED32", "fail32"),
            ("FAILED64", "fail64"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        compiler.write_object(&object).unwrap();
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

#[test]
#[cfg(unix)]
fn bitwise_native_objects_check_shifts_and_preserve_pending_operands() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../tests/fixtures/bitwise.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn record(n: UInt64) -> UInt64
            extern fn delay(n: UInt64) @suspends -> UInt64
            fn shl32(a: Int32, n: Int32) -> Int32 { a << n }
            fn shr32(a: Int32, n: Int32) -> Int32 { a >> n }
            fn shl64(a: Int64, n: Int64) -> Int64 { a << n }
            fn shr64(a: Int64, n: Int64) -> Int64 { a >> n }
            fn shlu32(a: UInt32, n: UInt32) -> UInt32 { a << n }
            fn shru32(a: UInt32, n: UInt32) -> UInt32 { a >> n }
            fn shlu64(a: UInt64, n: UInt64) -> UInt64 { a << n }
            fn shru64(a: UInt64, n: UInt64) -> UInt64 { a >> n }
            fn pending(n: UInt64) @suspends -> UInt64 {
                var value = n
                let bits = value & delay({ value = 0 record(n) })
                ~bits ^ (value | 3 << 1)
            }
            fn failure(n: UInt64) @suspends -> UInt64 { shlu64(1, delay(n)) | record(99) }
            fn bad_branch(n: UInt64) @suspends { shru64(1, delay(n)); }
            fn other(n: UInt64) @suspends { record(delay(n)); }
            fn bad_scope() @suspends -> UInt64 { scope { | bad_branch(64 | 0) | other(1 | 2) } record(99) }
        "#);
        let (dir, _cleanup) = scratch_dir("bitwise");
        let object = dir.join("bitwise.o");
        let program = dir.join("bitwise");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("bitwise").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        assert!(compiler.requires_runtime());
        let check = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!("JokyCpsFrame *c = joky_cps_frame_new({slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c);")
        } else {
            "if (check() != 42) return 1;".to_owned()
        };
        let declarations = if suspends {
            "extern int32_t check(JokyCpsFrame *);"
        } else {
            "extern int32_t check(void);"
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint64_t record(uint64_t n) { events++; return n; }
            int32_t delay(JokyCpsFrame *f) {
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, __joky_cps_slots(f)[0]); return 0;
            }
            DECLARATIONS
            extern int32_t shl32(int32_t, int32_t), shr32(int32_t, int32_t);
            extern int64_t shl64(int64_t, int64_t), shr64(int64_t, int64_t);
            extern uint32_t shlu32(uint32_t, uint32_t), shru32(uint32_t, uint32_t);
            extern uint64_t shlu64(uint64_t, uint64_t), shru64(uint64_t, uint64_t);
            extern int32_t pending(JokyCpsFrame *), failure(JokyCpsFrame *), bad_scope(JokyCpsFrame *);
            #define INVALID(call) do { joky_runtime_error_clear(); (void)(call); if (!joky_runtime_error_code() || joky_runtime_error_end() <= joky_runtime_error_start()) return 3; } while (0)
            int main(void) {
                CHECK
                if (shl32(1, 31) != INT32_MIN || shr32(INT32_MIN, 31) != -1 || shl64(1, 63) != INT64_MIN || shr64(INT64_MIN, 63) != -1) return 2;
                if (shlu32(1, 31) != UINT32_C(2147483648) || shru32(UINT32_MAX, 31) != 1 || shlu64(1, 63) != (UINT64_C(1) << 63) || shru64(UINT64_MAX, 63) != 1) return 2;
                INVALID(shl32(1, -1)); INVALID(shr32(1, 32)); INVALID(shl64(1, 64)); INVALID(shr64(1, INT64_MIN));
                INVALID(shlu32(1, UINT32_MAX)); INVALID(shru32(1, 32)); INVALID(shlu64(1, UINT64_C(1) << 32)); INVALID(shru64(1, UINT64_MAX));
                joky_runtime_error_clear();
                JokyCpsFrame *p = joky_cps_frame_new(PENDING);
                JokyCpsFrame *f = joky_cps_frame_new(FAILURE);
                JokyCpsFrame *s = joky_cps_frame_new(SCOPE);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING);
                uint64_t high = UINT64_C(0x800000000000005a);
                __joky_cps_slots(p)[0] = high;
                __joky_cps_slots(f)[0] = UINT64_C(1) << 32;
                __joky_cps_slots(cancelled)[0] = 7;
                for (int i = 0; i < 3; i++) { if (pending(p) != 1 || failure(f) != 1 || bad_scope(s) != 1 || pending(cancelled) != 1) return 4; }
                if (events != 7) return 5;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 0 || joky_cps_result(p) != (~high ^ UINT64_C(6))) return 6;
                if (failure(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED) return 7;
                joky_runtime_error_clear();
                if (bad_scope(s) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(s) != JOKY_CPS_FAILED || events != 7) return 8;
                joky_cps_frame_free(p); joky_cps_frame_free(f); joky_cps_frame_free(s);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING", "pending"),
            ("FAILURE", "failure"),
            ("SCOPE", "bad_scope"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        let output = link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror", "-I"])
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

#[test]
fn integer_conversion_only_objects_need_no_runtime() {
    let source = include_str!("../../../tests/fixtures/conversions.jk");
    let mut compiler = AotCompiler::new("pure_conversions").unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert!(!compiler.requires_runtime());
}

#[test]
#[cfg(unix)]
fn integer_conversion_native_objects_preserve_mixed_signatures_and_pending_values() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../tests/fixtures/conversions.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn record(n: UInt64) -> UInt64
            extern fn delay(n: UInt64) @suspends -> UInt64
            fn widen_signed(n: Int32) -> UInt64 { uint64(n) }
            fn widen_unsigned(n: UInt32) -> Int64 { int64(n) }
            fn narrow(n: UInt64) -> Int32 { int32(n) }
            fn reinterpret(n: UInt64) -> Int64 { int64(n) }
            fn pending(n: UInt64) @suspends -> UInt64 {
                var value = n
                let pair = (int32(value), uint32(delay({ value = 0 record(n) })))
                let (signed, unsigned) = pair
                uint64(int64(signed) + int64(unsigned)) ^ uint64(int32(delay(3))) ^ value
            }
            fn pending_signed(n: Int32) @suspends -> Int64 { int64(int32(delay(uint64(n)))) }
            fn pending_narrow(n: UInt64) @suspends -> Int32 { int32(delay(n)) }
            fn bad(n: UInt64) -> UInt64 { 1 << n }
            fn failure(n: UInt64) @suspends -> UInt64 { record(uint64(int32(bad(delay(n))))) }
            fn branch(n: UInt64) @suspends { record(uint64(uint32(delay(n)))); }
            fn scoped() @suspends -> UInt64 { scope { | branch(uint64(-1)) | branch(uint64(42)) } 42 }
        "#);
        let (dir, _cleanup) = scratch_dir("conversions");
        let object = dir.join("conversions.o");
        let program = dir.join("conversions");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("conversions").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let (declaration, check) = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            ("extern int32_t check(JokyCpsFrame *);", format!("JokyCpsFrame *c = joky_cps_frame_new({slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c);"))
        } else {
            (
                "extern int32_t check(void);",
                "if (check() != 42) return 1;".to_owned(),
            )
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint64_t record(uint64_t n) { events++; return n; }
            int32_t delay(JokyCpsFrame *f) {
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, __joky_cps_slots(f)[0]); return 0;
            }
            DECLARATION
            extern uint64_t widen_signed(int32_t);
            extern int64_t widen_unsigned(uint32_t), reinterpret(uint64_t);
            extern int32_t narrow(uint64_t);
            extern int32_t pending(JokyCpsFrame *), pending_signed(JokyCpsFrame *), pending_narrow(JokyCpsFrame *);
            extern int32_t failure(JokyCpsFrame *), scoped(JokyCpsFrame *);
            int main(void) {
                CHECK
                if (widen_signed(-1) != UINT64_MAX || widen_signed(INT32_MIN) != UINT64_MAX - INT32_MAX) return 2;
                if (widen_unsigned(UINT32_MAX) != INT64_C(4294967295) || narrow(UINT64_MAX) != -1 || narrow(UINT64_C(0x10000002a)) != 42) return 3;
                if (reinterpret(UINT64_MAX) != -1 || reinterpret(UINT64_C(1) << 63) != INT64_MIN) return 4;
                JokyCpsFrame *p = joky_cps_frame_new(PENDING);
                JokyCpsFrame *s = joky_cps_frame_new(SIGNED);
                JokyCpsFrame *n = joky_cps_frame_new(NARROW);
                JokyCpsFrame *f = joky_cps_frame_new(FAILURE);
                JokyCpsFrame *scope = joky_cps_frame_new(SCOPE);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING);
                __joky_cps_slots(p)[0] = UINT64_C(0x80000000ffffffff);
                __joky_cps_slots(s)[0] = UINT32_MAX;
                __joky_cps_slots(n)[0] = UINT64_MAX;
                __joky_cps_slots(f)[0] = 64;
                __joky_cps_slots(cancelled)[0] = 7;
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || pending_signed(s) != 1 || pending_narrow(n) != 1 || failure(f) != 1 || scoped(scope) != 1 || pending(cancelled) != 1) return 5;
                }
                if (events != 9) return 6;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 1 || pending(p) != 0 || joky_cps_result(p) != (UINT64_C(4294967294) ^ 3)) return 7;
                if (pending_signed(s) != 0 || joky_cps_result(s) != UINT64_MAX) return 8;
                if (pending_narrow(n) != 0 || (uint32_t)joky_cps_result(n) != UINT32_MAX) return 9;
                if (failure(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED) return 10;
                joky_runtime_error_clear();
                if (scoped(scope) != 0 || joky_cps_result(scope) != 42 || events != 12) return 11;
                joky_cps_frame_free(p); joky_cps_frame_free(s); joky_cps_frame_free(n);
                joky_cps_frame_free(f); joky_cps_frame_free(scope);
                return 0;
            }
        "#.replace("DECLARATION", declaration).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING", "pending"),
            ("SIGNED", "pending_signed"),
            ("NARROW", "pending_narrow"),
            ("FAILURE", "failure"),
            ("SCOPE", "scoped"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        let output = link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror", "-I"])
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

#[test]
#[cfg(unix)]
fn generic_native_objects_preserve_aggregate_specializations_and_pending_frames() {
    for suspends in [false, true] {
        let mut source = with_suspends(
            include_str!("../../../tests/fixtures/generics.jk"),
            suspends,
            Suspends::Typed,
        );
        source.push_str(r#"
            extern fn record(n: UInt64) -> UInt64
            extern fn delay(n: UInt64) @suspends -> UInt64
            enum Deferred<T> { Empty Value(T) }
            type Saved<T> = Deferred<T>
            type Value<T> = T
            impl<T> Deferred<T> {
                fn value(value: T) -> Self { Self.Value(value) }
                fn hold<U>(self, keep: U, n: UInt64) @suspends -> (Self, U) { delay(n); (self, keep) }
                fn unwrap(self, fallback: T) -> T { match self { | Empty => fallback | Value(value) => value } }
                fn bad<U>(self, keep: U, n: UInt64) -> (Self, U) { 1 << n; (self, keep) }
            }
            fn hold_enum<T>(value: Saved<T>, n: UInt64) @suspends -> Deferred<T> {
                let (saved, extra) = value.hold::<(Bool, UInt64)>((true, 0xffff_ffff_ffff_ffff), n)
                let (valid, wide) = extra
                if !valid || wide != 0xffff_ffff_ffff_ffff { 1 / 0; }
                saved
            }
            fn delayed<T>(value: Value<T>, n: UInt64) @suspends -> Value<T> {
                var saved = value
                let result = Option.None::<Bool>.zip(Option.Some({ saved = hold_enum(Deferred.value(value), n).unwrap(value) saved }))
                saved
            }
            fn keep<T>(a: T, b: T) -> T { a }
            fn bad<T>(value: T, n: UInt64) -> T { let (saved, extra) = Deferred.Value(value).bad(false, n) saved.unwrap(value) }
            fn bad_delayed<T>(value: T, n: UInt64) @suspends -> T {
                let err: Result<Unit, Bool> = Err(false)
                let result = err.and::<T>(Ok(Option.Some(value).unwrap_or(bad(value, delay(n)))))
                value
            }
            fn branch<T>(value: T, n: UInt64) @suspends { let _ = delayed(value, n) record(n); }
            fn bad_branch<T>(value: T) @suspends { bad_delayed(value, 64); record(99); }
            fn pending(n: UInt64) @suspends -> UInt64 {
                var pair = Pair { low: -1 high: n }
                let saved = Option.Some(pair).or(Option.Some(delayed({ pair.high = 0 pair }, record(n)))).unwrap_or(pair)
                let new = delayed(saved, 3)
                new.high + uint64(new.low) + pair.high
            }
            fn failure(n: UInt64) @suspends -> UInt64 { bad_delayed((42, true), n); record(99) }
            fn scoped() @suspends -> UInt64 { scope { | branch(Pair { low: 1 high: 2 }, 7) | branch::<Unit>({}, 8) } 42 }
            fn failed_scope() @suspends -> UInt64 { scope { | bad_branch(false) | branch({}, 9) } record(99) }
        "#);
        let (dir, _cleanup) = scratch_dir("generics");
        let object = dir.join("generics.o");
        let program = dir.join("generics");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("generics").unwrap();
        let exports = compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        assert!(!exports
            .iter()
            .any(|name| name == "identity" || name.starts_with("__joky_generic_")));
        assert_eq!(compiler.cps_frame_slots("delayed"), None);
        assert!(compiler.requires_runtime());
        let (declarations, check) = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            ("extern int32_t check(JokyCpsFrame *);", format!("JokyCpsFrame *c = joky_cps_frame_new({slots}); if (check(c) != 0 || joky_cps_result(c) != 42) return 1; joky_cps_frame_free(c);"))
        } else {
            (
                "extern int32_t check(void);",
                "if (check() != 42) return 1;".to_owned(),
            )
        };
        let mut harness = r#"
            #include "joky_cps.h"
            #include "joky_runtime.h"
            static int ready, events;
            uint64_t record(uint64_t n) { events++; return n; }
            int32_t delay(JokyCpsFrame *f) {
                if (!joky_cps_state(f)) { events++; joky_cps_set_state(f, 1); return 1; }
                if (!ready) return 1;
                joky_cps_set_result(f, __joky_cps_slots(f)[0]); return 0;
            }
            DECLARATIONS
            extern int32_t pending(JokyCpsFrame *), failure(JokyCpsFrame *), scoped(JokyCpsFrame *), failed_scope(JokyCpsFrame *);
            int main(void) {
                CHECK
                JokyCpsFrame *p = joky_cps_frame_new(PENDING);
                JokyCpsFrame *f = joky_cps_frame_new(FAILURE);
                JokyCpsFrame *s = joky_cps_frame_new(SCOPE);
                JokyCpsFrame *bad = joky_cps_frame_new(BADSCOPE);
                JokyCpsFrame *cancelled = joky_cps_frame_new(PENDING);
                uint64_t high = UINT64_C(0x80000000ffffffff);
                __joky_cps_slots(p)[0] = high;
                __joky_cps_slots(f)[0] = 64;
                __joky_cps_slots(cancelled)[0] = 7;
                for (int i = 0; i < 3; i++) {
                    if (pending(p) != 1 || failure(f) != 1 || scoped(s) != 1 || failed_scope(bad) != 1 || pending(cancelled) != 1) return 2;
                }
                if (events != 9) return 3;
                joky_cps_frame_free(cancelled);
                ready = 1;
                if (pending(p) != 1 || pending(p) != 0 || joky_cps_result(p) != high - 1) return 4;
                if (failure(f) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(f) != JOKY_CPS_FAILED) return 5;
                joky_runtime_error_clear();
                if (failed_scope(bad) != JOKY_CPS_RUNTIME_ERROR || !joky_runtime_error_code() || joky_cps_state(bad) != JOKY_CPS_FAILED) return 6;
                joky_runtime_error_clear();
                if (scoped(s) != 0 || joky_cps_result(s) != 42 || events != 12) return 7;
                joky_cps_frame_free(p); joky_cps_frame_free(f); joky_cps_frame_free(s); joky_cps_frame_free(bad);
                return 0;
            }
        "#.replace("DECLARATIONS", declarations).replace("CHECK", &check);
        for (marker, name) in [
            ("PENDING", "pending"),
            ("FAILURE", "failure"),
            ("BADSCOPE", "failed_scope"),
            ("SCOPE", "scoped"),
        ] {
            harness = harness.replace(marker, &compiler.cps_frame_slots(name).unwrap().to_string());
        }
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        let output = link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror", "-I"])
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

#[test]
fn unused_generic_templates_have_no_exports_or_runtime_dependencies() {
    let source = "struct Marker<T> {} impl<T> Marker<T> { fn unused<U>(self, value: U) @suspends -> Self { 1 / 0; self } } fn unused<T>(x: T) @suspends -> T { 1 / 0; x } fn id<T>(x: T) -> T { x } fn answer() -> Int32 { id(42) }";
    let mut compiler = AotCompiler::new("unused_generics").unwrap();
    assert_eq!(
        compiler
            .compile_module(&crate::parser::parse(source).unwrap())
            .unwrap(),
        ["answer"]
    );
    assert!(!compiler.requires_runtime());
}

#[test]
#[cfg(unix)]
fn generic_type_native_objects_preserve_direct_and_cps_specializations() {
    for fixture in [
        include_str!("../../../tests/fixtures/generic_enums.jk"),
        include_str!("../../../tests/fixtures/generic_records.jk"),
        include_str!("../../../tests/fixtures/generic_aliases.jk"),
        include_str!("../../../tests/fixtures/generic_impls.jk"),
        include_str!("../../../tests/fixtures/generic_methods.jk"),
        include_str!("../../../tests/fixtures/generic_library_methods.jk"),
        include_str!("../../../tests/fixtures/option_result_methods.jk"),
        r#"
            import model.{type Saved as Alias, make, first}
            struct Hidden<T> { wrong: T }
            fn check() -> Int32 { let p: Alias<Int64> = make(int64(42)) int32(first(p)) }
            fn wide() -> UInt64 { let value: UInt64 = 0xffff_ffff_ffff_ffff first(make(value)) }
        "#,
        r#"
            import model.{type Pair as Renamed}
            struct Pair<T> { wrong: T }
            fn helper<T>(value: T) -> Bool { false }
            fn check() -> Int32 { let (value, valid) = Renamed.make(0, 42).swap().zip(true) if valid { value } else { 0 } }
            fn wide() -> UInt64 { let (value, extra) = model.Pair::<UInt64>.zip::<Unit>(model.Pair::<UInt64>.make(0xffff_ffff_ffff_ffff, 0), {}) value }
        "#,
    ] {
        for suspends in [false, true] {
            let source = with_suspends(fixture, suspends, Suspends::Typed);
            let (dir, _cleanup) = scratch_dir("generic_types");
            let object = dir.join("generic_types.o");
            let program = dir.join("generic_types");
            let host = dir.join("host.c");
            let mut compiler = AotCompiler::new("generic_types").unwrap();
            compiler.set_prelude_enabled(false).unwrap();
            let mut modules = crate::ModuleSources::new();
            modules
                .insert(
                    "model",
                    with_suspends(
                        r#"
                struct Hidden<T> { value: T }
                pub type Saved<T> = Hidden<T>
                pub fn make<T>(value: T) -> Saved<T> { Hidden { value: value } }
                pub fn first<T>(p: Saved<T>) -> T { p.value }
                fn helper<T>(value: T) -> T { value }
                pub struct Pair<T> { left: T right: T }
                impl<T> Pair<T> {
                    pub fn make(left: T, right: T) -> Self { Self { left: left right: right } }
                    fn private(self) -> T { self.left }
                    pub fn first(self) -> T { self.private() }
                    pub fn swap(self) -> Self { Self.make(self.right, self.left) }
                    fn private_zip<U>(self, value: U) -> (T, U) { (self.private(), helper(value)) }
                    pub fn zip<U>(self, value: U) -> (T, U) { self.private_zip::<U>(value) }
                }
            "#,
                        suspends,
                        Suspends::Typed,
                    ),
                )
                .unwrap();
            let ast = compiler
                .types
                .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
                .unwrap();
            compiler.compile_module(&ast).unwrap();
            let harness = if suspends {
                let check = compiler.cps_frame_slots("check").unwrap();
                let wide = compiler.cps_frame_slots("wide").unwrap();
                format!(
                    r#"
                #include "joky_cps.h"
                extern int32_t check(JokyCpsFrame *), wide(JokyCpsFrame *);
                int main(void) {{
                    JokyCpsFrame *c = joky_cps_frame_new({check});
                    if (check(c) != 0 || joky_cps_result(c) != 42) return 1;
                    joky_cps_frame_free(c);
                    c = joky_cps_frame_new({wide});
                    if (wide(c) != 0 || joky_cps_result(c) != UINT64_MAX) return 2;
                    joky_cps_frame_free(c);
                    return 0;
                }}
            "#
                )
            } else {
                "#include <stdint.h>\nextern int32_t check(void); extern uint64_t wide(void);\nint main(void) { return check() != 42 || wide() != UINT64_MAX; }".to_owned()
            };
            compiler.write_object(&object).unwrap();
            std::fs::write(&host, harness).unwrap();
            let output = link_runtime(
                Command::new("cc")
                    .arg(&host)
                    .arg(&object)
                    .args(["-Wall", "-Wextra", "-Werror", "-I"])
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
}

#[test]
#[cfg(unix)]
fn inherent_method_native_objects_preserve_direct_and_cps_calls() {
    for suspends in [false, true] {
        let source = with_suspends(
            include_str!("../../../tests/fixtures/methods.jk"),
            suspends,
            Suspends::Typed,
        );
        let (dir, _cleanup) = scratch_dir("methods");
        let object = dir.join("methods.o");
        let program = dir.join("methods");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("methods").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let harness = if suspends {
            let check = compiler.cps_frame_slots("check").unwrap();
            let wide = compiler.cps_frame_slots("wide").unwrap();
            format!(
                r#"
                #include "joky_cps.h"
                extern int32_t check(JokyCpsFrame *), wide(JokyCpsFrame *);
                int main(void) {{
                    JokyCpsFrame *c = joky_cps_frame_new({check});
                    if (check(c) != 0 || joky_cps_result(c) != 42) return 1;
                    joky_cps_frame_free(c);
                    c = joky_cps_frame_new({wide});
                    if (wide(c) != 0 || joky_cps_result(c) != UINT64_MAX) return 2;
                    joky_cps_frame_free(c);
                    return 0;
                }}
            "#
            )
        } else {
            "#include <stdint.h>\nextern int32_t check(void); extern uint64_t wide(void);\nint main(void) { return check() != 42 || wide() != UINT64_MAX; }".to_owned()
        };
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        let output = link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror", "-I"])
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

#[test]
#[cfg(unix)]
fn concrete_branch_hints_match_native_direct_and_cps_execution() {
    for suspends in [false, true] {
        let source = with_suspends(
            include_str!("../../../tests/fixtures/inference.jk"),
            suspends,
            Suspends::All,
        );
        let mut compiler = AotCompiler::new("inference").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let (dir, _cleanup) = scratch_dir("inference");
        let object = dir.join("inference.o");
        let host = dir.join("host.c");
        let program = dir.join("inference");
        let harness = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!(
                r#"
                #include "joky_cps.h"
                extern int32_t check(JokyCpsFrame *);
                int main(void) {{
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    int failed = check(frame) != 0 || joky_cps_result(frame) != 42;
                    joky_cps_frame_free(frame);
                    return failed;
                }}
                "#
            )
        } else {
            "#include <stdint.h>\nextern int32_t check(void);\nint main(void) { return check() != 42; }".to_owned()
        };
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        let output = link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror", "-I"])
                .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include")),
        )
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn method_native_pending_arguments_keep_snapshots_and_do_not_replay_effects() {
    let source = r#"
        import joky/integer
        extern fn record(n: Int32) -> Int32
        extern fn delay(n: Int32) @suspends -> Int32
        struct Pair { x: Int32 }
        impl Pair {
            fn add(self, n: Int32) @suspends -> Pair { Pair { x: self.x + delay(n) } }
        }
        impl Int32 {
            fn receive(self, other: Int32) @suspends -> Option<UInt64> { delay(other) self.to_u64() }
            fn branch(self) @suspends { record(delay(self)); }
        }
        fn pending() @suspends -> Int32 {
            var pair = Pair { x: record(40) }
            let result = pair.add({ pair.x = 99 record(2) })
            result.x + pair.x
        }
        fn receiver() @suspends -> Int32 {
            match delay(42).receive(record(7)) { | Some(n) => int32(n) | None => -1 }
        }
        fn scoped() @suspends -> Int32 { scope { | 1.branch() | 2.branch() } 42 }
        fn failure() @suspends -> Int32 { (1 / delay(0)).to_i32(); record(99) }
    "#;
    let (dir, _cleanup) = scratch_dir("method_pending");
    let object = dir.join("methods.o");
    let host = dir.join("host.c");
    let program = dir.join("methods");
    let mut compiler = AotCompiler::new("method_pending").unwrap();
    let exports = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert!(!exports.iter().any(|name| name.contains('.')));
    let pending = compiler.cps_frame_slots("pending").unwrap();
    let receiver = compiler.cps_frame_slots("receiver").unwrap();
    let scoped = compiler.cps_frame_slots("scoped").unwrap();
    let failure = compiler.cps_frame_slots("failure").unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, format!(r#"
        #include "joky_cps.h"
        #include "joky_runtime.h"
        extern int32_t pending(JokyCpsFrame *), receiver(JokyCpsFrame *), scoped(JokyCpsFrame *), failure(JokyCpsFrame *);
        static int ready, count;
        static int events[32];
        int32_t record(int32_t n) {{ events[count++] = n; return n; }}
        int32_t delay(JokyCpsFrame *frame) {{
            if (joky_cps_state(frame) == 0) {{
                record((int32_t)__joky_cps_slots(frame)[0]);
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }}
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0]);
            return JOKY_CPS_COMPLETED;
        }}
        int main(void) {{
            JokyCpsFrame *a = joky_cps_frame_new({pending}), *b = joky_cps_frame_new({receiver});
            JokyCpsFrame *c = joky_cps_frame_new({scoped}), *d = joky_cps_frame_new({failure});
            JokyCpsFrame *cancelled = joky_cps_frame_new({pending});
            for (int i = 0; i < 3; i++) {{
                if (pending(a) != 1 || receiver(b) != 1 || scoped(c) != 1 || failure(d) != 1 || pending(cancelled) != 1) return 1;
            }}
            int initial[] = {{40,2,2,42,1,2,0,40,2,2}};
            if (count != 10) return 2;
            for (int i = 0; i < 10; i++) if (events[i] != initial[i]) return 3;
            joky_cps_frame_free(cancelled);
            ready = 1;
            if (pending(a) != 0 || joky_cps_result(a) != 141) return 4;
            if (receiver(b) != 1 || receiver(b) != 0 || joky_cps_result(b) != 42) return 5;
            if (scoped(c) != 0 || joky_cps_result(c) != 42) return 6;
            if (failure(d) != JOKY_CPS_RUNTIME_ERROR) return 7;
            if (count != 14 || events[10] != 7 || events[11] != 7 || events[12] != 1 || events[13] != 2) return 8;
            joky_cps_frame_free(a); joky_cps_frame_free(b); joky_cps_frame_free(c); joky_cps_frame_free(d);
            return 0;
        }}
    "#)).unwrap();
    let output = link_runtime(
        Command::new("cc")
            .arg(&host)
            .arg(&object)
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
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
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[cfg(unix)]
fn checked_integer_method_matrix_matches_native_direct_and_cps() {
    for suspends in [false, true] {
        let source = include_str!("../../../tests/fixtures/checked_conversions.jk");
        let source = with_suspends(source, suspends, Suspends::Named("check"));
        let mut compiler = AotCompiler::new("checked_conversions").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        assert_eq!(compiler.requires_runtime(), suspends);
        let (dir, _cleanup) = scratch_dir("checked_conversions");
        let object = dir.join("checked.o");
        let host = dir.join("host.c");
        let program = dir.join("checked");
        let harness = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!("#include \"joky_cps.h\"\nextern int32_t check(JokyCpsFrame *); int main(void) {{ JokyCpsFrame *f = joky_cps_frame_new({slots}); int status = check(f); int result = (int)joky_cps_result(f); joky_cps_frame_free(f); return status || result; }}")
        } else {
            "#include <stdint.h>\nextern int32_t check(void); int main(void) { return check(); }"
                .into()
        };
        std::fs::write(&host, harness).unwrap();
        compiler.write_object(&object).unwrap();
        let mut command = Command::new("cc");
        command
            .arg(host)
            .arg(object)
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"));
        if suspends {
            link_runtime(&mut command);
        }
        let output = command.arg("-o").arg(&program).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(program).output().unwrap();
        assert!(output.status.success(), "suspends={suspends}: {output:?}");
    }
}

#[test]
#[cfg(unix)]
fn imported_constructor_aliases_execute_in_direct_and_cps_objects() {
    for suspends in [false, true] {
        let source = with_suspends(
            include_str!("../../../tests/fixtures/imports.jk"),
            suspends,
            Suspends::Typed,
        );
        let (dir, _cleanup) = scratch_dir("imports");
        let object = dir.join("imports.o");
        let program = dir.join("imports");
        let host = dir.join("host.c");
        let mut compiler = AotCompiler::new("imports").unwrap();
        compiler.set_prelude_enabled(!suspends).unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let harness = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!(
                r#"
                #include "joky_cps.h"
                extern int32_t check(JokyCpsFrame *);
                int main(void) {{
                    JokyCpsFrame *frame = joky_cps_frame_new({slots});
                    int status = check(frame);
                    int failed = status != 0 || joky_cps_result(frame) != 42;
                    joky_cps_frame_free(frame);
                    return failed;
                }}
            "#
            )
        } else {
            "#include <stdint.h>\nextern int32_t check(void);\nint main(void) { return check() != 42; }".into()
        };
        compiler.write_object(&object).unwrap();
        std::fs::write(&host, harness).unwrap();
        let output = link_runtime(
            Command::new("cc")
                .arg(&host)
                .arg(&object)
                .args(["-Wall", "-Wextra", "-Werror", "-I"])
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

#[test]
#[cfg(unix)]
fn arena_objects_use_task_wrappers_and_cleanup_in_native_objects() {
    let source = include_str!("../../../tests/fixtures/arena.jk");
    let (dir, _cleanup) = scratch_dir("arena");
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
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
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
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn arena_graphs_execute_recursive_and_shared_objects_in_native_code() {
    let source = include_str!("../../../tests/fixtures/arena_graphs.jk");
    let (dir, _cleanup) = scratch_dir("arena-graphs");
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
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
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
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn arena_cycles_execute_sealed_graphs_in_native_code() {
    let source = include_str!("../../../tests/fixtures/arena_cycles.jk");
    let (dir, _cleanup) = scratch_dir("arena-cycles");
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
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
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
    assert!(Command::new(executable).status().unwrap().success());
}

#[test]
fn local_borrows_execute_in_standalone_native_code() {
    let source = include_str!("../../../tests/fixtures/local_borrows.jk");
    let (dir, _cleanup) = scratch_dir("local-borrows");
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
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
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
    let source = include_str!("../../../tests/fixtures/local_fields.jk");
    let (dir, _cleanup) = scratch_dir("local-fields");
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
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
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
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn check_arena_cps_fixture(
    name: &str,
    source: &str,
    cases: &[(&str, &str, usize)],
    extra: impl FnOnce(&AotCompiler) -> String,
) {
    let (dir, _cleanup) = scratch_dir(name);
    let object = dir.join("arena.o");
    let host = dir.join("host.c");
    let executable = dir.join("arena");
    let mut compiler = AotCompiler::new(name).unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let mut harness = String::from(
        r#"
        #include "joky_cps.h"
        #include "joky_runtime.h"
        static int ready, count;
        static int32_t events[64];
        int32_t record(int32_t value) {
            if (count < 64) events[count] = value;
            ++count; return value;
        }
        int32_t delayed_value(JokyCpsFrame *frame) {
            if (joky_cps_state(frame) == 0) {
                record((int32_t)__joky_cps_slots(frame)[0]);
                joky_cps_set_state(frame, 1);
                return JOKY_CPS_PENDING;
            }
            if (!ready) return JOKY_CPS_PENDING;
            joky_cps_set_result(frame, __joky_cps_slots(frame)[0] + 1);
            return JOKY_CPS_COMPLETED;
        }
        int32_t fail_now(JokyCpsFrame *frame) { (void)frame; return 7; }
        static int check(JokyCpsEntry entry, size_t slots, const int32_t *expected, int length) {
            ready = 0; count = 0;
            JokyCpsFrame *frame = joky_cps_frame_new(slots);
            if (entry(frame) != JOKY_CPS_PENDING) return 1;
            int pending_count = count;
            for (int i = 0; i < 4; ++i) {
                if (entry(frame) != JOKY_CPS_PENDING || count != pending_count) return 2;
            }
            ready = 1;
            int status = JOKY_CPS_PENDING;
            for (int i = 0; i < 16 && status == JOKY_CPS_PENDING; ++i) status = entry(frame);
            if (status != JOKY_CPS_COMPLETED || joky_cps_state(frame) != JOKY_CPS_DONE) return 3;
            if (joky_cps_result(frame) != 42 || joky_runtime_error_code() || count != length) return 4;
            for (int i = 0; i < length; ++i) if (events[i] != expected[i]) return 5;
            if (entry(frame) != JOKY_CPS_INVALID_STATE) return 6;
            joky_cps_frame_free(frame);
            return 0;
        }
    "#,
    );
    for (name, _, _) in cases {
        harness.push_str(&format!("extern int32_t {name}(JokyCpsFrame *);\n"));
    }
    harness.push_str("int main(void) { for (int run = 0; run < 20; ++run) {\n");
    for (name, events, length) in cases {
        let slots = compiler.cps_frame_slots(name).unwrap();
        harness.push_str(&format!("{{ const int32_t expected[] = {{{events}}}; if (check({name}, {slots}, expected, {length})) return 10; }}\n"));
    }
    harness.push_str(&extra(&compiler));
    harness.push_str("} return 0; }\n");
    compiler.write_object(&object).unwrap();
    std::fs::write(&host, harness).unwrap();
    let linked = link_runtime(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include"))
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
    let output = Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn arena_suspension_executes_in_standalone_native_code() {
    let cases = [
        ("arena_suspend", "10, 30", 2),
        ("arena_return_suspend", "20, 0, 20, 0, 0", 5),
        ("arena_cycle_suspend", "20, 22, 0", 3),
        ("arena_loop_suspend", "13, 0, 13, 0, 13, 0", 6),
        ("arena_scope_wait", "42, 0, 0", 3),
    ];
    check_arena_cps_fixture(
        "arena-suspension",
        include_str!("../../../tests/fixtures/arena_suspension.jk"),
        &cases,
        |compiler| {
            let mut extra = String::new();
            for (name, code) in [("arena_fail_suspend", 3), ("arena_host_fail_suspend", 7)] {
                let slots = compiler.cps_frame_slots(name).unwrap();
                extra.push_str(&format!(r#"
                    {{
                        extern int32_t {name}(JokyCpsFrame *);
                        ready = 0; count = 0;
                        JokyCpsFrame *frame = joky_cps_frame_new({slots});
                        if ({name}(frame) != JOKY_CPS_PENDING) return 11;
                        ready = 1;
                        if ({name}(frame) != {code} || joky_cps_state(frame) != JOKY_CPS_FAILED) return 12;
                        joky_runtime_error_clear();
                        if ({name}(frame) != JOKY_CPS_INVALID_STATE) return 13;
                        joky_cps_frame_free(frame);
                    }}
                "#));
            }
            let slots = compiler.cps_frame_slots("arena_cycle_suspend").unwrap();
            extra.push_str(&format!(
                r#"
                ready = 0; count = 0;
                JokyCpsFrame *cancelled = joky_cps_frame_new({slots});
                if (arena_cycle_suspend(cancelled) != JOKY_CPS_PENDING) return 14;
                joky_cps_frame_free(cancelled);
            "#
            ));
            extra
        },
    );
}

#[test]
fn arena_scope_executes_in_standalone_native_code() {
    let cases = [
        ("scope_shared", "20, 0, 20, 1, 20, 20", 6),
        ("scope_distinct", "0, 0, 21, 21", 4),
        ("scope_nested", "0, 20, 1, 20, 2, 21, 20, 20", 8),
        ("scope_graph", "20, 0, 22, 1, 22, 20", 6),
        ("scope_aggregate", "0, 1, 2", 3),
        ("scope_retained", "0, 1, 21, 21", 4),
        ("scope_allocations", "20, 20, 22, 22, 42, 42, 20, 22, 42", 9),
        ("scope_partial", "20, 20, 22, 1, 20, 2, 22", 7),
        ("scope_loop", "6, 6, 7, 7, 6, 6, 14, 14, 6, 6, 21, 21", 12),
        ("scope_arguments", "0, -1, -1, 0, 0, 21, 21", 7),
    ];
    check_arena_cps_fixture(
        "arena-scope",
        include_str!("../../../tests/fixtures/arena_scope.jk"),
        &cases,
        |compiler| {
            let partial = compiler.cps_frame_slots("scope_partial").unwrap();
            let failure = compiler.cps_frame_slots("scope_failure").unwrap();
            let argument = compiler.cps_frame_slots("scope_argument_failure").unwrap();
            format!(
                r#"
                extern int32_t scope_failure(JokyCpsFrame *);
                extern int32_t scope_argument_failure(JokyCpsFrame *);
                ready = 0; count = 0;
                JokyCpsFrame *partial = joky_cps_frame_new({partial});
                if (scope_partial(partial) != JOKY_CPS_PENDING || count != 4) return 20;
                ready = 1;
                if (scope_partial(partial) != JOKY_CPS_PENDING || count != 6) return 21;
                if (events[4] != 20 || events[5] != 2) return 22;
                /* Cancel after one branch completes and another is still pending. */
                joky_cps_frame_free(partial);

                ready = 0; count = 0;
                JokyCpsFrame *failed = joky_cps_frame_new({failure});
                __joky_cps_slots(failed)[0] = 0;
                if (scope_failure(failed) != JOKY_CPS_PENDING) return 23;
                ready = 1;
                if (scope_failure(failed) != JOKY_CPS_RUNTIME_ERROR) return 24;
                if (joky_cps_state(failed) != JOKY_CPS_FAILED || !joky_runtime_error_code()) return 25;
                const int32_t expected[] = {{20, 20, 22, 1, 3, 40, 40, 20, 2}};
                if (count != 9) return 26;
                for (int i = 0; i < 9; ++i) if (events[i] != expected[i]) return 27;
                joky_runtime_error_clear();
                if (scope_failure(failed) != JOKY_CPS_INVALID_STATE) return 28;
                joky_cps_frame_free(failed);

                ready = 0; count = 0;
                JokyCpsFrame *argument = joky_cps_frame_new({argument});
                __joky_cps_slots(argument)[0] = 0;
                if (scope_argument_failure(argument) != JOKY_CPS_PENDING) return 29;
                ready = 1;
                if (scope_argument_failure(argument) != JOKY_CPS_RUNTIME_ERROR) return 30;
                if (joky_cps_state(argument) != JOKY_CPS_FAILED || count != 1 || events[0] != 1) return 31;
                joky_runtime_error_clear();
                joky_cps_frame_free(argument);
            "#
            )
        },
    );
}
