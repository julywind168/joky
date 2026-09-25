use super::parity::Package;
use std::process::Command;

#[test]
fn integer_suffixes_survive_cache_and_aot() {
    Package::new(
        "integer-suffixes",
        &[("main.jk", include_str!("../fixtures/integer_suffixes.jk"))],
    )
    .check_cached("", None, &[]);
}

#[test]
fn integer_suffixes_preserve_exported_constants_defaults_and_templates() {
    Package::new(
        "integer-suffixes-modules",
        &[
            (
                "values.jk",
                r#"
            pub const MIN = -128i8
            pub const MAX = 18446744073709551615u64
            pub const SUM = 1u8 + 2u8
            pub const PAIR = (-9223372036854775808i64, 0xffu8)
            struct Header { let tag: UInt8 = 0xffu8 }
            pub fn marker(T: type, value: T) -> UInt16 { let _ = value; 123u16 }
        "#,
            ),
            (
                "main.jk",
                r#"
            import values
            fn main() {
                if values.MIN != -128i8 { panic("imported minimum") }
                if values.MAX != 0xffff_ffff_ffff_ffffu64 { panic("imported maximum") }
                if values.SUM != 3u8 { panic("imported computation") }
                if values.PAIR != (-9223372036854775808i64, 255u8) { panic("imported tuple") }
                if values.Header().tag != 255u8 { panic("imported default") }
                if values.marker(UInt8, 1u8) != 123u16 { panic("imported template") }
            }
        "#,
            ),
        ],
    )
    .check_cached("", None, &[]);
}

#[test]
fn integer_suffixes_report_errors_during_cli_check() {
    for (index, (source, diagnostic)) in [
        ("fn main() { let x = 256u8; () }", "out of range"),
        ("fn main() { let x = -129i8; () }", "out of range"),
        ("fn main() { let x = -1u8; () }", "cannot negate unsigned"),
        (
            "fn main() { let x: UInt64 = 1u8; () }",
            "expected UInt64, found UInt8",
        ),
        ("fn main() { let x = 1u128; () }", "invalid numeric suffix"),
        ("fn main() { let x = 1.5u8; () }", "invalid numeric suffix"),
        ("fn main() { let x = 1u8u16; () }", "invalid numeric suffix"),
        (
            "fn main() { let x = 0b102u8; () }",
            "invalid numeric literal",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let package = Package::new(
            &format!("integer-suffixes-error-{index}"),
            &[("main.jk", source)],
        );
        let output = Command::new(env!("CARGO_BIN_EXE_joky"))
            .current_dir(&package.root)
            .args(["check", "--no-cache"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{source} was accepted");
        assert!(stderr.contains(diagnostic), "{source}: {stderr}");
    }
}

#[test]
fn integer_suffixes_keep_runtime_overflow_checks() {
    for (index, expression) in ["-(-128i8)", "127i8 + 1"].into_iter().enumerate() {
        let source = format!("fn main() {{ let value = {expression}; println(value); }}");
        Package::new(
            &format!("integer-suffixes-overflow-{index}"),
            &[("main.jk", &source)],
        )
        .check_modes(&[], |mode, output| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{mode}: {expression} was accepted"
            );
            assert!(
                stderr.contains("integer arithmetic overflow"),
                "{mode}: {stderr}"
            );
        });
    }
}
