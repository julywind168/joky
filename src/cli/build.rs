//! Compile and link a standalone executable, publishing it only after success.
use joky_vm_interop::{
    aot::AotCompiler,
    parser::ast::{Item, Module},
    Type,
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
) -> Result<(), String> {
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
        return Err(format!("Output path '{}' is a directory", output.display()));
    }
    // Keep temporary output on the same filesystem so publication is a rename.
    let work = WorkDir::new(&parent)?;
    let object = work.0.join("program.o");
    let driver = work.0.join("driver.c");
    let executable = work.0.join("program");
    // Keep source symbols separate from the C entry point and library functions.
    let mut compiler = AotCompiler::with_symbol_prefix("joky_program", "__joky_")?;
    compiler.compile_module(ast)?;
    compiler.write_object(&object)?;
    let entry = if result_type == Type::Int32 {
        "extern int32_t __joky_main(void);\nint main(void) { return (uint8_t)__joky_main(); }"
    } else {
        "extern void __joky_main(void);\nint main(void) { __joky_main(); return 0; }"
    };
    let host = if ast
        .items
        .iter()
        .any(|item| matches!(item, Item::ExternFn(f) if f.name == "print_int"))
    {
        "void __joky_print_int(int32_t value) { printf(\"%\" PRId32 \"\\n\", value); }"
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
    let linked = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&driver)
        .arg(&object)
        .arg("-o")
        .arg(&executable)
        .output()
        .map_err(|error| format!("Cannot start C compiler 'cc': {error}. Install a system C toolchain to use 'joky build'."))?;
    if !linked.status.success() {
        return Err(format!(
            "Native linking failed ({}):\n{}",
            linked.status,
            String::from_utf8_lossy(&linked.stderr).trim()
        ));
    }
    std::fs::rename(&executable, &output)
        .map_err(|error| format!("Cannot publish executable '{}': {error}", output.display()))?;
    Ok(())
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
