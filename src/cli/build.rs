//! Compile and link a standalone executable, publishing it only after success.
mod driver;

use joky::{aot::AotCompiler, parser::ast::Module, Error, ErrorCode, ErrorPhase};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) fn build(
    ast: &Module,
    entry_point: super::EntryPoint,
    source: &Path,
    output: &Path,
    mode: joky::runtime::parallel::ExecutionMode,
    project: bool,
) -> Result<(), Error> {
    let source = source
        .canonicalize()
        .map_err(|error| build_error(ErrorCode::IoError, error.to_string()))?;
    if output.canonicalize().is_ok_and(|path| {
        path == source
            || ast.sources().files().any(|file| {
                Path::new(file.name())
                    .canonicalize()
                    .is_ok_and(|file| file == path)
            })
    }) {
        return Err(build_error(
            ErrorCode::InvalidArgument,
            "Output path must not replace the source file or an imported source",
        ));
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize().map_err(|error| {
        build_error(
            ErrorCode::IoError,
            format!(
                "Cannot access output directory '{}': {error}",
                parent.display()
            ),
        )
    })?;
    let name = output.file_name().ok_or_else(|| {
        build_error(
            ErrorCode::InvalidArgument,
            "Output path must name an executable",
        )
    })?;
    let output = parent.join(name);
    if output.is_dir() {
        return Err(build_error(
            ErrorCode::InvalidArgument,
            format!("Output path '{}' is a directory", output.display()),
        ));
    }
    // Keep temporary output on the same filesystem so publication is a rename.
    let work = WorkDir::new(&parent)?;
    let object = work.0.join("program.o");
    let driver = work.0.join("driver.c");
    let executable = work.0.join("program");
    // Keep source symbols separate from the C entry point and library functions.
    let mut compiler = AotCompiler::with_symbol_prefix("joky_program", "__joky_user_")?;
    compiler.compile_module(ast)?;
    let parallel_main = match mode {
        joky::runtime::parallel::ExecutionMode::Local => None,
        joky::runtime::parallel::ExecutionMode::Parallel { .. } => Some(
            compiler
                .parallel_entry("main")?
                .standalone_driver_v1(4096)?,
        ),
    };
    let requires_runtime =
        compiler.requires_runtime() || ast.imports.iter().any(|import| import.path == "joky/vm");
    let main_slots = compiler.cps_frame_slots("main");
    compiler.write_object(&object)?;
    let driver_source =
        parallel_main.unwrap_or(driver::render(&entry_point, main_slots, requires_runtime)?);
    let driver_source = driver::startup(driver_source, mode, project, requires_runtime);
    std::fs::write(&driver, driver_source).map_err(|error| {
        build_error(
            ErrorCode::IoError,
            format!("Cannot write linker driver: {error}"),
        )
    })?;
    let mut linker = Command::new("cc");
    linker
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&driver)
        .arg(&object);
    // The system linker also needs to discard unused static runtime code.
    // Cargo's release profile only controls the compiler and runtime archive.
    if !cfg!(debug_assertions) {
        if cfg!(target_os = "macos") {
            // Standalone programs register JIT hosts by address and need no exports.
            linker.arg("-Wl,-dead_strip,-S,-x,-no_exported_symbols");
        } else if cfg!(target_os = "linux") {
            linker.arg("-Wl,--gc-sections,--strip-all");
        }
    }
    if requires_runtime {
        linker.arg(runtime_archive()?);
        if cfg!(target_os = "macos") {
            linker.args(["-liconv", "-lSystem"]);
        } else if cfg!(target_os = "linux") {
            linker.args([
                "-lgcc_s",
                "-lutil",
                "-lrt",
                "-lpthread",
                "-lm",
                "-ldl",
                "-lc",
            ]);
        } else {
            return Err(build_error(
                ErrorCode::LinkError,
                "Building programs using the native runtime currently requires macOS or Linux",
            ));
        }
    }
    super::progress::status("Linking", Path::new(name).display());
    let linked = linker.arg("-o").arg(&executable).output()
        .map_err(|error| {
            build_error(
                ErrorCode::LinkError,
                format!("Cannot start C compiler 'cc': {error}. Install a system C toolchain to use 'joky build'."),
            )
        })?;
    if !linked.status.success() {
        return Err(build_error(
            ErrorCode::LinkError,
            format!(
                "Native linking failed ({}):\n{}",
                linked.status,
                String::from_utf8_lossy(&linked.stderr).trim()
            ),
        ));
    }
    std::fs::rename(&executable, &output).map_err(|error| {
        build_error(
            ErrorCode::IoError,
            format!("Cannot publish executable '{}': {error}", output.display()),
        )
    })?;
    Ok(())
}

fn runtime_archive() -> Result<PathBuf, Error> {
    if let Some(path) = std::env::var_os("JOKY_RUNTIME") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path
                .canonicalize()
                .map_err(|e| build_error(ErrorCode::IoError, e.to_string()));
        }
        return Err(build_error(
            ErrorCode::NotFound,
            format!(
                "JOKY_RUNTIME does not name a runtime archive: '{}'",
                path.display()
            ),
        ));
    }
    let executable =
        std::env::current_exe().map_err(|e| build_error(ErrorCode::IoError, e.to_string()))?;
    let directory = executable
        .parent()
        .ok_or_else(|| build_error(ErrorCode::NotFound, "Cannot locate the Joky installation"))?;
    for path in [
        directory.join("libjoky.a"),
        directory.join("../lib/joky/libjoky.a"),
    ] {
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(build_error(
        ErrorCode::NotFound,
        "Cannot find the Joky runtime. Build it with 'cargo build --release --lib', then set JOKY_RUNTIME to libjoky.a or install it in lib/joky beside the bin directory",
    ))
}

pub(super) struct WorkDir(pub(super) PathBuf);

impl WorkDir {
    pub(super) fn new(parent: &Path) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".joky-build-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(build_error(
                        ErrorCode::IoError,
                        format!("Cannot create build directory: {error}"),
                    ))
                }
            }
        }
        Err(build_error(
            ErrorCode::InvalidState,
            "Cannot create a unique build directory",
        ))
    }
}

fn build_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, code, message)
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
