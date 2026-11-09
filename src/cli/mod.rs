//! User-facing commands; compiler and runtime internals stay in the library.
mod build;
mod check;
mod diagnostic;
mod doc;
mod explain;
mod format;
mod help;
mod init;
mod progress;
mod project;
mod source;
mod testing;
mod workers;

use joky::{
    parser::{self, ast::Item},
    runtime::parallel::ExecutionMode,
    vm::{VmEngine, VmMode},
    Error, ErrorCode, ErrorPhase, Span, Type,
};
use std::{ffi::OsString, path::PathBuf};

const HELP: &str = "Joky — check, test, run, build, inspect and format Joky programs

Usage:
  joky init [--name <name>] [<directory>]
  joky doc [--std] [--open] [-o <directory>]
  joky test [--interpreter] [--no-prelude] [--filter <text>] [--list] [--nocapture] [--timeout-ms N] [<file.jk> | <project-dir> | joky.toml]
  joky check [--no-prelude] [<file.jk> | <project-dir> | joky.toml]
  joky run [-q | -v] [--interpreter] [--no-prelude] [--parallel] [--task-workers N] [<file.jk> | <project-dir> | joky.toml] [-- <program-args>...]
  joky build [--no-prelude] [--parallel] [--task-workers N] [<file.jk> | <project-dir> | joky.toml] [-o <output>]
  joky explain-parallel [--no-prelude] [<file.jk> | <project-dir> | joky.toml]
  joky fmt [--check] [<file.jk> | <directory> | joky.toml]
  joky fmt [--check] --stdin
  joky --help
  joky --version

Commands:
  init             Create a minimal application with joky.toml and src/main.jk
  doc              Generate offline homepage and standard-library HTML (default: target/doc)
  test             Run #test functions in isolated processes (default timeout: 30000 ms)
  check            Check syntax and semantics without running or generating code
  explain-parallel  Explain dispatch eligibility without running the program
  fmt              Format files, directories or projects without running code
  run              Execute the program with the JIT compiler (or the interpreter)
  build            Compile a native executable using the system C compiler (cc)

Options:
  --std               Generate only standard-library docs (default: target/doc/std)
  --open              Open generated documentation in the default browser
  --check             Report formatting differences without writing (exit 1)
  --stdin             Read fmt input from stdin; write formatted source to stdout
  --interpreter       Execute 'run' or 'test' with the register bytecode interpreter
  --no-prelude        Disable implicit standard-library imports
  --parallel          Enable checked Parallel execution (default: 1 worker)
  --task-workers N    Enable Parallel with 1..256 workers; build sets a startup default
  -q, --quiet         Hide run startup progress (errors are still reported)
  -v, --verbose       Show run startup progress and timings even when stderr is redirected
  -o, --output <path>  Output executable, project bundle, or doc directory
  -h, --help           Show this help
  -V, --version        Show the version

Use joky <command> --help or joky help <command> for command-specific usage.
Without an input, check/test/run/build/fmt/explain-parallel find the nearest joky.toml in the current directory or its parents.
Check allows single files without main; projects require a valid program entry.
Successful checks are silent (exit 0); errors go to stderr (exit 1).
Projects declare [project] name, entry, source-root (default \".\"), and resources.
Project builds default to <project>/dist/<name>; run them directly after moving the bundle.
Single-file builds default to the source path without .jk.
Program arguments follow -- after an explicit run input or in a native executable.
Parallel executables accept --task-workers N before -- at startup. Precedence:
startup option > JOKY_TASK_WORKERS > build default. Local rejects worker configuration.
Run uses the same precedence after explicit --parallel or --task-workers; build ignores the environment.
Run shows loading and compilation timings when stderr is a terminal.
Programs start at fn main() or fn main() -> Int32.
Import joky/io.{print, println}; Show values are borrowed automatically, e.g. println(42).
String literals interpolate by default, e.g. \"value={value}\"; double braces escape literal braces.
Import joky/vm to create and call an embedded VM.
Use vm.register_int32 to expose a synchronous local Int32 function to it.
Use @suspends on main for cooperative tasks; import joky/task for yield_now and sleep(Duration).";

enum Command {
    Help(&'static str),
    Init(init::Options),
    Version,
    Doc(doc::Options),
    Test(testing::Options),
    Format(format::Options),
    Check {
        source: PathBuf,
        prelude: bool,
    },
    Explain {
        source: PathBuf,
        prelude: bool,
    },
    Run {
        source: PathBuf,
        progress: Option<bool>,
        interpreter: bool,
        arguments: Vec<OsString>,
        prelude: bool,
        mode: ExecutionMode,
    },
    Build {
        source: PathBuf,
        output: Option<PathBuf>,
        prelude: bool,
        mode: ExecutionMode,
    },
}

impl Command {
    fn parse(args: impl Iterator<Item = OsString>) -> Result<Self, Error> {
        let mut args = args;
        let Some(command) = args.next() else {
            return Ok(Self::Help(HELP));
        };
        if command == "--help" || command == "-h" {
            return Ok(Self::Help(HELP));
        }
        if command == "help" {
            let topic = args.next();
            if args.next().is_some() {
                return Err(cli_argument_error("help accepts at most one command"));
            }
            let text = match topic {
                None => HELP,
                Some(topic) => help::command(&topic.to_string_lossy()).ok_or_else(|| {
                    cli_argument_error(format!("Unknown command `{}`", topic.to_string_lossy()))
                })?,
            };
            return Ok(Self::Help(text));
        }
        if command == "init" {
            return init::parse(args);
        }
        if command == "--version" || command == "-V" {
            if args.next().is_some() {
                return Err(cli_argument_error("Unexpected arguments after --version"));
            }
            return Ok(Self::Version);
        }
        if command == "doc" {
            return doc::parse(args);
        }
        if command == "fmt" {
            return format::parse(args);
        }
        if command == "explain-parallel" {
            return explain::parse(args);
        }
        if command == "test" {
            return testing::parse(args);
        }
        if command == "check" {
            return check::parse(args);
        }
        let build = command == "build";
        if !build && command != "run" {
            return Err(cli_argument_error(format!(
                "Unknown command `{}`. Use 'joky --help'.",
                command.to_string_lossy()
            )));
        }
        let mut arguments = Vec::new();
        let mut source = None;
        let mut output = None;
        let mut interpreter = false;
        let mut progress = None;
        let mut prelude = true;
        let mut parallel = false;
        let mut workers = None;
        let mut positional_only = false;
        while let Some(arg) = args.next() {
            if !positional_only && (arg == "--help" || arg == "-h") {
                return Ok(Self::Help(if build { help::BUILD } else { help::RUN }));
            }
            if arg == "--" && (!positional_only || !build && source.is_some()) {
                if !build && source.is_some() {
                    arguments.extend(args);
                    break;
                }
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
            } else if !positional_only
                && matches!(arg.to_str(), Some("-q" | "--quiet" | "-v" | "--verbose"))
            {
                if build {
                    return Err(cli_argument_error(
                        "--quiet and --verbose are only available for 'joky run'",
                    ));
                }
                if progress.is_some() {
                    return Err(cli_argument_error(
                        "Specify --quiet or --verbose only once; they cannot be combined",
                    ));
                }
                progress = Some(arg == "-v" || arg == "--verbose");
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
            } else if !positional_only && arg == "--parallel" {
                if parallel {
                    return Err(cli_argument_error(
                        "--parallel was specified more than once",
                    ));
                }
                parallel = true;
            } else if !positional_only && arg == "--task-workers" {
                if workers.is_some() {
                    return Err(cli_argument_error(
                        "--task-workers was specified more than once",
                    ));
                }
                let value = args.next().ok_or_else(|| {
                    cli_argument_error("--task-workers requires a worker count in 1..=256")
                })?;
                workers = Some(workers::parse(&value, "--task-workers")?);
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
        let source = source.or_else(project::discover).ok_or_else(|| {
            cli_argument_error(if build {
                "Missing source file or joky.toml project. Try 'joky init' to create a project, or 'joky build hello.jk' to build a file."
            } else {
                "Missing source file or joky.toml project. Try 'joky init' followed by 'joky run', or 'joky run hello.jk' to run a file."
            })
        })?;
        if !source.is_dir()
            && source.file_name().is_none_or(|name| name != "joky.toml")
            && source.extension().is_none_or(|ext| ext != "jk")
        {
            return Err(cli_argument_error(
                "Source files must have the .jk extension",
            ));
        }
        let environment = if build {
            None
        } else {
            std::env::var_os("JOKY_TASK_WORKERS")
        };
        let mode = workers::resolve(
            parallel || workers.is_some(),
            workers,
            environment.as_deref(),
        )?;
        if build {
            if output.as_ref().is_some_and(|p| p.as_os_str().is_empty()) {
                return Err(cli_argument_error("Output path must not be empty"));
            }
            Ok(Self::Build {
                source,
                output,
                prelude,
                mode,
            })
        } else {
            Ok(Self::Run {
                source,
                progress,
                interpreter,
                arguments,
                prelude,
                mode,
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
        Command::Help(text) => {
            println!("{text}");
            return 0;
        }
        Command::Version => {
            println!("joky {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Command::Init(options) => return init::execute(options),
        Command::Doc(output) => return doc::execute(output),
        Command::Test(options) => return testing::execute(options),
        Command::Format(options) => return format::execute(options),
        Command::Check { source, prelude } => return check::execute(source, *prelude),
        Command::Explain { source, prelude } => return explain::execute(source, *prelude),
        Command::Run { source: path, .. } | Command::Build { source: path, .. } => path,
    };
    let started = std::time::Instant::now();
    if matches!(command, Command::Build { .. }) {
        progress::status("Compiling", format_args!("{} (AOT)", path.display()));
    }
    let mut progress = progress::Progress::new(
        path,
        match command {
            Command::Run { progress, .. } => progress,
            _ => Some(false),
        },
    );
    let result = (|| -> Result<u8, Error> {
        let project = project::Project::load(path)?;
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
        if let Command::Run { ref arguments, .. } = command {
            let context = joky::runtime::process::ProcessContext::capture(arguments.clone())
                .map_err(|error| cli_error_with_code(ErrorCode::IoError, error.message()))?;
            vm.set_file_service(
                joky::runtime::fs::FileService::native_with_stdio(
                    project
                        .as_ref()
                        .map_or(std::path::Path::new("."), |p| p.root.as_path()),
                    Default::default(),
                )?
                .with_process_context(context),
            )?;
        }
        vm.set_prelude_enabled(prelude);
        let ast = match &project {
            Some(project) => project.resolve(vm.type_registry())?,
            None => vm.type_registry().resolve_file(path)?,
        };
        let result = (|| {
            let entry = validate_program(&ast, vm.type_registry())?;
            match command {
                Command::Run {
                    mode, interpreter, ..
                } => {
                    progress.compiling(interpreter);
                    run(&ast, entry, &mut vm, mode, &progress)
                }
                Command::Build {
                    source: ref source_path,
                    ref output,
                    mode,
                    ..
                } => {
                    let output = if let Some(project) = &project {
                        project.build(&ast, entry, output.as_deref(), mode)?
                    } else {
                        let output = output.clone().unwrap_or_else(|| {
                            source_path.with_extension(std::env::consts::EXE_EXTENSION)
                        });
                        build::build(&ast, entry, source_path, &output, mode, false)?;
                        output
                    };
                    progress::status(
                        "Finished",
                        format_args!(
                            "{} (built in {:.2}s)",
                            output.display(),
                            started.elapsed().as_secs_f64()
                        ),
                    );
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
    validate_module(ast, types)?.ok_or_else(|| {
        Error::new_with_code(
            ErrorPhase::TypeCheck,
            ErrorCode::InvalidEntryPoint,
            "Missing entry point: define fn main() or fn main() -> Int32",
        )
    })
}

fn validate_module(
    ast: &parser::ast::Module,
    types: &joky::TypeRegistry,
) -> Result<Option<EntryPoint>, Error> {
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
    Ok(main)
}

fn run(
    ast: &parser::ast::Module,
    entry: EntryPoint,
    vm: &mut VmEngine,
    mode: ExecutionMode,
    progress: &progress::Progress<'_>,
) -> Result<u8, Error> {
    vm.load_module(ast)?;
    progress.running();
    if let ExecutionMode::Parallel { workers } = mode {
        let prepared = vm.prepare_parallel_task("main", vec![], vec![])?;
        let executor = joky::runtime::parallel::ParallelExecutor::new(workers, 4096)?;
        let task = prepared.submit(&executor)?;
        task.wait()?;
        return match task.take_result()?.into_value() {
            joky::vm::TaskValue::Scalar(joky::vm::InstanceValue::Int32(value)) => Ok(value as u8),
            joky::vm::TaskValue::Scalar(joky::vm::InstanceValue::Unit) => Ok(0),
            _ => Err(cli_argument_error("Unsupported parallel main result")),
        };
    }
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
