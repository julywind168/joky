//! CLI host metadata shared by validation, VM registration, and C rendering.
use joky::{vm::VmEngine, Error, FunctionSignature, Type};
use std::io::Write;

pub(super) struct CliHostSpec {
    pub name: &'static str,
    pub integer_type: Type,
    c_type: &'static str,
    printf_format: &'static str,
    callback: *const u8,
}

impl CliHostSpec {
    fn signature(&self) -> FunctionSignature {
        FunctionSignature::new(vec![self.integer_type.clone()], Type::Unit)
    }

    pub fn c_definition(&self) -> String {
        format!(
            "void __joky_user_{}({} value) {{ printf(\"%\" {} \"\\n\", value); }}",
            self.name, self.c_type, self.printf_format
        )
    }
}

pub(super) fn all() -> [CliHostSpec; 4] {
    [
        CliHostSpec {
            name: "print_int",
            integer_type: Type::Int32,
            c_type: "int32_t",
            printf_format: "PRId32",
            callback: print_int as *const u8,
        },
        CliHostSpec {
            name: "print_int64",
            integer_type: Type::Int64,
            c_type: "int64_t",
            printf_format: "PRId64",
            callback: print_int64 as *const u8,
        },
        CliHostSpec {
            name: "print_uint32",
            integer_type: Type::UInt32,
            c_type: "uint32_t",
            printf_format: "PRIu32",
            callback: print_uint32 as *const u8,
        },
        CliHostSpec {
            name: "print_uint64",
            integer_type: Type::UInt64,
            c_type: "uint64_t",
            printf_format: "PRIu64",
            callback: print_uint64 as *const u8,
        },
    ]
}

pub(super) fn lookup(name: &str) -> Option<CliHostSpec> {
    all().into_iter().find(|host| host.name == name)
}

pub(super) fn available_names() -> String {
    all().map(|host| host.name).join(", ")
}

pub(super) fn register(vm: &mut VmEngine) -> Result<(), Error> {
    for host in all() {
        // These static callbacks have exactly their declared ABI and never unwind.
        unsafe { vm.register_fn(host.name, host.signature(), host.callback)? };
    }
    Ok(())
}

extern "C" fn print_int(value: i32) {
    // Output failure must never unwind across generated code.
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}

extern "C" fn print_int64(value: i64) {
    // Output failure must never unwind across generated code.
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}

extern "C" fn print_uint32(value: u32) {
    // Output failure must never unwind across generated code.
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}

extern "C" fn print_uint64(value: u64) {
    // Output failure must never unwind across generated code.
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}
