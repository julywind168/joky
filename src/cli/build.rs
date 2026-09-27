//! Compile and link a standalone executable, publishing it only after success.
use joky::{
    aot::AotCompiler,
    parser::ast::{Item, Module},
    Error, Type,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) fn build(
    ast: &Module,
    result_type: Type,
    source: &Path,
    output: &Path,
) -> Result<(), Error> {
    let source = source.canonicalize().map_err(|error| error.to_string())?;
    if output.canonicalize().is_ok_and(|path| path == source) {
        return Err("Output path must not replace the source file".into());
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize().map_err(|error| {
        format!(
            "Cannot access output directory '{}': {error}",
            parent.display()
        )
    })?;
    let name = output
        .file_name()
        .ok_or("Output path must name an executable")?;
    let output = parent.join(name);
    if output.is_dir() {
        return Err(format!("Output path '{}' is a directory", output.display()).into());
    }
    // Keep temporary output on the same filesystem so publication is a rename.
    let work = WorkDir::new(&parent)?;
    let object = work.0.join("program.o");
    let driver = work.0.join("driver.c");
    let executable = work.0.join("program");
    // Keep source symbols separate from the C entry point and library functions.
    let mut compiler = AotCompiler::with_symbol_prefix("joky_program", "__joky_user_")?;
    compiler.compile_module(ast)?;
    compiler.write_object(&object)?;
    let entry = if result_type == Type::Int32 {
        "extern int32_t __joky_user_main(void);\nint main(void) { return (uint8_t)__joky_user_main(); }"
    } else {
        "extern void __joky_user_main(void);\nint main(void) { __joky_user_main(); return 0; }"
    };
    let host = if ast
        .items
        .iter()
        .any(|item| matches!(item, Item::ExternFn(f) if f.name == "print_int"))
    {
        "void __joky_user_print_int(int32_t value) { printf(\"%\" PRId32 \"\\n\", value); }"
    } else {
        ""
    };
    std::fs::write(
        &driver,
        format!(
            "#include <stdint.h>\n#include <inttypes.h>\n#include <stdio.h>\n{host}\n{entry}\n"
        ),
    )
    .map_err(|error| format!("Cannot write linker driver: {error}"))?;
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
    if ast.imports.iter().any(|import| import.path == "joky/vm") {
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
            return Err("Building programs with joky/vm currently requires macOS or Linux".into());
        }
    }
    let linked = linker.arg("-o").arg(&executable).output()
        .map_err(|error| format!("Cannot start C compiler 'cc': {error}. Install a system C toolchain to use 'joky build'."))?;
    if !linked.status.success() {
        return Err(format!(
            "Native linking failed ({}):\n{}",
            linked.status,
            String::from_utf8_lossy(&linked.stderr).trim()
        )
        .into());
    }
    std::fs::rename(&executable, &output)
        .map_err(|error| format!("Cannot publish executable '{}': {error}", output.display()))?;
    Ok(())
}

fn runtime_archive() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("JOKY_RUNTIME") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path.canonicalize().map_err(|e| e.to_string());
        }
        return Err(format!(
            "JOKY_RUNTIME does not name a runtime archive: '{}'",
            path.display()
        ));
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let directory = executable
        .parent()
        .ok_or("Cannot locate the Joky installation")?;
    for path in [
        directory.join("libjoky.a"),
        directory.join("../lib/joky/libjoky.a"),
    ] {
        if path.is_file() {
            return Ok(path);
        }
    }
    Err("Cannot find the Joky runtime for joky/vm. Build it with 'cargo build --release --lib', then set JOKY_RUNTIME to libjoky.a or install it in lib/joky beside the bin directory".into())
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(parent: &Path) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".joky-build-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("Cannot create build directory: {error}")),
            }
        }
        Err("Cannot create a unique build directory".into())
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
