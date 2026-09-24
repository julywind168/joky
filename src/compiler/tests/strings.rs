use super::*;

#[test]
fn raw_and_multiline_string_literals_reach_the_runtime() {
    run_program(include_str!(
        "../../../tests/fixtures/raw_and_multiline_strings.jk"
    ));
}

#[test]
fn byte_string_literals_reach_the_runtime() {
    run_program(include_str!("../../../tests/fixtures/byte_strings.jk"));
}
