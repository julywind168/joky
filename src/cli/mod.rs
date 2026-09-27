//! User-facing commands; compiler and runtime internals stay in the library.
mod build;
mod diagnostic;

use joky::{
    parser::{self, ast::Item},
    vm::{VmEngine, VmMode},
    Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type,
};
use std::{ffi::OsString, io::Write, path::PathBuf};

const HELP: &str = "Joky — run and build Joky programs

Usage:
  joky run [--interpreter] <file.jk>
  joky build <file.jk> [-o <executable>]
  joky --help
  joky --version

Commands:
  run      Execute the program with the JIT compiler (or the interpreter)
  build    Compile a native executable using the system C compiler (cc)

Options:
  --interpreter       Execute 'run' with the register bytecode interpreter
  -o, --output <path>  Output executable (default: source path without .jk)
  -h, --help           Show this help
  -V, --version        Show the version

Programs start at fn main() or fn main() -> Int32.
Declare extern fn print_int(value: Int32) to print an integer.
Import joky/vm to create and call an embedded VM.
Use vm.register_int32 to expose a synchronous local Int32 function to it.
The CLI currently supports synchronous programs.";

enum Command {
    Help,
    Version,
    Run { source: PathBuf, interpreter: bool },
    Build { source: PathBuf, output: PathBuf },
}

impl Command {
    fn parse(args: impl Iterator<Item = OsString>) -> Result<Self, Error> {
        let mut args = args;
        let Some(command) = args.next() else {
            return Ok(Self::Help);
        };
        if command == "--help" || command == "-h" {
            return Ok(Self::Help);
        }
        if command == "--version" || command == "-V" {
            if args.next().is_some() {
                return Err(cli_argument_error("Unexpected arguments after --version"));
            }
            return Ok(Self::Version);
        }
        let build = command == "build";
        if !build && command != "run" {
            return Err(cli_argument_error(format!(
                "Unknown command `{}`. Use 'joky --help'.",
                command.to_string_lossy()
            )));
        }
        let mut source = None;
        let mut output = None;
        let mut interpreter = false;
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
                    return Err(cli_argument_error(
                        "--output is only available for 'joky build'",
                    ));
                }
                if output.is_some() {
                    return Err(cli_argument_error(
                        "Output path was specified more than once",
                    ));
                }
                output =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        cli_argument_error("Missing path after --output")
                    })?));
            } else if !positional_only && arg == "--interpreter" {
                if build {
                    return Err(cli_argument_error(
                        "--interpreter is only available for 'joky run'",
                    ));
                }
                if interpreter {
                    return Err(cli_argument_error(
                        "--interpreter was specified more than once",
                    ));
                }
                interpreter = true;
            } else if !positional_only && arg.to_string_lossy().starts_with('-') {
                return Err(cli_argument_error(format!(
                    "Unknown option `{}`. Use 'joky --help'.",
                    arg.to_string_lossy()
                )));
            } else if source.replace(PathBuf::from(arg)).is_some() {
                return Err(cli_argument_error("Expected exactly one source file"));
            }
        }
        let source =
            source.ok_or_else(|| cli_argument_error("Missing source file. Use 'joky --help'."))?;
        if source.extension().is_none_or(|ext| ext != "jk") {
            return Err(cli_argument_error(
                "Source files must have the .jk extension",
            ));
        }
        if build {
            let output =
                output.unwrap_or_else(|| source.with_extension(std::env::consts::EXE_EXTENSION));
            if output.as_os_str().is_empty() {
                return Err(cli_argument_error("Output path must not be empty"));
            }
            Ok(Self::Build { source, output })
        } else {
            Ok(Self::Run {
                source,
                interpreter,
            })
        }
    }
}

pub fn execute(args: impl Iterator<Item = OsString>) -> u8 {
    let command = match Command::parse(args) {
        Ok(command) => command,
        Err(error) => return report(&error),
    };
    let path = match &command {
        Command::Help => {
            println!("{HELP}");
            return 0;
        }
        Command::Version => {
            println!("joky {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Command::Run { source: path, .. } | Command::Build { source: path, .. } => path,
    };
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            let error = cli_error_with_code(
                ErrorCode::IoError,
                format!("Cannot read '{}': {error}", path.display()),
            );
            return report(&error);
        }
    };
    let result = (|| -> Result<u8, Error> {
        let ast = parser::parse(&source)?;
        let result_type = validate_program(&ast)?;
        match command {
            Command::Run { interpreter, .. } => run(&source, result_type, interpreter),
            Command::Build {
                source: ref source_path,
                ref output,
            } => {
                build::build(&ast, result_type, source_path, output)?;
                eprintln!("Built {}", output.display());
                Ok(0)
            }
            _ => unreachable!(),
        }
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            // Rendering happens once, here: the error keeps its phase, code,
            // line and span all the way to this print.
            let rendered = diagnostic::render_error(path, &source, &error);
            eprintln!("error: {}: {rendered}", path.display());
            1
        }
    }
}

fn report(error: &Error) -> u8 {
    eprintln!("error: {error}");
    1
}

fn validate_program(ast: &parser::ast::Module) -> Result<Type, Error> {
    let mut main = None;
    for item in &ast.items {
        match item {
            Item::FnDef(function) => {
                if function.suspends {
                    return Err(cli_error(
                        &function.span,
                        ErrorCode::InvalidEntryPoint,
                        "The CLI does not support @suspends yet; use the embedding API",
                    ));
                }
                if function.name == "main" {
                    if main.is_some() {
                        return Err(cli_error(
                            &function.span,
                            ErrorCode::DuplicateDefinition,
                            "Duplicate function `main`",
                        ));
                    }
                    if !function.params.is_empty()
                        || !matches!(function.ret_ty, Type::Unit | Type::Int32)
                    {
                        return Err(cli_error(
                            &function.span,
                            ErrorCode::InvalidEntryPoint,
                            "Entry point must be fn main() or fn main() -> Int32",
                        ));
                    }
                    main = Some(function.ret_ty.clone());
                }
            }
            Item::ExternFn(function) => {
                if function.name != "print_int" {
                    return Err(cli_error(
                        &function.span,
                        ErrorCode::UnknownName,
                        format!(
                            "Unknown CLI host function `{}`; available: print_int",
                            function.name
                        ),
                    ));
                }
                if function.suspends
                    || function.ret_ty != Type::Unit
                    || function.params.len() != 1
                    || function.params[0].ty != Type::Int32
                {
                    return Err(cli_error(
                        &function.span,
                        ErrorCode::InvalidEntryPoint,
                        "Expected extern fn print_int(value: Int32)",
                    ));
                }
            }
        }
    }
    main.ok_or_else(|| {
        Error::new_with_code(
            ErrorPhase::TypeCheck,
            ErrorCode::InvalidEntryPoint,
            "Missing entry point: define fn main() or fn main() -> Int32",
        )
    })
}

extern "C" fn print_int(value: i32) {
    // Output failure must never unwind across generated code.
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}

fn run(source: &str, result_type: Type, interpreter: bool) -> Result<u8, Error> {
    let mut vm = VmEngine::with_mode(if interpreter {
        VmMode::Interpreter
    } else {
        VmMode::Jit
    });
    // This static callback has exactly the declared ABI and never unwinds.
    unsafe {
        vm.register_fn(
            "print_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            print_int as *const u8,
        )
        .unwrap();
    }
    vm.load_source(source)?;
    if result_type == Type::Int32 {
        Ok(vm.call_int32_0("main")? as u8)
    } else {
        vm.call_unit_0("main")?;
        Ok(0)
    }
}

fn cli_error(span: &Span, code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message).with_span(*span)
}

fn cli_argument_error(message: impl Into<String>) -> Error {
    cli_error_with_code(ErrorCode::InvalidArgument, message)
}

fn cli_error_with_code(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Cli, code, message)
}
