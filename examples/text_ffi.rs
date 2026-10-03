//! Run both VM backends, or emit the object and headers for text_ffi.c.
//! cargo run --example text_ffi -- /tmp/joky-text-ffi
use joky::{
    aot::AotCompiler,
    parser,
    runtime::{string::JokyText, text_ffi::*},
    vm::{VmEngine, VmMode},
    FunctionSignature, Type,
};
use std::{
    ffi::{c_char, c_void},
    path::Path,
};

const SOURCE: &str = r#"
extern fn c_echo(text: &String) -> String
fn run() -> String { let text = "Hello, 中🙂"; c_echo(&text) }
"#;

unsafe extern "C" {
    fn strlen(text: *const c_char) -> usize;
}

// The registered input mode guarantees a NUL terminator and valid UTF-8.
// Return a borrowed view of that temporary; the runtime copies it before cleanup.
unsafe extern "C" fn echo(
    _: *mut c_void,
    inputs: *const JokyText,
    _: usize,
    _: *mut u8,
    _: usize,
    output: *mut TextFfiOutputV1,
) -> i32 {
    let input = &*inputs;
    (*output).data = input.data;
    (*output).byte_length = strlen(input.data.cast()) as u64 + 1;
    0
}

fn signature() -> FunctionSignature {
    let ast = parser::parse(SOURCE).unwrap();
    let parser::ast::Item::ExternFn(f) = &ast.items[0] else {
        unreachable!()
    };
    FunctionSignature::new(vec![f.params[0].ty.clone()], Type::String)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_text_ffi_fn(
                "c_echo",
                signature(),
                TextFfiHostV1 {
                    abi_version: TEXT_FFI_ABI_V1,
                    flags: 0,
                    input_count: 1,
                    input_kinds: [TEXT_C_STRING, 0, 0, 0],
                    input_limits: [127, 0, 0, 0],
                    output_kind: TEXT_C_STRING,
                    output_limit: 128,
                    context: std::ptr::null_mut(),
                    call: Some(echo),
                    release: None,
                },
                None,
            )?;
        }
        vm.load_source(SOURCE)?;
        let output = vm.call_string_0("run")?;
        assert_eq!(output, "Hello, 中🙂");
        println!("{mode:?}: {output}");
    }
    if let Some(directory) = std::env::args_os().nth(1) {
        let directory = Path::new(&directory);
        std::fs::create_dir_all(directory)?;
        let mut compiler = AotCompiler::new("text-ffi-example")?;
        compiler.register_text_ffi_fn("c_echo", signature())?;
        compiler.compile_module(&parser::parse(SOURCE)?)?;
        std::fs::write(
            directory.join("text_ffi_exports.h"),
            compiler.text_header_v2(),
        )?;
        std::fs::write(
            directory.join("text_ffi_imports.h"),
            compiler.text_ffi_header_v1(),
        )?;
        compiler.write_object(&directory.join("text_ffi.o"))?;
    }
    Ok(())
}
