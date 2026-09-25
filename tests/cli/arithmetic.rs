use super::parity::Package;

#[test]
fn arithmetic_modes_survive_cache_and_aot() {
    Package::new(
        "arithmetic-modes",
        &[("main.jk", include_str!("../fixtures/arithmetic_modes.jk"))],
    )
    .check_cached("", None, &[]);
}

#[test]
fn arithmetic_modes_preserve_pending_operands() {
    Package::new(
        "arithmetic-pending",
        &[
            (
                "ops.jk",
                r#"
            import joky/time
            fn pending(value: Int8) -> Int8 effects { time } { time.sleep(1ms); value }
            pub fn checked(a: Int8, b: Int8) -> Result(Int8, String) effects { time } {
                let result = (pending(a) +? pending(b))?
                result *? b
            }
            pub fn saturated(a: Int8, b: Int8) -> Int8 effects { time } { pending(a) *| pending(b) }
            pub fn wrapped(a: Int8, b: Int8) -> Int8 effects { time } { pending(a) +% pending(b) }
        "#,
            ),
            (
                "main.jk",
                r#"
            import ops
            import joky/time
            fn main() effects { time } {
                if !ops.checked(127, 1).is_err() { panic("pending overflow") }
                if ops.checked(2, 3)! as Int32 != 15 { panic("pending checked success") }
                if ops.saturated(-128, -1) as Int32 != 127 { panic("pending clamp") }
                if ops.wrapped(127, 1) as Int32 != -128 { panic("pending wrap") }
            }
        "#,
            ),
        ],
    )
    .check_cached("", None, &[]);
}

#[test]
fn default_arithmetic_panics_consistently() {
    for (index, (ty, expression)) in [
        ("Int8", "127 + 1"),
        ("UInt8", "0 - 1"),
        ("Int64", "9223372036854775807 * 2"),
        ("Int8", "(-128) / (-1)"),
        ("Int64", "let value: Int64 = -9223372036854775808; -value"),
        ("Int8", "var value: Int8 = 127; value += 1; value"),
    ]
    .into_iter()
    .enumerate()
    {
        let source =
            format!("fn fail() -> {ty} {{ {expression} }} fn main() {{ let _ = fail(); }}");
        Package::new(
            &format!("arithmetic-overflow-{index}"),
            &[("main.jk", &source)],
        )
        .check_modes(&[], |mode, output| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{mode}: {output:?}");
            assert!(
                stderr.contains("integer arithmetic overflow"),
                "{mode}: {stderr}"
            );
        });
    }
}

#[test]
fn zero_divisors_panic_in_nonchecked_modes() {
    for (index, operator) in ["/", "%", "/%", "/|", "%%", "%|"].into_iter().enumerate() {
        let source = format!("fn divide(a: Int64, b: Int64) -> Int64 {{ a {operator} b }} fn main() {{ let _ = divide(1, 0); }}");
        Package::new(&format!("arithmetic-zero-{index}"), &[("main.jk", &source)]).check_modes(
            &[],
            |mode, output| {
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(!output.status.success(), "{mode}: {output:?}");
                assert!(stderr.contains("division by zero"), "{mode}: {stderr}");
            },
        );
    }
}
