use super::support::{with_suspends, Suspends};
use super::{VmEngine, VmMode};
use crate::ErrorPhase;

mod environment {
    use super::*;
    use crate::{
        runtime::{
            cps::{CpsFrame, DONE},
            float_env::{testing, Guard},
        },
        FunctionSignature, Type,
    };
    use std::cell::{Cell, RefCell};

    thread_local! {
        static VM: Cell<*const VmEngine> = const { Cell::new(std::ptr::null()) };
        static OBSERVED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
        static STOPS: Cell<usize> = const { Cell::new(0) };
    }
    unsafe extern "C" fn dirty() {
        testing::hostile();
        let saved = testing::state();
        VM.with(|v| {
            if let Some(vm) = v.get().as_ref() {
                assert_eq!(
                    vm.call_float64_0("folded64").unwrap().to_bits(),
                    1.0f64.to_bits()
                );
                assert_eq!(testing::state(), saved, "reentrant call restores its host");
            }
        });
    }
    extern "C" fn observe(value: f64) {
        OBSERVED.with(|v| v.borrow_mut().push(value.to_bits()));
    }
    unsafe extern "C" fn pause_poll(frame: *mut CpsFrame) -> i32 {
        testing::hostile();
        if (*frame).state == 0 {
            (*frame).state = 1;
            1
        } else {
            (*frame).state = DONE;
            0
        }
    }
    unsafe extern "C" fn pause_stop(_: *mut CpsFrame, _: u32) {
        dirty();
        STOPS.with(|v| v.set(v.get() + 1));
    }
    unsafe extern "C" fn pause(frame: *mut CpsFrame) -> i32 {
        use crate::runtime::borrowed_host::{joky_cps_poll_borrowing_v1, BorrowingHostV1};
        static OPERATION: BorrowingHostV1 = BorrowingHostV1 {
            abi_version: 1,
            poll: Some(pause_poll),
            stop: Some(pause_stop),
        };
        joky_cps_poll_borrowing_v1(frame, &OPERATION)
    }

    #[test]
    fn float_environment_isolated_across_compilation_calls_pending_and_cleanup() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let _restore = Guard::enter();
            testing::hostile();
            let saved = testing::state();
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_fn(
                    "dirty",
                    FunctionSignature::new(vec![], Type::Unit),
                    dirty as *const u8,
                )
                .unwrap();
                vm.register_fn(
                    "observe",
                    FunctionSignature::new(vec![Type::Float64], Type::Unit),
                    observe as *const u8,
                )
                .unwrap();
                vm.register_suspending_fn(
                    "pause",
                    FunctionSignature::new(vec![], Type::Unit),
                    pause,
                )
                .unwrap();
            }
            vm.load_source(include_str!("../../tests/fixtures/float_environment.jk"))
                .unwrap_or_else(|e| panic!("{}", e.message()));
            assert_eq!(testing::state(), saved, "{mode:?} compile");
            VM.with(|v| v.set(&vm));
            assert_eq!(
                vm.call_float64_0("folded64").unwrap().to_bits(),
                0x3ff0000000000000
            );
            assert_eq!(vm.call_float32_0("folded32").unwrap().to_bits(), 0x3f800000);
            assert_eq!(
                vm.call_float64_0("folded_tiny").unwrap().to_bits(),
                0x8000000000000
            );
            for name in ["add64", "indirect"] {
                assert_eq!(
                    vm.call_float64_2(name, 1.0, f64::from_bits(0x3ca0000000000000))
                        .unwrap()
                        .to_bits(),
                    0x3ff0000000000000,
                    "{mode:?} {name}"
                );
                assert_eq!(testing::state(), saved);
            }
            assert_eq!(
                vm.call_float32_2("add32", 1.0, f32::from_bits(0x33800000))
                    .unwrap()
                    .to_bits(),
                0x3f800000
            );
            for (input, expected) in [(f64::MIN_POSITIVE, 0x8000000000000), (f64::from_bits(2), 1)]
            {
                assert_eq!(
                    vm.call_float64_1("half", input).unwrap().to_bits(),
                    expected
                );
                assert_eq!(testing::state(), saved);
            }
            assert_eq!(
                vm.call_float32_1("half32", f32::MIN_POSITIVE)
                    .unwrap()
                    .to_bits(),
                0x400000
            );
            assert_eq!(vm.call_int32_0("integer_result").unwrap(), 1);
            assert!(vm.call_float64_1("fail", 1.0).is_err());
            assert_eq!(testing::state(), saved, "{mode:?} error");

            for cancel in [false, true] {
                OBSERVED.with(|v| v.borrow_mut().clear());
                STOPS.with(|v| v.set(0));
                let mut task = vm
                    .call_async_float64("flowing", &[f64::MIN_POSITIVE, 0.0])
                    .unwrap();
                assert!(!task.is_finished());
                assert_eq!(testing::state(), saved, "{mode:?} Pending");
                if !cancel {
                    task.resume().unwrap();
                    assert_eq!(task.cps_result(), Some(f64::MIN_POSITIVE.to_bits()));
                    assert_eq!(testing::state(), saved, "{mode:?} resume");
                }
                drop(task);
                let expected = if cancel {
                    vec![]
                } else {
                    vec![0x8000000000000]
                };
                assert_eq!(OBSERVED.with(|v| v.borrow().clone()), expected);
                assert_eq!(STOPS.with(Cell::get), 1);
                assert_eq!(testing::state(), saved, "{mode:?} cleanup");
            }
            OBSERVED.with(|v| v.borrow_mut().clear());
            let id = vm.create_instance("create", &[]).unwrap();
            let super::super::InstanceValue::Float64(value) =
                vm.call_instance(id, "update", &[]).unwrap()
            else {
                panic!("Float64 state result");
            };
            assert_eq!(value.to_bits(), 0x8000000000000);
            vm.close_instance(id).unwrap();
            assert_eq!(OBSERVED.with(|v| v.borrow().clone()), [0x4000000000000]);
            assert_eq!(testing::state(), saved, "{mode:?} instance");
            VM.with(|v| v.set(std::ptr::null()));
        }
    }
}

/// Float32/Float64 literals, arithmetic, comparison, conversion and storage
/// must agree across the JIT and interpreter, with and without suspension.
#[test]
fn float_arithmetic_comparison_and_conversion_match_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(
                include_str!("../../tests/fixtures/float.jk"),
                suspends,
                Suspends::Typed,
            );
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap();
            let check = |vm: &mut VmEngine, name: &str| -> i32 {
                if suspends {
                    vm.call_async(name, &[]).unwrap().cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                }
            };
            assert_eq!(check(&mut vm, "check"), 42, "{mode:?} suspends={suspends}");
            assert_eq!(check(&mut vm, "check_aggregate"), 0, "{mode:?} {suspends}");
        }
    }
}

/// Float elements and Map values keep their exact bit pattern; signed zero is
/// observable through division. Container borrows rule out the `@suspends` rewrite.
#[test]
fn float_container_storage_matches_backends() {
    let source = include_str!("../../tests/fixtures/float_storage.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 0, "{mode:?}");
    }
}

/// A Float32 result crossing a suspension must stay a Float32, not an integer.
#[test]
fn float_precision_survives_suspension() {
    let source = r#"
        fn halve(v: Float64) @suspends -> Float64 { v * 0.5 }
        fn bump(v: Float32) @suspends -> Float32 { v + 0.25 }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        for v in [3.0f64, -0.0, 1.0e300, 5.0e-324] {
            let task = vm.call_async_float64("halve", &[v]).unwrap();
            assert_eq!(task.cps_result().unwrap(), (v * 0.5).to_bits(), "{mode:?}");
        }
        for v in [1.0f32, -0.0, 1.0e-45] {
            let task = vm.call_async_float32("bump", &[v]).unwrap();
            // The CPS result is the zero-extended 32-bit pattern.
            assert_eq!(
                task.cps_result().unwrap(),
                u64::from((v + 0.25).to_bits()),
                "{mode:?}"
            );
        }
    }
}

#[test]
fn float_arithmetic_bit_patterns_match_rust() {
    let source = include_str!("../../tests/fixtures/float.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        // Small finite cases exercise rounding to nearest-even at both widths.
        for (a, b) in [
            (0.0f64, 0.0),
            (1.0, 3.0),
            (-1.0, 3.0),
            (1.0e300, 1.0e300),
            (0.1, 0.2),
            (-0.0, 0.0),
            (5.0e-324, 2.0),
        ] {
            for (name, expected) in [
                ("add64", a + b),
                ("sub64", a - b),
                ("mul64", a * b),
                ("div64", a / b),
            ] {
                assert_eq!(
                    vm.call_float64_2(name, a, b).unwrap().to_bits(),
                    expected.to_bits(),
                    "{mode:?} {name}({a}, {b})"
                );
            }
        }
        for a in [1.0f64, -0.0, 5.0e-324, -1.0e300] {
            assert_eq!(
                vm.call_float64_1("neg64", a).unwrap().to_bits(),
                (-a).to_bits(),
                "{mode:?} neg64({a})"
            );
        }
        for (a, b) in [(1.5f32, 2.0), (-1.5, 2.0), (0.1, 0.2), (1.0e30, 1.0e30)] {
            assert_eq!(
                vm.call_float32_2("add32", a, b).unwrap().to_bits(),
                (a + b).to_bits(),
                "{mode:?} add32({a}, {b})"
            );
            assert_eq!(
                vm.call_float32_2("mul32", a, b).unwrap().to_bits(),
                (a * b).to_bits(),
                "{mode:?} mul32({a}, {b})"
            );
        }
        for a in [1.5f32, -0.0, 1.0e-45] {
            assert_eq!(
                vm.call_float32_1("neg32", a).unwrap().to_bits(),
                (-a).to_bits(),
                "{mode:?} neg32({a})"
            );
        }
        // Integer and width conversions are covered by `conversions()` in the fixture.
    }
}

/// Every scalar call entry rejects a mismatched signature instead of
/// reinterpreting the Float bits as an integer.
#[test]
fn float_call_entries_validate_signatures() {
    let source = include_str!("../../tests/fixtures/float.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42, "{mode:?}");
        assert!(vm.call_int32_0("add64").is_err(), "{mode:?}");
        assert!(vm.call_float64_2("check", 1.0, 2.0).is_err(), "{mode:?}");
        assert!(vm.call_uint64_0("check").is_err(), "{mode:?}");
    }
}

/// Float literals outside the target finite range must fail at compile time,
/// not silently become infinity.
#[test]
fn float_literals_outside_finite_range_are_rejected() {
    for (source, message) in [
        (
            "fn main() -> Int32 { let x: Float32 = 1.0e39 0 }",
            "finite range",
        ),
        (
            "fn main() -> Int32 { let x: Float64 = 1.0e309 0 }",
            "finite range",
        ),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let error = vm.load_source(source).unwrap_err();
            assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{mode:?} {source}");
            assert!(error.to_string().contains(message), "{mode:?} {error}");
        }
    }
}

/// Mixed-width arithmetic and non-arithmetic operators on floats are rejected.
#[test]
fn invalid_float_operations_are_rejected() {
    for source in [
        "fn main() { let x = float32(1.0) + float64(2.0) }",
        "fn main() { let x = 1.0 & 2.0 }",
        "fn main() { let x = 1.0 << 2 }",
        "fn main() { let x = ~1.0 }",
        "fn main() { let x = !1.0 }",
        "fn main() { let x: Int32 = 1 let y: Float64 = 1.0 + x }",
    ] {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        assert!(vm.load_source(source).is_err(), "{source}");
    }
}

/// Constant expressions fold at the target precision, so a Float32 fold must
/// not round through Float64 first.
#[test]
fn float_constant_folding_uses_target_precision() {
    let source = r#"
        fn w() -> Float64 { 0.1 + 0.2 }
        fn n() -> Float32 { 0.1 + 0.2 }
        fn l() -> Float32 { 1.5e-3 }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(
            vm.call_float64_0("w").unwrap().to_bits(),
            (0.1f64 + 0.2f64).to_bits(),
            "{mode:?}"
        );
        assert_eq!(
            vm.call_float32_0("n").unwrap().to_bits(),
            (0.1f32 + 0.2f32).to_bits(),
            "{mode:?}"
        );
        assert_eq!(
            vm.call_float32_0("l").unwrap().to_bits(),
            1.5e-3f32.to_bits(),
            "{mode:?}"
        );
    }
}
