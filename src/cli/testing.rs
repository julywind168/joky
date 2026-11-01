//! Native Joky tests share the ordinary frontend and execute in supervised children.
mod directory;

use super::{cli_argument_error, cli_error_with_code, diagnostic, project, Command};
use joky::{
    parser::ast::Item,
    vm::{VmEngine, VmMode},
    Checker, Error, ErrorCode,
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    time::{Duration, Instant},
};

pub(super) struct Options {
    source: PathBuf,
    interpreter: bool,
    prelude: bool,
    filter: Option<String>,
    list: bool,
    nocapture: bool,
    timeout: Duration,
    // Private worker protocol: source and source-root are separate to preserve imports.
    worker: Option<String>,
    root: Option<PathBuf>,
    directory: Option<PathBuf>,
}

pub(super) fn parse(args: impl Iterator<Item = OsString>) -> Result<Command, Error> {
    let mut options = Options {
        source: PathBuf::new(),
        interpreter: false,
        prelude: true,
        filter: None,
        list: false,
        nocapture: false,
        timeout: Duration::from_secs(30),
        worker: None,
        root: None,
        directory: None,
    };
    let mut source = None;
    let mut seen = BTreeSet::new();
    let mut positional = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        if !positional && (arg == "--help" || arg == "-h") {
            return Ok(Command::Help);
        }
        if !positional && arg == "--" {
            positional = true;
            continue;
        }
        if !positional && arg.to_string_lossy().starts_with('-') {
            if !seen.insert(arg.clone()) {
                return Err(cli_argument_error(format!(
                    "{} was specified more than once",
                    arg.to_string_lossy()
                )));
            }
            match arg.to_str() {
                Some("--interpreter") => options.interpreter = true,
                Some("--no-prelude") => options.prelude = false,
                Some("--list") => options.list = true,
                Some("--nocapture") => options.nocapture = true,
                Some(
                    "--filter" | "--timeout-ms" | "--test-worker" | "--test-root"
                    | "--test-directory",
                ) => {
                    let value = args.next().ok_or_else(|| {
                        cli_argument_error(format!("{} requires a value", arg.to_string_lossy()))
                    })?;
                    if arg == "--test-directory" {
                        options.directory = Some(value.into());
                        continue;
                    }
                    if arg == "--test-root" {
                        options.root = Some(value.into());
                        continue;
                    }
                    let value = value
                        .into_string()
                        .map_err(|_| cli_argument_error("Test option values must be UTF-8"))?;
                    if value.is_empty() {
                        return Err(cli_argument_error("Test option values must not be empty"));
                    }
                    if arg == "--filter" {
                        options.filter = Some(value);
                    } else if arg == "--test-worker" {
                        options.worker = Some(value);
                    } else {
                        let ms = value
                            .parse::<u64>()
                            .ok()
                            .filter(|n| (1..=86_400_000).contains(n))
                            .ok_or_else(|| {
                                cli_argument_error(
                                    "--timeout-ms requires an integer in 1..=86400000",
                                )
                            })?;
                        options.timeout = Duration::from_millis(ms);
                    }
                }
                _ => {
                    return Err(cli_argument_error(format!(
                        "Unknown test option `{}`",
                        arg.to_string_lossy()
                    )))
                }
            }
        } else if source.replace(PathBuf::from(arg)).is_some() {
            return Err(cli_argument_error(
                "Expected exactly one source file or project",
            ));
        }
    }
    if options.worker.is_some() != options.root.is_some()
        || options.worker.is_some() != options.directory.is_some()
        || options.worker.is_some()
            && (options.list || options.filter.is_some() || options.nocapture)
    {
        return Err(cli_argument_error("Invalid internal test worker arguments"));
    }
    options.source = source
        .or_else(project::discover)
        .ok_or_else(|| cli_argument_error("Missing source file or joky.toml project"))?;
    if !options.source.is_dir()
        && options.source.file_name().is_none_or(|n| n != "joky.toml")
        && options.source.extension().is_none_or(|e| e != "jk")
    {
        return Err(cli_argument_error(
            "Source files must have the .jk extension",
        ));
    }
    Ok(Command::Test(options))
}

pub(super) fn execute(options: &Options) -> u8 {
    let result = if let Some(name) = &options.worker {
        worker(options, name).map(|()| 0)
    } else {
        suite(options)
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "error: {}",
                diagnostic::render_error(&options.source, "", &error)
            );
            1
        }
    }
}

struct Test {
    file: PathBuf,
    function: String,
    name: String,
}

fn suite(options: &Options) -> Result<u8, Error> {
    let mut checker = Checker::new();
    checker.set_prelude_enabled(options.prelude);
    let project = project::Project::load(&options.source)?;
    let mut files = BTreeSet::new();
    let root = if let Some(project) = &project {
        let ast = project.resolve(checker.type_registry())?;
        checker.check_module(&ast)?;
        for file in ast.sources().files() {
            let path = Path::new(file.name());
            if path.is_absolute() && path.starts_with(&project.source_root) {
                files.insert(path.to_owned());
            }
        }
        collect_files(&project.source_root, &mut files)?;
        project.source_root.clone()
    } else {
        let file = options.source.canonicalize().map_err(io)?;
        let root = file.parent().expect("file has a parent").to_owned();
        files.insert(file);
        root
    };
    let mut tests = Vec::new();
    // Preflight every discovered test module before executing any test, even with a filter.
    for file in files {
        let ast = checker
            .type_registry()
            .resolve_test_file_with_root(&file, &root)?;
        super::validate_module(&ast, checker.type_registry())
            .map_err(|error| error.with_sources(ast.sources()))?;
        checker.check_module(&ast)?;
        let module = file
            .strip_prefix(&root)
            .expect("discovery stays inside root")
            .with_extension("")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        for item in &ast.items {
            if let Item::FnDef(function) = item {
                if function.is_test() {
                    tests.push(Test {
                        file: file.clone(),
                        function: function.name.clone(),
                        name: format!("{module}::{}", function.name),
                    });
                }
            }
        }
    }
    tests.sort_by(|a, b| a.name.cmp(&b.name));
    if tests.is_empty() {
        return Err(cli_argument_error("No #test functions found"));
    }
    tests.retain(|test| {
        options
            .filter
            .as_ref()
            .is_none_or(|filter| test.name.contains(filter))
    });
    if tests.is_empty() {
        return Err(cli_argument_error("No tests match --filter"));
    }
    if options.list {
        for test in tests {
            println!("{}", test.name);
        }
        return Ok(0);
    }
    println!("running {} tests", tests.len());
    let mut failed = 0;
    for test in &tests {
        std::io::stdout().flush().map_err(io)?;
        if !supervise(options, &root, test)? {
            failed += 1;
        }
    }
    println!(
        "test result: {}; {} passed; {} failed",
        if failed == 0 { "ok" } else { "FAILED" },
        tests.len() - failed,
        failed
    );
    Ok(u8::from(failed != 0))
}

fn collect_files(directory: &Path, files: &mut BTreeSet<PathBuf>) -> Result<(), Error> {
    for entry in std::fs::read_dir(directory).map_err(io)? {
        let entry = entry.map_err(io)?;
        let kind = entry.file_type().map_err(io)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_dir() && !name.starts_with('.') && name != "target" && name != "dist" {
            collect_files(&entry.path(), files)?;
        } else if kind.is_file() && name.ends_with("_test.jk") {
            files.insert(entry.path());
        }
    }
    Ok(())
}

fn worker(options: &Options, name: &str) -> Result<(), Error> {
    let mut vm = VmEngine::with_mode(if options.interpreter {
        VmMode::Interpreter
    } else {
        VmMode::Jit
    });
    vm.set_prelude_enabled(options.prelude);
    vm.set_file_service(joky::runtime::fs::FileService::testing(
        options
            .directory
            .as_ref()
            .expect("validated test directory"),
        Default::default(),
    )?)?;
    let ast = vm.type_registry().resolve_test_file_with_root(
        &options.source,
        options.root.as_ref().expect("validated worker root"),
    )?;
    let result = (|| {
        super::validate_module(&ast, vm.type_registry())?;
        let function = ast
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(f) if f.is_test() && f.name == name => Some(f),
                _ => None,
            })
            .ok_or_else(|| cli_argument_error(format!("Test `{name}` no longer exists")))?;
        vm.load_module(&ast)?;
        if function.suspends {
            joky::runtime::driver::block_on(vm.prepare_async(name, &[])?)?;
        } else {
            vm.call_unit_0(name)?;
        }
        Ok(())
    })();
    result.map_err(|error: Error| error.with_sources(ast.sources()))
}

// Always reap on early I/O failure as well as on timeout.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn supervise(options: &Options, root: &Path, test: &Test) -> Result<bool, Error> {
    let directory = directory::Directory::new().map_err(io)?;
    let mut command = Process::new(std::env::current_exe().map_err(io)?);
    command
        .arg("test")
        .arg("--test-worker")
        .arg(&test.function)
        .arg("--test-root")
        .arg(root)
        .arg("--test-directory")
        .arg(&directory.0);
    if options.interpreter {
        command.arg("--interpreter");
    }
    if !options.prelude {
        command.arg("--no-prelude");
    }
    command.arg("--").arg(&test.file).stdin(Stdio::null());
    if !options.nocapture {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let mut child = ChildGuard(command.spawn().map_err(io)?);
    let stdout = child
        .0
        .stdout
        .take()
        .map(|stream| std::thread::spawn(move || capture(stream)));
    let stderr = child
        .0
        .stderr
        .take()
        .map(|stream| std::thread::spawn(move || capture(stream)));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.0.try_wait().map_err(io)? {
            break status;
        }
        if start.elapsed() >= options.timeout {
            timed_out = true;
            child.0.kill().map_err(io)?;
            break child.0.wait().map_err(io)?;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let passed = !timed_out && status.success();
    println!(
        "test {} ... {}",
        test.name,
        if timed_out {
            "TIMEOUT"
        } else if passed {
            "ok"
        } else {
            "FAILED"
        }
    );
    for (label, stream) in [("stdout", stdout), ("stderr", stderr)] {
        if let Some(stream) = stream {
            let bytes = stream
                .join()
                .map_err(|_| cli_argument_error("Test output reader panicked"))?
                .map_err(io)?;
            if !passed && !bytes.is_empty() {
                eprintln!("---- {} {label} ----", test.name);
                std::io::stderr().write_all(&bytes).map_err(io)?;
                eprintln!();
            }
        }
    }
    if timed_out {
        eprintln!(
            "{} exceeded {} ms (including compilation)",
            test.name,
            options.timeout.as_millis()
        );
    } else if !passed {
        eprintln!("{}: child exited with {status}", test.name);
    }
    directory.clean().map_err(io)?;
    Ok(passed)
}

fn capture(mut stream: impl Read) -> std::io::Result<Vec<u8>> {
    const LIMIT: usize = 1024 * 1024;
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut truncated = false;
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let keep = count.min(LIMIT - output.len());
        output.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
    }
    if truncated {
        output.extend_from_slice(b"\n[output truncated after 1 MiB]\n");
    }
    Ok(output)
}

fn io(error: std::io::Error) -> Error {
    cli_error_with_code(ErrorCode::IoError, error.to_string())
}
