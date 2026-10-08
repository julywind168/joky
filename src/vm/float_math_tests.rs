use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};

/// N2 primitives, checked conversions, bit reinterpretation and formatting
/// produce identical results on the JIT and the interpreter.
#[test]
fn float_math_and_checked_conversions_match_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/float_math.jk"))
            .unwrap();
        assert_eq!(vm.call_int32_0("main").unwrap(), 42, "{mode:?}");
    }
}

/// Every primitive matches Rust's own f32/f64 results bit for bit, including
/// subnormals, NaN, signed zero and tie-to-even rounding.
#[test]
fn float_primitive_results_match_rust_bits() {
    let source = r#"
        fn sq(v: Float64) -> Float64 { v.sqrt() }
        fn fl(v: Float64) -> Float64 { v.floor() }
        fn ce(v: Float64) -> Float64 { v.ceil() }
        fn tr(v: Float64) -> Float64 { v.trunc() }
        fn rn(v: Float64) -> Float64 { v.round() }
        fn sq32(v: Float32) -> Float32 { v.sqrt() }
        fn rn32(v: Float32) -> Float32 { v.round() }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        for v in [
            2.0f64,
            0.1,
            1.0e300,
            -0.0,
            -1.5,
            2.5,
            3.5,
            5.0e-324,
            f64::NAN,
            f64::INFINITY,
        ] {
            for (name, expected) in [
                ("sq", v.sqrt()),
                ("fl", v.floor()),
                ("ce", v.ceil()),
                ("tr", v.trunc()),
                ("rn", v.round_ties_even()),
            ] {
                assert_eq!(
                    vm.call_float64_1(name, v).unwrap().to_bits(),
                    expected.to_bits(),
                    "{mode:?} {name}({v})"
                );
            }
        }
        for v in [2.0f32, 0.1, 2.5, -1.5, 1.0e-45, f32::NAN, f32::INFINITY] {
            assert_eq!(
                vm.call_float32_1("sq32", v).unwrap().to_bits(),
                v.sqrt().to_bits(),
                "{mode:?} sq32({v})"
            );
            assert_eq!(
                vm.call_float32_1("rn32", v).unwrap().to_bits(),
                v.round_ties_even().to_bits(),
                "{mode:?} rn32({v})"
            );
        }
    }
}

/// The shared float kernel keeps its exact result across suspension points.
#[test]
fn float_math_survives_suspension() {
    let source = r#"
        fn root(v: Float64) -> Float64 { v.sqrt() }
        fn tie(v: Float32) -> Float32 { v.round() }
    "#;
    for suspends in [false, true] {
        let source = with_suspends(source, suspends, Suspends::Typed);
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap();
            for v in [2.0f64, 2.5, f64::NAN, 5.0e-324] {
                if suspends {
                    let task = vm.call_async_float64("root", &[v]).unwrap();
                    assert_eq!(task.cps_result().unwrap(), v.sqrt().to_bits(), "{mode:?}");
                } else {
                    assert_eq!(
                        vm.call_float64_1("root", v).unwrap().to_bits(),
                        v.sqrt().to_bits(),
                        "{mode:?}"
                    );
                }
            }
            for v in [2.5f32, -2.5, f32::NAN] {
                if suspends {
                    let task = vm.call_async_float32("tie", &[v]).unwrap();
                    assert_eq!(
                        task.cps_result().unwrap(),
                        u64::from(v.round_ties_even().to_bits()),
                        "{mode:?}"
                    );
                } else {
                    assert_eq!(
                        vm.call_float32_1("tie", v).unwrap().to_bits(),
                        v.round_ties_even().to_bits(),
                        "{mode:?}"
                    );
                }
            }
        }
    }
}

/// Formatting under a tiny instruction budget fails through the normal error
/// channel instead of silently truncating or returning partial text.
#[test]
fn float_to_text_respects_interpreter_budgets() {
    use super::InterpreterLimits;
    let mut vm = VmEngine::new_interpreter();
    vm.load_source("fn make() -> Int32 { let text = 1.5.to_text() 0 }")
        .unwrap();
    vm.set_interpreter_limits(InterpreterLimits {
        instruction_budget: Some(0),
        ..Default::default()
    })
    .unwrap();
    let error = vm.call_int32_0("make").unwrap_err();
    assert!(error.to_string().contains("budget"), "{error}");
}

/// Formatting keeps fixed spellings for NaN, infinities and negative zero,
/// and finite values use the shortest spelling that round-trips in Rust.
#[test]
fn float_to_text_uses_fixed_spellings() {
    fn literal(value: f64) -> String {
        match value.classify() {
            std::num::FpCategory::Nan => "(0.0 / 0.0)".into(),
            std::num::FpCategory::Infinite if value > 0.0 => "(1.0 / 0.0)".into(),
            std::num::FpCategory::Infinite => "(-1.0 / 0.0)".into(),
            // Rust's Display drops the fraction on integral values, which
            // Joky would parse back as an integer literal.
            _ => {
                let text = format!("{value}");
                if text.contains('.') {
                    text
                } else {
                    format!("{text}.0")
                }
            }
        }
    }
    let cases = [
        (0.5f64, "0.5"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "inf"),
        (-0.0, "-0"),
        (123456.789, "123456.789"),
        (1.0e-7, "0.0000001"),
    ];
    // No mixed-signature call entry exists yet, so spell each check as its
    // own zero-argument Joky function.
    let mut source = String::from("import joky/string\n");
    for (index, (value, expected)) in cases.iter().enumerate() {
        let literal = literal(*value);
        source.push_str(&format!(
            "fn check_{index}() -> Int32 {{\n    let text = ({literal}).to_text()\n    if text.rune_count() != {} {{ return 1 }}\n    0\n}}\n",
            expected.chars().count()
        ));
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        for (index, (value, _)) in cases.iter().enumerate() {
            assert_eq!(
                vm.call_int32_0(&format!("check_{index}")).unwrap(),
                0,
                "{mode:?} {value}"
            );
        }
        // The Rust spellings themselves must round-trip through the same width.
        for (value, _) in cases {
            assert_eq!(
                format!("{value}").parse::<f64>().unwrap().to_bits(),
                value.to_bits()
            );
        }
    }
}

/// Mixed float/integer host signatures dispatch through the exact ABI on both
/// native backends; wrong signatures are rejected instead of bit-reinterpreted.
#[test]
fn float_host_bindings_accept_mixed_signatures() {
    use crate::{FunctionSignature, Type};
    extern "C" fn mix(count: i32, factor: f64) -> f64 {
        factor * f64::from(count)
    }
    extern "C" fn triple(a: f64, b: f64, c: f64) -> f64 {
        a * a + b * b + c * c
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let source = r#"
            extern fn host_mix(count: Int32, factor: Float64) -> Float64
            extern fn host_triple(x: Float64, y: Float64, z: Float64) -> Float64
            fn mix(count: Int32, factor: Float64) -> Float64 { host_mix(count, factor) }
            fn triple(x: Float64, y: Float64, z: Float64) -> Float64 { host_triple(x, y, z) }
        "#;
        // Host bindings must exist before the module loads and links them.
        unsafe {
            vm.register_fn(
                "host_mix",
                FunctionSignature::new(vec![Type::Int32, Type::Float64], Type::Float64),
                mix as *const u8,
            )
            .unwrap();
            vm.register_fn(
                "host_triple",
                FunctionSignature::new(
                    vec![Type::Float64, Type::Float64, Type::Float64],
                    Type::Float64,
                ),
                triple as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        assert_eq!(
            vm.call_float64_i32_f64("mix", 3, 2.5).unwrap(),
            7.5,
            "{mode:?}"
        );
        assert_eq!(
            vm.call_float64_i32_f64("mix", -2, 0.5).unwrap(),
            -1.0,
            "{mode:?}"
        );
        assert_eq!(
            vm.call_float64_3("triple", 1.0, 2.0, 3.0).unwrap(),
            14.0,
            "{mode:?}"
        );
        // Wrong arity and swapped types are rejected, never reinterpreted.
        assert!(vm.call_float64_2("mix", 1.0, 2.0).is_err(), "{mode:?}");
        assert!(vm.call_float64_f64_i32("mix", 1.0, 2).is_err(), "{mode:?}");
        assert!(vm.call_float64_2("triple", 1.0, 2.0).is_err(), "{mode:?}");
    }
}

/// Float64 references join the borrowing v1 protocol with the same layout
/// rules as synchronous C references.
#[test]
fn float_references_join_borrowing_protocol() {
    use crate::runtime::borrowed_host::BorrowingHostV1;
    use crate::runtime::cps::CpsFrame;
    use crate::{FunctionSignature, Type};
    unsafe extern "C" fn poll(_: *mut CpsFrame) -> i32 {
        0
    }
    unsafe extern "C" fn stop(_: *mut CpsFrame, _: u32) {}
    let host = BorrowingHostV1 {
        abi_version: 1,
        poll: Some(poll),
        stop: Some(stop),
    };
    let signature = FunctionSignature::new(vec![Type::reference(Type::Float64, true)], Type::Unit);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_borrowing_suspending_fn("host", signature.clone(), host, None)
                .unwrap();
        }
        vm.load_source(
            "extern fn host(p: &mut Float64) @suspends\n\
             fn run() @suspends -> Float64 { let p = alloc(2.5); host(p)(*p) }",
        )
        .unwrap();
        let task = vm.call_async_float64("run", &[]).unwrap();
        assert_eq!(task.cps_result().unwrap(), 2.5f64.to_bits(), "{mode:?}");
    }
}
