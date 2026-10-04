use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, Type};
use std::cell::Cell;

const SOURCE: &str = include_str!("../../tests/fixtures/uint8.jk");
extern "C" fn host_byte(value: u8) -> u8 {
    value.wrapping_add(1)
}

#[test]
fn uint8_layout_storage_conversions_and_host_calls() {
    let layout = Type::UInt8.memory_layout().unwrap();
    assert_eq!((layout.size, layout.align), (1, 1));
    let pair = Type::tuple(vec![Type::UInt8, Type::UInt8]);
    assert_eq!(pair.memory_layout().unwrap().size, 2);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        // SAFETY: The callback uses exactly the registered u8 -> u8 ABI.
        unsafe {
            vm.register_fn(
                "host_byte",
                FunctionSignature::new(vec![Type::UInt8], Type::UInt8),
                host_byte as *const u8,
            )
            .unwrap();
        }
        vm.load_source(&format!("{SOURCE}\nextern fn host_byte(value: UInt8) -> UInt8\nfn host(value: UInt8) -> UInt8 {{ host_byte(value) }}\nfn task(value: UInt8) @suspends -> UInt8 {{ host_byte(value) }}")).unwrap();
        assert_eq!(vm.call_int32_0("byte_check").unwrap(), 42, "{mode:?}");
        for byte in [0, 127, 128, 254, 255] {
            assert_eq!(vm.call_uint8_1("host", byte).unwrap(), byte.wrapping_add(1));
            assert_eq!(
                vm.call_async_uint8("task", &[byte]).unwrap().cps_result(),
                Some(u64::from(byte.wrapping_add(1)))
            );
        }
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}
#[test]
fn uint8_array_budget_failures_clean_storage() {
    let _restore = RestoreLimit(arena::LIMIT.with(Cell::get));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let mut failed = false;
        let mut completed = false;
        for budget in (0..16384).step_by(32) {
            arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("byte_check");
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
            arena::LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?} budget={budget}"));
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                    break;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit, "{error}");
                    failed = true;
                }
            }
        }
        assert!(failed && completed, "{mode:?}");
    }
}

#[test]
fn uint8_rejects_out_of_range_literals_mixed_types_and_c_record_fields() {
    for source in [
        "fn f() -> UInt8 { 256 }",
        "fn f() -> UInt8 { -1 }",
        "fn f(a: UInt8, b: UInt32) -> UInt8 { a + b }",
        "fn f(a: UInt8) -> UInt8 { -a }",
        "type UInt8 = Int32",
        "fn uint8(a: Int32) -> Int32 { a }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(vm.load_source(source).is_err(), "{mode:?}: {source}");
        }
        let mut aot = crate::aot::AotCompiler::new("byte_rejection").unwrap();
        if let Ok(ast) = crate::parser::parse(source) {
            assert!(aot.compile_module(&ast).is_err(), "{source}");
        }
    }
}

thread_local! { static POLLS: Cell<usize> = const { Cell::new(0) }; }
unsafe extern "C" fn delay_byte(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    POLLS.with(|polls| polls.set(polls.get() + 1));
    if frame.state < 3 {
        frame.state += 1;
        return 1;
    }
    frame.result = u64::from((frame.slots[0] as u8).wrapping_add(1));
    0
}
#[test]
fn uint8_pending_survives_reload_drop_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        // SAFETY: The callback uses the registered CPS scalar-slot ABI.
        unsafe {
            vm.register_suspending_fn(
                "delay_byte",
                FunctionSignature::new(vec![Type::UInt8], Type::UInt8),
                delay_byte,
            )
            .unwrap();
        }
        vm.load_source("extern fn delay_byte(value: UInt8) @suspends -> UInt8 fn task(value: UInt8) @suspends -> UInt8 { let saved = value let next = delay_byte(value) saved + next }").unwrap();
        POLLS.with(|polls| polls.set(0));
        let mut task = vm.call_async_uint8("task", &[128]).unwrap();
        let cancelled = vm.call_async_uint8("task", &[255]).unwrap();
        drop(cancelled);
        vm.load_source("fn task(value: UInt8) @suspends -> UInt8 { value }")
            .unwrap();
        assert_eq!(
            vm.call_async_uint8("task", &[255]).unwrap().cps_result(),
            Some(255)
        );
        drop(vm);
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(1));
        POLLS.with(|polls| assert_eq!(polls.get(), 5));
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn rune_literals_validate_scalars_and_preserve_scalar_identity() {
    let source = r#"
        import joky/check.{require}
        fn rune_check() -> UInt32 {
            let ascii = 'a'
            let chinese = '中'
            let emoji = '\u{1F642}'
            { let assertion_condition = ascii == 'a' let assertion_message = "ASCII rune" require(assertion_condition, &assertion_message) }
            { let assertion_condition = Rune.from_u32(0).is_some() let assertion_message = "NUL rune" require(assertion_condition, &assertion_message) }
            { let assertion_condition = Rune.from_u32(0x10ffff).is_some() let assertion_message = "Upper scalar" require(assertion_condition, &assertion_message) }
            { let assertion_condition = Rune.from_u32(0xd800).is_none() let assertion_message = "Surrogate rejected" require(assertion_condition, &assertion_message) }
            { let assertion_condition = Rune.from_u32(0x110000).is_none() let assertion_message = "Out of range rejected" require(assertion_condition, &assertion_message) }
            { let assertion_condition = ascii.to_u32() == 97 let assertion_message = "Rune conversion" require(assertion_condition, &assertion_message) }
            { let assertion_condition = chinese == '中' let assertion_message = "Unicode rune" require(assertion_condition, &assertion_message) }
            { let assertion_condition = emoji == '\u{1F642}' let assertion_message = "Emoji rune" require(assertion_condition, &assertion_message) }
            if ascii == 'a' { 97 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_uint32_0("rune_check").unwrap(), 97, "{mode:?}");
    }
    for source in [
        "fn bad() -> Rune { '' }",
        "fn bad() -> Rune { 'ab' }",
        "fn bad() -> Rune { '\\u{D800}' }",
        "fn bad() -> Rune { '\\u{110000}' }",
        "fn bad() -> Rune { '\\q' }",
        "fn bad() -> Rune { '中 }",
    ] {
        assert!(
            VmEngine::with_mode(VmMode::Jit)
                .load_source(source)
                .is_err(),
            "{source}"
        );
        assert!(
            VmEngine::with_mode(VmMode::Interpreter)
                .load_source(source)
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn rune_rejects_integer_forging_and_unchecked_host_boundaries() {
    for source in [
        "fn bad() -> Rune { 0xd800 }",
        "fn bad() -> Rune { rune(0xd800) }",
        "fn bad(value: UInt32) -> Rune { rune(value) }",
        "extern fn bad() -> Rune fn check() -> Rune { bad() }",
        "extern fn bad(value: &mut Rune) fn check(value: &mut Rune) { bad(value) }",
        "#repr(C) struct Bad { value: Rune }",
        "import joky/integer.{rune_from_valid_u32} fn bad() -> Rune { rune_from_valid_u32(0xd800) }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(source).is_err(), "{mode:?}: {source}");
        }
        let mut aot = crate::aot::AotCompiler::new("rune_rejection").unwrap();
        if let Ok(ast) = crate::parser::parse(source) {
            assert!(aot.compile_module(&ast).is_err(), "{source}");
        }
    }
}

#[test]
fn rune_storage_constants_and_pending_values_preserve_valid_scalars() {
    let layout = Type::Rune.memory_layout().unwrap();
    assert_eq!(
        (layout.size, layout.align),
        (4, std::mem::align_of::<i32>())
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&format!("{}\nfn pending() @suspends -> Int32 {{ let scalar = task('🙂') if scalar == '🙂' {{ 42 }} else {{ 0 }} }}",
            include_str!("../../tests/fixtures/rune.jk"))).unwrap();
        assert_eq!(vm.call_int32_0("rune_check").unwrap(), 42, "{mode:?}");
        assert_eq!(
            vm.call_async("pending", &[]).unwrap().cps_result(),
            Some(42)
        );
        // SAFETY: Registration must reject Rune before accepting this pointer.
        let result = unsafe {
            vm.register_fn(
                "unchecked_rune",
                FunctionSignature::new(vec![Type::Rune], Type::Rune),
                host_byte as *const u8,
            )
        };
        assert_eq!(result.unwrap_err().code(), ErrorCode::AbiError);
    }
}
