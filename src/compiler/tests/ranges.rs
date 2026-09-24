use super::*;

#[test]
fn ranges_iterate_with_boundaries_steps_and_cursor_composition() {
    run_program(include_str!("../../../tests/fixtures/ranges.jk"));
}

#[test]
fn ranges_reject_invalid_types_steps_and_parallel_iteration() {
    for (source, expected) in [
        (
            "fn main() { let r = 0..5 by 0 }",
            "range step must not be zero",
        ),
        (
            "fn main() { let r = 0..5 by (2 - 2) }",
            "range step must not be zero",
        ),
        ("fn main() { let r = 0.0..5.0 }", "integer type"),
        ("fn main() { let r = 1ms..5ms }", "integer type"),
        ("fn main() { let r: Range(Float64) = 0..5 }", "integer type"),
        (
            "fn main() { let end: Int64 = 5; let start: Int32 = 0; let r = start..end }",
            "expected Int32",
        ),
        (
            "fn main() { let r: Range(UInt8) = 5..0 by -1 }",
            "cannot negate",
        ),
        (
            "fn main() { let r = @parallel(limit: 2) for i in 0..5 { i } }",
            "parallel for requires a List",
        ),
    ] {
        let error = try_run_program(source).expect_err(source).to_string();
        assert!(error.contains(expected), "{source}: {error}");
    }
}
