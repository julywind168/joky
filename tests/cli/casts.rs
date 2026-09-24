use super::parity::Package;

#[test]
fn integer_casts_survive_cache_and_aot() {
    Package::new(
        "integer-casts",
        &[("main.jk", include_str!("../fixtures/casts.jk"))],
    )
    .check_cached("", None, &[]);
}

#[test]
fn checked_casts_preserve_pending_operands_and_propagation() {
    Package::new(
        "integer-casts-pending",
        &[
            (
                "convert.jk",
                r#"
                import joky/time
                fn pending(value: Int64) -> Int64 effects { time } {
                    time.sleep(1ms)
                    value
                }
                pub fn checked(value: Int64) -> Result(Int32, String) effects { time } {
                    let byte = pending(value) as? Int8 ?
                    Ok(byte as Int32)
                }
                pub fn wrapped(value: Int64) -> Int32 effects { time } {
                    pending(value) as% Int8 as Int32
                }
                pub fn saturated(value: Int64) -> Int32 effects { time } {
                    pending(value) as| Int8 as Int32
                }
                "#,
            ),
            (
                "main.jk",
                r#"
                import convert
                import joky/time
                fn main() effects { time } {
                    let good = convert.checked(-128)
                    if good! != -128 { panic("pending minimum") }
                    let bad = convert.checked(-129)
                    if !bad.is_err() { panic("pending underflow") }
                    let wrapped = convert.wrapped(255)
                    if wrapped != -1 { panic("pending wrapping") }
                    if convert.saturated(300) != 127 { panic("pending upper clamp") }
                    if convert.saturated(-200) != -128 { panic("pending lower clamp") }
                    if convert.saturated(42) != 42 { panic("pending interior") }
                }
                "#,
            ),
        ],
    )
    .check_cached("", None, &[]);
}
