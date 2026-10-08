//! Control flow AOT fixtures.
use super::*;

#[test]
#[cfg(unix)]
fn direct_aot_loops_assignments_and_comparisons_execute_in_a_native_process() {
    let mut source = include_str!("../../../../tests/fixtures/loops.jk").to_owned();
    source.push_str(include_str!("../../../../tests/fixtures/sequences.jk"));
    source.push_str(include_str!("../../../../tests/fixtures/expressions.jk"));
    source.push_str(include_str!("../../../../tests/fixtures/conditionals.jk"));
    source.push_str(include_str!("../../../../tests/fixtures/jumps.jk"));
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
    let fixture = ScratchDir::new("loops");
    let dir = fixture.path();
    let object = dir.join("loops.o");
    let host = dir.join("host.c");
    let executable = dir.join("loops");
    let mut compiler = AotCompiler::new("loops").unwrap();
    compiler.compile_module(&ast).unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
    compiler.write_object(&object).unwrap();
    std::fs::write(
        &host,
        r#"
        #include "text.h"
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
            if (expression_newline() != -2 || expression_sequence_arguments() != -23) return 17;
            if (conditional_scopes() != 34 || logical_precedence() != 1 || logical_effects() != 3) return 22;
            if (logical_snapshots() != 7 || conditional_grouping() != 11) return 23;
            if (conditional_loop(0) != 0 || conditional_loop(3) != 12 || conditional_loop(10) != 32) return 24;
            if (conditional_safe_division(0) != 0 || conditional_safe_division(3) != 4 || conditional_safe_division(-1) != 0) return 25;
            conditional_empty();
            for (int a = 0; a < 2; a++) {
                if (return_bool(a) != !a) return 42;
                JokyOwnedTextV2 returned = {0};
                if (__joky_user_text_v2_return_text(a, &returned)) return 44;
                uint64_t returned_length = returned.byte_length;
                int returned_bad = returned_length != (a ? 3 : 2) || memcmp(returned.data, a ? "yes" : "no", returned_length);
                joky_string_result_free_v2(&returned);
                if (returned_bad) return 43;
                if (expression_not(a) != !a) return 18;
                conditional_unit(a);
                if (conditional_flag(a) != !a) return 26;
                JokyOwnedTextV2 selected = {0};
                if (__joky_user_text_v2_conditional_text(a, &selected)) return 44;
                uint64_t selected_length = selected.byte_length;
                int selected_bad = selected_length != (a ? 3 : 2) || memcmp(selected.data, a ? "yes" : "no", selected_length);
                joky_string_result_free_v2(&selected);
                if (selected_bad) return 27;
                for (int b = 0; b < 2; b++) {
                    if (expression_equal(a,b) != (a == b) || expression_unequal(a,b) != (a != b)) return 19;
                    int expected = a == b ? 10 : 1;
                    if (expression_truth(a,b) != expected || expression_bool_loops(a,b) != expected) return 20;
                    if (logical_values(a,b) != (a && b) * 10 + (a || b)) return 28;
                    if (conditional_nested(a,b) != (a ? (b ? 11 : 12) : 20)) return 29;
                }
            }
            JokyOwnedTextV2 text = {0};
            if (__joky_user_text_v2_string_value(&text)) return 6;
            int bad = text.byte_length != 5 || memcmp(text.data, "after", 5);
            joky_string_result_free_v2(&text);
            if (bad) return 6;
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
    link(
        link_runtime(
            Command::new("cc")
                .arg("-I")
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/include"))
                .args(["-Wall", "-Wextra", "-Werror"])
                .arg(&host)
                .arg(&object),
        ),
        &executable,
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
    let fixture = ScratchDir::new("direct");
    let dir = fixture.path();
    let object = dir.join("direct.o");
    let host = dir.join("host.c");
    let executable = dir.join("direct");
    let mut compiler = AotCompiler::new("direct").unwrap();
    compiler.compile_module(&ast).unwrap();
    std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
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
    link(
        link_runtime(
            Command::new("cc")
                .args(["-Wall", "-Wextra", "-Werror"])
                .arg(&host)
                .arg(&object),
        ),
        &executable,
    );
    assert!(Command::new(&executable).status().unwrap().success());
}

#[test]
#[cfg(unix)]
fn concrete_branch_hints_match_native_direct_and_cps_execution() {
    for suspends in [false, true] {
        let source = with_suspends(
            include_str!("../../../../tests/fixtures/inference.jk"),
            suspends,
            Suspends::All,
        );
        let mut compiler = AotCompiler::new("inference").unwrap();
        compiler
            .compile_module(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let fixture = ScratchDir::new("inference");
        let dir = fixture.path();
        let object = dir.join("inference.o");
        let host = dir.join("host.c");
        let program = dir.join("inference");
        let harness = if suspends {
            let slots = compiler.cps_frame_slots("check").unwrap();
            format!(
                r#"
                #include "joky_cps.h"
        #include "text.h"
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
        std::fs::write(fixture.path().join("text.h"), compiler.text_header_v2()).unwrap();
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
