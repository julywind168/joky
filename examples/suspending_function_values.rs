//! Run with: cargo run --example suspending_function_values
use joky::{
    runtime::cps::CpsFrame,
    vm::{VmEngine, VmMode},
    FunctionSignature, Type,
};

// Each invocation yields twice before publishing its result. The frame belongs
// to the runtime; the callback does not retain any frame or slot pointer.
unsafe extern "C" fn pause(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state < 2 {
        frame.state += 1;
        return 1;
    }
    frame.result = frame.slots[0];
    0
}

const SOURCE: &str = r#"
    extern fn pause(x: Int32) @suspends -> Int32
    fn increment(x: Int32) @suspends -> Int32 { pause(x) + 1 }
    fn apply<T, U>(value: T, transform: fn(T) @suspends -> U) @suspends -> U {
        transform(value)
    }
    fn compute() @suspends -> Int32 {
        let transform: fn(Int32) @suspends -> Int32 = increment
        apply(41, transform)
    }
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        // The callback has the declared CPS signature and static code lifetime.
        unsafe {
            vm.register_suspending_fn(
                "pause",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                pause,
            )?;
        }
        vm.load_source(SOURCE)?;
        let mut task = vm.call_async("compute", &[])?;
        assert!(task.cps_result().is_none());
        // A pending indirect call keeps its code and descriptor alive.
        vm.load_source("fn replacement() -> Int32 { 0 }")?;
        drop(vm);
        while !task.is_finished() {
            task.resume()
                .map_err(|e| std::io::Error::other(format!("Resume failed: {e:?}")))?;
        }
        assert_eq!(task.cps_result(), Some(42));
        println!("{mode:?}: 42");
    }
    Ok(())
}
