//! Execute integer object identity, float components and descriptor text together.
use super::{VmEngine, VmMode};
use crate::{runtime::string::JokyText, FunctionSignature, Type};
extern "C" fn coordinate(token: i32) -> f64 {
    if token == -7 {
        -0.25
    } else {
        0.0
    }
}
extern "C" fn position(token: i32, x: f64, y: f64) -> i32 {
    i32::from(token == -7 && x.to_bits() == (-0.0f64).to_bits() && y == 0.125) * 42
}
extern "C" fn text(_: i32) -> *const u8 {
    thread_local! { static TEXT: JokyText = JokyText::new("中🙂"); }
    TEXT.with(JokyText::as_ptr)
}
unsafe extern "C" fn lookup(token: i32, text: *const u8) -> i32 {
    i32::from(token == -7 && crate::runtime::string::borrow(text).ok() == Some("中🙂")) * 42
}
#[test]
fn mixed_object_host_adapters_preserve_integer_float_and_text_abi() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        // Exact C wire signatures; inputs are not retained, TLS output stays live.
        unsafe {
            vm.register_fn(
                "host_coordinate",
                FunctionSignature::new(vec![Type::Int32], Type::Float64),
                coordinate as *const u8,
            )
            .unwrap();
            vm.register_fn(
                "host_position",
                FunctionSignature::new(
                    vec![Type::Int32, Type::Float64, Type::Float64],
                    Type::Int32,
                ),
                position as *const u8,
            )
            .unwrap();
            vm.register_text_fn(
                "host_text",
                FunctionSignature::new(vec![Type::Int32], Type::String),
                text as *const u8,
            )
            .unwrap();
            vm.register_text_fn(
                "host_lookup",
                FunctionSignature::new(
                    vec![Type::Int32, Type::reference(Type::String, false)],
                    Type::Int32,
                ),
                lookup as *const u8,
            )
            .unwrap();
        }
        vm.load_source(include_str!("../../tests/fixtures/mixed_object_hosts.jk"))
            .unwrap();
        for _ in 0..20 {
            assert_eq!(vm.call_int32_0("check_object_hosts").unwrap(), 42);
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

thread_local! { static CALLS: std::cell::Cell<i32> = const { std::cell::Cell::new(0) }; }
unsafe extern "C" fn text_call(name: *const u8) {
    assert_eq!(crate::runtime::string::borrow(name).unwrap(), "中🙂");
    CALLS.with(|n| n.set(n.get() + 1));
}
unsafe extern "C" fn text_read(name: *const u8) -> f64 {
    text_call(name);
    0.375
}
unsafe extern "C" fn text_write_int(name: *const u8, n: i32) {
    text_call(name);
    assert_eq!(n, -42);
}
unsafe extern "C" fn text_write_float(name: *const u8, n: f64) {
    text_call(name);
    assert_eq!(n.to_bits(), (-0.125f64).to_bits());
}
#[test]
fn text_state_host_adapters_execute_in_direct_and_suspending_functions() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let text = || Type::reference(Type::String, false);
        unsafe {
            for (name, params, ret, entry) in [
                (
                    "text_call",
                    vec![text()],
                    Type::Unit,
                    text_call as *const u8,
                ),
                (
                    "text_read",
                    vec![text()],
                    Type::Float64,
                    text_read as *const u8,
                ),
                (
                    "text_write_int",
                    vec![text(), Type::Int32],
                    Type::Unit,
                    text_write_int as *const u8,
                ),
                (
                    "text_write_float",
                    vec![text(), Type::Float64],
                    Type::Unit,
                    text_write_float as *const u8,
                ),
            ] {
                vm.register_text_fn(name, FunctionSignature::new(params, ret), entry)
                    .unwrap();
            }
        }
        vm.load_source(include_str!("../../tests/fixtures/text_state_hosts.jk"))
            .unwrap();
        CALLS.with(|n| n.set(0));
        assert_eq!(vm.call_int32_0("check_text_hosts").unwrap(), 42);
        let mut task = vm.call_async("check_text_hosts_async", &[]).unwrap();
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        assert_eq!(CALLS.with(std::cell::Cell::get), 8);
    }
}
