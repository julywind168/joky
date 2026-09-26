//! Examples of embedding Joky and using its compiler and runtime APIs.
use joky::{aot::AotCompiler, parser, runtime, vm::VmEngine, FunctionSignature, Type};

// A nonblocking native callback: one pending poll followed by completion.
unsafe extern "C" fn host_yield(frame: *mut runtime::cps::CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        frame.state = 1;
        println!("  [native host] Pending");
        1
    } else {
        println!("  [native host] Completed");
        0
    }
}

fn main() -> Result<(), String> {
    println!("=== Joky embedding examples ===");
    let source = "fn add(x: Int32, y: Int32) -> Int32 { x + y }";
    println!(
        "Parsing\n{:#?}",
        parser::parse(source).map_err(|e| e.to_string())?
    );

    println!("JIT compilation");
    let mut vm = VmEngine::new();
    vm.load_source(
        r#"
        fn add(x: Int32, y: Int32) -> Int32 { x + y }
        fn sub(x: Int32, y: Int32) -> Int32 { x - y }
        fn mul(x: Int32, y: Int32) -> Int32 { x * y }
        fn div(x: Int32, y: Int32) -> Int32 { x / y }
        fn double_add(x: Int32, y: Int32) -> Int32 { add(x, y) + add(x, y) }
        fn compute(x: Int32) -> Int32 { let a = x + 5 a * 2 }
    "#,
    )?;
    assert_eq!(vm.call_int32_2("add", 10, 20)?, 30);
    assert_eq!(vm.call_int32_2("sub", 20, 8)?, 12);
    assert_eq!(vm.call_int32_2("mul", 6, 7)?, 42);
    assert_eq!(vm.call_int32_2("div", 100, 4)?, 25);
    assert_eq!(vm.call_int32_2("double_add", 3, 7)?, 20);
    assert_eq!(vm.call_int32_1("compute", 10)?, 30);

    println!("Interpreter execution");
    let mut interpreter = VmEngine::new_interpreter();
    interpreter.load_source("fn add(x: Int32, y: Int32) -> Int32 { x + y }")?;
    assert_eq!(interpreter.call_int32_2("add", 20, 22)?, 42);

    println!("AOT compilation and linking");
    run_aot_demo(source)?;

    println!("Native host calls and module replacement");
    extern "C" fn multiply(x: i32, y: i32) -> i32 {
        x * y
    }
    unsafe {
        vm.register_fn(
            "multiply",
            FunctionSignature::new(vec![Type::Int32, Type::Int32], Type::Int32),
            multiply as *const u8,
        );
    }
    vm.load_source(
        r#"
        extern fn multiply(x: Int32, y: Int32) -> Int32
        fn compute(x: Int32) -> Int32 { multiply(x + 5, 5) }
    "#,
    )?;
    assert_eq!(vm.call_int32_1("compute", 5)?, 50);
    // Reloading publishes a new module only after the source compiles.
    vm.load_source("fn compute(x: Int32) -> Int32 { x * 10 }")?;
    assert_eq!(vm.call_int32_1("compute", 10)?, 100);

    println!("Nested JIT continuations and native callbacks");
    use runtime::scheduler::Scheduler;
    unsafe {
        vm.register_suspending_fn(
            "host_yield",
            FunctionSignature::new(vec![], Type::Unit),
            host_yield,
        );
    }
    vm.load_source(
        r#"
        extern fn host_yield() @suspends
        fn delayed_add(x: Int32, y: Int32) @suspends -> Int32 {
            host_yield()
            x + y
        }
        fn compute(x: Int32) @suspends -> Int32 {
            let y = delayed_add(x, 20)
            host_yield()
            y + 2
        }
    "#,
    )?;
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("compute", &[20])?);
    assert_eq!(scheduler.suspended_count(), 1);
    scheduler.spawn(|| println!("  Other work runs while the call is pending"));
    scheduler.run_all();
    scheduler
        .resume(id)
        .map_err(|e| format!("First resume failed: {e:?}"))?;
    assert_eq!(scheduler.suspended_count(), 1);
    scheduler
        .resume(id)
        .map_err(|e| format!("Second resume failed: {e:?}"))?;
    assert_eq!(scheduler.suspended_count(), 0);
    let task = scheduler
        .take_completed(id)
        .map_err(|e| format!("Completion failed: {e:?}"))?;
    assert_eq!(task.cps_result(), Some(42));
    println!("=== Joky embedding examples completed ===");
    Ok(())
}

fn run_aot_demo(source: &str) -> Result<(), String> {
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("joky-aot-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let object = dir.join("demo.o");
    let driver = dir.join("driver.c");
    let executable = dir.join("demo");
    let mut aot = AotCompiler::new("demo")?;
    aot.compile_module(&parser::parse(source).map_err(|e| e.to_string())?)?;
    aot.write_object(&object)?;
    std::fs::write(
        &driver,
        r#"
        #include <stdint.h>
        extern int32_t add(int32_t, int32_t);
        int main(void) { return add(10, 20) == 30 ? 0 : 1; }
    "#,
    )
    .map_err(|e| e.to_string())?;
    let link = Command::new("cc")
        .arg(&driver)
        .arg(&object)
        .arg("-o")
        .arg(&executable)
        .output()
        .map_err(|e| e.to_string())?;
    if !link.status.success() {
        return Err(String::from_utf8_lossy(&link.stderr).into_owned());
    }
    let result = Command::new(&executable)
        .status()
        .map_err(|e| e.to_string())?;
    if !result.success() {
        return Err(format!("AOT executable failed: {result}"));
    }
    Ok(())
}
