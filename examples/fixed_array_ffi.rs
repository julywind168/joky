//! Execute both VM modes; optionally emit fixed_array.o and bindings.h for fixed_array_ffi.c.
//! cargo run --example fixed_array_ffi -- /tmp/joky-fixed-array
use joky::{
    aot::AotCompiler,
    parser::{
        self,
        ast::{ConstArgument, FixedArrayType},
    },
    vm::{VmEngine, VmMode},
    FunctionSignature, Type,
};
use std::{path::Path, sync::Arc};
const SOURCE: &str = include_str!("fixed_array_ffi.jk");

// The unsafe registration guarantees aligned, live array/length references for
// this synchronous call. Neither entry retains pointers or reenters the VM.
unsafe extern "C" fn read(values: *const [i32; 4], length: *const u64) -> i32 {
    let Ok(length) = usize::try_from(*length) else {
        return -1;
    };
    if length > 4 {
        return -1;
    }
    (&*values)[..length].iter().sum()
}
unsafe extern "C" fn write(values: *mut [i32; 4]) -> i32 {
    (*values)[0] = 8;
    -1
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let row = Type::FixedArray(Arc::new(FixedArrayType {
        element: Type::Int32,
        length: ConstArgument::Value(4),
    }));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_c_fn(
                "joky_example_read",
                FunctionSignature::new(
                    vec![
                        Type::reference(row.clone(), false),
                        Type::reference(Type::UInt64, false),
                    ],
                    Type::Int32,
                ),
                read as *const u8,
                None,
            )?;
            vm.register_c_fn(
                "joky_example_write",
                FunctionSignature::new(vec![Type::reference(row.clone(), true)], Type::Int32),
                write as *const u8,
                None,
            )?;
        }
        vm.load_source(SOURCE)?;
        assert_eq!(vm.call_int32_0("run")?, 42);
        println!("{mode:?}: 42");
    }
    if let Some(directory) = std::env::args_os().nth(1) {
        let directory = Path::new(&directory);
        std::fs::create_dir_all(directory)?;
        let ast = parser::parse(SOURCE)?;
        std::fs::write(directory.join("bindings.h"), joky::ffi::c_header(&ast)?)?;
        let mut compiler = AotCompiler::new("fixed-array-example")?;
        compiler.compile_module(&ast)?;
        compiler.write_object(&directory.join("fixed_array.o"))?;
    }
    Ok(())
}
