//! User-facing commands; compiler and runtime internals stay in the library.
mod build;

use joky_vm_interop::{
    parser::{self, ast::Item},
    vm::VmEngine,
    FunctionSignature, Type,
};
use std::{ffi::OsString, io::Write, path::PathBuf};

const HELP: &str = "Joky — run and build Joky programs

Usage:
  joky run <file.jk>
  joky build <file.jk> [-o <executable>]
  joky --help
  joky --version

Commands:
  run      Execute the program with the JIT compiler
  build    Compile a native executable using the system C compiler (cc)

Options:
  -o, --output <path>  Output executable (default: source path without .jk)
  -h, --help           Show this help
  -V, --version        Show the version

Programs start at fn main() or fn main() -> Int32.
Declare extern fn print_int(value: Int32) to print an integer.
The CLI currently supports synchronous programs.";

enum Command {
    Help,
    Version,
    Run(PathBuf),
    Build { source: PathBuf, output: PathBuf },
}

impl Command {
    fn parse(args: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args;
        let Some(command) = args.next() else {
            return Ok(Self::Help);
        };
        if command == "--help" || command == "-h" {
            return Ok(Self::Help);
        }
        if command == "--version" || command == "-V" {
            if args.next().is_some() {
                return Err("Unexpected arguments after --version".into());
            }
            return Ok(Self::Version);
        }
        let build = command == "build";
        if !build && command != "run" {
            return Err(format!(
                "Unknown command `{}`. Use 'joky --help'.",
                command.to_string_lossy()
            ));
        }
        let mut source = None;
        let mut output = None;
        let mut positional_only = false;
        while let Some(arg) = args.next() {
            if !positional_only && (arg == "--help" || arg == "-h") {
                return Ok(Self::Help);
            }
            if !positional_only && arg == "--" {
                positional_only = true;
                continue;
            }
            if !positional_only && (arg == "-o" || arg == "--output") {
                if !build {
                    return Err("--output is only available for 'joky build'".into());
                }
                if output.is_some() {
                    return Err("Output path was specified more than once".into());
                }
                output = Some(PathBuf::from(
                    args.next().ok_or("Missing path after --output")?,
                ));
            } else if !positional_only && arg.to_string_lossy().starts_with('-') {
                return Err(format!(
                    "Unknown option `{}`. Use 'joky --help'.",
                    arg.to_string_lossy()
                ));
            } else if source.replace(PathBuf::from(arg)).is_some() {
                return Err("Expected exactly one source file".into());
            }
        }
        let source = source.ok_or("Missing source file. Use 'joky --help'.")?;
        if source.extension().is_none_or(|ext| ext != "jk") {
            return Err("Source files must have the .jk extension".into());
        }
        if build {
            let output =
                output.unwrap_or_else(|| source.with_extension(std::env::consts::EXE_EXTENSION));
            if output.as_os_str().is_empty() {
                return Err("Output path must not be empty".into());
            }
            Ok(Self::Build { source, output })
        } else {
            Ok(Self::Run(source))
        }
    }
}

pub fn execute(args: impl Iterator<Item = OsString>) -> Result<u8, String> {
    let command = Command::parse(args)?;
    let path = match &command {
        Command::Help => {
            println!("{HELP}");
            return Ok(0);
        }
        Command::Version => {
            println!("joky {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Command::Run(path) | Command::Build { source: path, .. } => path,
    };
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("Cannot read '{}': {error}", path.display()))?;
    let result = (|| {
        let ast = parser::parse(&source).map_err(|error| error.to_string())?;
        let result_type = validate_program(&ast)?;
        match command {
            Command::Run(_) => run(&source, result_type),
            Command::Build {
                ref source,
                ref output,
            } => {
                build::build(&ast, result_type, source, output)?;
                eprintln!("Built {}", output.display());
                Ok(0)
            }
            _ => unreachable!(),
        }
    })();
    result.map_err(|error| format!("{}: {error}", path.display()))
}

fn validate_program(ast: &parser::ast::Module) -> Result<Type, String> {
    let mut main = None;
    for item in &ast.items {
        match item {
            Item::FnDef(function) => {
                if function.suspends {
                    return Err(
                        "The CLI does not support @suspends yet; use the embedding API".into(),
                    );
                }
                if function.name == "main" {
                    if main.is_some() {
                        return Err("Duplicate function `main`".into());
                    }
                    if !function.params.is_empty()
                        || !matches!(function.ret_ty, Type::Unit | Type::Int32)
                    {
                        return Err("Entry point must be fn main() or fn main() -> Int32".into());
                    }
                    main = Some(function.ret_ty.clone());
                }
            }
            Item::ExternFn(function) => {
                if function.name != "print_int" {
                    return Err(format!(
                        "Unknown CLI host function `{}`; available: print_int",
                        function.name
                    ));
                }
                if function.suspends
                    || function.ret_ty != Type::Unit
                    || function.params.len() != 1
                    || function.params[0].ty != Type::Int32
                {
                    return Err("Expected extern fn print_int(value: Int32)".into());
                }
            }
        }
    }
    main.ok_or_else(|| "Missing entry point: define fn main() or fn main() -> Int32".into())
}

extern "C" fn print_int(value: i32) {
    // Output failure must never unwind across generated code.
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}

fn run(source: &str, result_type: Type) -> Result<u8, String> {
    let mut vm = VmEngine::new();
    // This static callback has exactly the declared ABI and never unwinds.
    unsafe {
        vm.register_fn(
            "print_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            print_int as *const u8,
        );
    }
    vm.load_source(source)?;
    if result_type == Type::Int32 {
        Ok(vm.call_int32_0("main")? as u8)
    } else {
        vm.call_unit_0("main")?;
        Ok(0)
    }
}
