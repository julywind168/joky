use super::{check_program, Type};
use crate::syntax::{parse_program, ExprKind};

fn inferred(expression: &str) -> Type {
    let source = format!("fn main() {{ let value = {expression}; () }}");
    let program = parse_program(&source).unwrap();
    let types = check_program(&program).unwrap_or_else(|error| panic!("{expression}: {error}"));
    let ExprKind::Block(body) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::Let { value, .. } = &body[0].kind else {
        panic!("binding")
    };
    types.get(value)
}

fn rejects(source: &str, message: &str) {
    let program = parse_program(source).unwrap();
    let error = check_program(&program).unwrap_err().to_string();
    assert!(
        error.contains(message),
        "{source}: expected {message}, got {error}"
    );
}

#[test]
fn integer_suffixes_fix_all_eight_types_and_boundaries() {
    for (suffix, ty, maximum) in [
        ("i8", Type::I8, i8::MAX as u64),
        ("i16", Type::I16, i16::MAX as u64),
        ("i32", Type::I32, i32::MAX as u64),
        ("i64", Type::I64, i64::MAX as u64),
        ("u8", Type::U8, u8::MAX as u64),
        ("u16", Type::U16, u16::MAX as u64),
        ("u32", Type::U32, u32::MAX as u64),
        ("u64", Type::U64, u64::MAX),
    ] {
        assert_eq!(inferred(&format!("0{suffix}")), ty);
        assert_eq!(inferred(&format!("{maximum}{suffix}")), ty);
        if maximum != u64::MAX {
            rejects(
                &format!("fn main() {{ let x = {}{suffix}; () }}", maximum + 1),
                "out of range",
            );
        }
    }
}

#[test]
fn integer_suffixes_allow_signed_minima_and_reject_unsigned_negation() {
    for (suffix, ty, magnitude) in [
        ("i8", Type::I8, 128u64),
        ("i16", Type::I16, 32768),
        ("i32", Type::I32, 2147483648),
        ("i64", Type::I64, 9223372036854775808),
    ] {
        assert_eq!(inferred(&format!("-{magnitude}{suffix}")), ty);
        rejects(
            &format!("fn main() {{ let x = -{}{suffix}; () }}", magnitude + 1),
            "out of range",
        );
    }
    for suffix in ["u8", "u16", "u32", "u64"] {
        for magnitude in [0, 1] {
            rejects(
                &format!("fn main() {{ let x = -{magnitude}{suffix}; () }}"),
                "cannot negate unsigned",
            );
        }
    }
}

#[test]
fn integer_suffixes_preserve_types_through_operators_and_casts() {
    for expression in [
        "1u8 + 2",
        "1 + 2u8",
        "1u8 + 2 + 3",
        "~0u8",
        "~~0u8",
        "1u8 << 2",
        "1 << 2u8",
        "1u8 | 2",
        "(1u8 +? 2)!",
        "(1 +? 2u8)!",
        "255u8 +% 1",
        "255u8 +| 1",
        "255u16 as% UInt8",
        "(255u16 as? UInt8)!",
        "255u16 as| UInt8",
        "1u8.max(2u8)",
    ] {
        assert_eq!(inferred(expression), Type::U8, "{expression}");
    }
    for expression in ["-1i8", "-(-1i8)", "-(1i8 + 2)", "~(-128i8)"] {
        assert_eq!(inferred(expression), Type::I8, "{expression}");
    }
    assert_eq!(inferred("1u8 as UInt64"), Type::U64);
    assert_eq!(inferred("1u8 == 1u8"), Type::Bool);
    assert_eq!(inferred("1_000u64"), Type::U64);
    assert_eq!(inferred("0xffu8"), Type::U8);
    assert_eq!(inferred("-0x80i8"), Type::I8);
}

#[test]
fn integer_suffixes_reject_conflicting_contexts() {
    for source in [
        "fn main() { let x: UInt32 = 1u8; () }",
        "fn main() { var x: UInt8 = 0; x = 1u16; () }",
        "fn f(x: UInt32) {} fn main() { f(1u8) }",
        "fn f() -> Int64 { 1i8 } fn main() {}",
        "fn main() { let x: Int64 = -1i8; () }",
        "fn main() { let x: UInt64 = ~0u8; () }",
        "fn main() { let x = if true 1u8 else 2u16; () }",
        "fn main() { let x = List(1u8, 2u16); () }",
        "fn main() { let x = 1u8 + 2u16; () }",
        "fn main() { let x = 1u8 +? 2u16; () }",
        "fn main() { let x: Result(UInt64, String) = 1u8 +? 2; () }",
    ] {
        rejects(source, "expected");
    }
    rejects(
        "fn main() { let x = 1u8 == 1u16; () }",
        "same comparable type",
    );
}

#[test]
fn integer_suffixes_keep_range_checks_before_casts_or_bit_not() {
    for expression in [
        "256u8 as UInt64",
        "256u8 as% Int8",
        "256u8 as| Int8",
        "256u8 as? Int8",
        "~256u8",
        "128i8 +% 1",
        "128i8 +| 1",
        "128i8 +? 1",
    ] {
        rejects(
            &format!("fn main() {{ let x = {expression}; () }}"),
            "out of range",
        );
    }
}

#[test]
fn integer_suffixes_participate_in_constant_validation() {
    for length in ["4u8", "256u8", "4i64"] {
        rejects(
            &format!("fn main() {{ let T: type = CArray(UInt8, {length}); () }}"),
            "CArray length must be an unsuffixed integer constant",
        );
    }
    rejects("fn main() { let x = 1u8 / 0u8; () }", "division by zero");
    rejects(
        "fn main() { let x = 1u8 / (2u8 - 2u8); () }",
        "division by zero",
    );
    rejects(
        "fn main() { let x = (-128i8).abs(); () }",
        "minimum integer",
    );
    rejects(
        "fn main() { let x = @parallel(limit: 0u64) for x in List(1, 2) { x }; () }",
        "parallel limit must be greater than zero",
    );
    let program = parse_program(
        "const N: Int8 = -128i8\nconst U: UInt64 = 18446744073709551615u64\nfn main() {}",
    )
    .unwrap();
    check_program(&program).unwrap();
}

#[test]
fn integer_suffixes_do_not_change_unsuffixed_or_duration_inference() {
    assert_eq!(inferred("42"), Type::I32);
    assert_eq!(inferred("-42"), Type::I32);
    assert_eq!(inferred("1.5"), Type::F64);
    assert_eq!(inferred("100ms"), Type::Duration);
    assert_eq!(inferred("1h2m3s4ms"), Type::Duration);
    let program =
        parse_program("fn main() { let x: UInt64 = 42; let y: Int8 = -128; () }").unwrap();
    check_program(&program).unwrap();
}
