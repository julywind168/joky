use super::*;
use crate::syntax::MAX_EXPRESSION_NESTING;

fn long_addition_source(terms: usize) -> String {
    let sum = (0..terms)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" + ");
    format!("fn main() {{ let _ = {sum} }}")
}

fn nested_if_source(depth: usize) -> String {
    let mut body = String::from("1");
    for _ in 0..depth {
        body = format!("if true {{ {body} }} else {{ 0 }}");
    }
    format!("fn main() {{ let _ = {body} }}")
}

fn wide_parallel_cown_source(branches: usize, rounds: usize) -> String {
    let bump = "when (counter) |state| { state.bump() }; ";
    let arms = (0..branches)
        .map(|index| format!("| {{ {}{} }}", bump.repeat(rounds), index))
        .collect::<Vec<_>>()
        .join("\n");
    let sum = (0..branches)
        .map(|index| format!("results.{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    format!(
        "class Counter {{\n\
         \x20   var value: Int32 = 0\n\
         \x20   fn bump() {{ self.value = self.value + 1 }}\n\
         }}\n\
         fn main() {{\n\
         \x20   let counter = Cown.new(Counter(value: 0))\n\
         \x20   let results = parallel {{\n{arms}\n}}\n\
         \x20   let total = when (counter) |state| {{ state.value }}\n\
         \x20   println(total)\n\
         \x20   println({sum})\n\
         }}"
    )
}

#[test]
fn compiles_wide_parallel_cown_contention_without_stack_overflow() {
    // Phase 4 recorded 64 branch × 8 rounds overflowing the compiler stack.
    // The hotspot was the left-associated `results.0 + results.1 + …` tree,
    // not the width of `parallel` itself.
    let source = wide_parallel_cown_source(64, 8);
    let mut compiler = Compiler::new().expect("compiler");
    compiler
        .compile_object_program(&source)
        .expect("64-arm parallel with a 64-term join sum should compile");
}

#[test]
fn compiles_long_left_associated_addition_chains() {
    let mut compiler = Compiler::new().expect("compiler");
    compiler
        .compile_object_program(&long_addition_source(128))
        .expect("128-term addition should compile iteratively");
}

#[test]
fn compiles_moderately_nested_if_expressions() {
    let mut compiler = Compiler::new().expect("compiler");
    compiler
        .compile_object_program(&nested_if_source(16))
        .expect("16 nested ifs should compile");
}

#[test]
fn diagnoses_expression_nesting_beyond_the_defined_limit() {
    let error = crate::syntax::parse_program(&nested_if_source(MAX_EXPRESSION_NESTING + 1))
        .expect_err("65 nested ifs must not overflow; they get a diagnostic");
    assert!(
        error.to_string().contains(&format!(
            "expression nesting exceeds {MAX_EXPRESSION_NESTING}"
        )),
        "{error}"
    );
}
