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
