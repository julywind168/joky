//! User-facing commands; compiler and runtime internals stay in the library.
mod build;
mod diagnostic;

use joky::{
    parser::{self, ast::Item},
    vm::{VmEngine, VmMode},
    Error, ErrorCode, ErrorPhase, Span, Type,
};
use std::{ffi::OsString, path::PathBuf};

const HELP: &str = "Joky — run and build Joky programs

Usage:
  joky run [--interpreter] [--no-prelude] <file.jk>
  joky build [--no-prelude] <file.jk> [-o <executable>]
  joky --help
  joky --version

Commands:
  run      Execute the program with the JIT compiler (or the interpreter)
  build    Compile a native executable using the system C compiler (cc)

Options:
  --interpreter       Execute 'run' with the register bytecode interpreter
  --no-prelude        Disable implicit standard-library imports
  -o, --output <path>  Output executable (default: source path without .jk)
  -h, --help           Show this help
  -V, --version        Show the version

Programs start at fn main() or fn main() -> Int32.
Import joky/io.{print, println} and pass a shared reference to a Show value.
String literals interpolate by default, e.g. \"value={value}\"; double braces escape literal braces.
Import joky/vm to create and call an embedded VM.
Use vm.register_int32 to expose a synchronous local Int32 function to it.
Use @suspends on main for cooperative tasks; import joky/task for yield_now and sleep_ms.";

enum Command {
    Help,
    Version,
    Run {
        source: PathBuf,
        interpreter: bool,
        prelude: bool,
    },
    Build {
        source: PathBuf,
        output: PathBuf,
        prelude: bool,
    },
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
        let mut prelude = true;
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
            } else if !positional_only && arg == "--no-prelude" {
                if !prelude {
                    return Err(cli_argument_error(
                        "--no-prelude was specified more than once",
                    ));
                }
                prelude = false;
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
            Ok(Self::Build {
                source,
                output,
                prelude,
            })
        } else {
            Ok(Self::Run {
                source,
                interpreter,
                prelude,
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
    let result = (|| -> Result<u8, Error> {
        let prelude = match command {
            Command::Run { prelude, .. } | Command::Build { prelude, .. } => prelude,
            _ => unreachable!(),
        };
        let mut vm = VmEngine::with_mode(
            if matches!(
                command,
                Command::Run {
                    interpreter: true,
                    ..
                }
            ) {
                VmMode::Interpreter
            } else {
                VmMode::Jit
            },
        );
        vm.set_prelude_enabled(prelude);
        let ast = vm.type_registry().resolve_file(path)?;
        let result = (|| {
            let entry = validate_program(&ast, vm.type_registry())?;
            match command {
                Command::Run { .. } => run(&ast, entry, &mut vm),
                Command::Build {
                    source: ref source_path,
                    ref output,
                    ..
                } => {
                    build::build(&ast, entry, source_path, output)?;
                    eprintln!("Built {}", output.display());
                    Ok(0)
                }
                _ => unreachable!(),
            }
        })();
        result.map_err(|e: Error| e.with_sources(ast.sources()))
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            // Rendering happens once, here: the error keeps its phase, code,
            // line and span all the way to this print.
            // Named source snapshots cover parse, compile and interpreter errors.
            let rendered = diagnostic::render_error(path, "", &error);
            eprintln!("error: {}: {rendered}", path.display());
            1
        }
    }
}

fn report(error: &Error) -> u8 {
    eprintln!("error: {error}");
    1
}

struct EntryPoint {
    result_type: Type,
    suspends: bool,
}

fn validate_program(
    ast: &parser::ast::Module,
    types: &joky::TypeRegistry,
) -> Result<EntryPoint, Error> {
    let mut main = None;
    for item in &ast.items {
        match item {
            Item::FnDef(function) => {
                if function.name == "main" {
                    if main.is_some() {
                        return Err(cli_error(
                            &function.span,
                            ErrorCode::DuplicateDefinition,
                            "Duplicate function `main`",
                        ));
                    }
                    if !function.type_params.is_empty()
                        || !function.params.is_empty()
                        || !matches!(function.ret_ty, Type::Unit | Type::Int32)
                    {
                        return Err(cli_error(
                            &function.span,
                            ErrorCode::InvalidEntryPoint,
                            "Entry point must be fn main() or fn main() -> Int32",
                        ));
                    }
                    main = Some(EntryPoint {
                        result_type: function.ret_ty.clone(),
                        suspends: function.suspends,
                    });
                }
            }
            Item::ExternFn(function) => {
                // The VM/AOT registry validates these signatures and native entries.
                if types.is_builtin_host(&function.name) {
                    continue;
                }
                return Err(cli_error(
                    &function.span,
                    ErrorCode::UnknownName,
                    format!(
                        "Unknown CLI host function `{}`; import joky/io for print and println",
                        function.name
                    ),
                ));
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

fn run(ast: &parser::ast::Module, entry: EntryPoint, vm: &mut VmEngine) -> Result<u8, Error> {
    vm.load_module(ast)?;
    if entry.suspends {
        let result = joky::runtime::driver::block_on(vm.prepare_async("main", &[])?)?;
        Ok(if entry.result_type == Type::Int32 {
            result as u8
        } else {
            0
        })
    } else if entry.result_type == Type::Int32 {
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
