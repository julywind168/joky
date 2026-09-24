use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use joky::module::{resolve_module_path as resolve_path, ModuleGraph, ModuleLoadError};
use joky::syntax::parse_program;
use joky::{
    write_diagnostic, AotProviderMetadata, CheckResult, Compiler, Diagnostic, FrontendDiagnostic,
};

mod build_record;
mod debug_symbols;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Message(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
        Err(CliError::Silent) => ExitCode::FAILURE,
        Err(CliError::Diagnostic {
            path,
            source,
            diagnostic,
        }) => {
            let filename = path.to_string_lossy();
            if let Err(error) =
                write_diagnostic(&diagnostic, &filename, &source, io::stderr().lock())
            {
                eprintln!("failed to render diagnostic: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), CliError> {
    let mut args = std::env::args_os().skip(1);

    let Some(argument) = args.next().and_then(|value| value.into_string().ok()) else {
        return Err(CliError::usage());
    };

    match argument.as_str() {
        "new" => {
            let name = args
                .next()
                .ok_or_else(CliError::usage)?
                .into_string()
                .map_err(|_| CliError::usage())?;
            if args.next().is_some() {
                return Err(CliError::usage());
            }
            new_project(&name)
        }
        "run" => {
            let mut path = None;
            let mut options = RunOptions::default();
            while let Some(argument) = args.next() {
                if argument == "--" {
                    options.args = args.collect();
                    break;
                }
                let argument = argument.into_string().map_err(|_| CliError::usage())?;
                match argument.as_str() {
                    "--legacy" => options.legacy = true,
                    "--no-cache" => options.no_cache = true,
                    "--verbose" | "-v" => options.verbose = true,
                    flag if flag.starts_with('-') => return Err(CliError::usage()),
                    _ if path.is_none() => path = Some(argument),
                    _ => return Err(CliError::usage()),
                }
            }
            match path {
                Some(path) => run_file(Path::new(&path), options),
                None => run_project(Path::new("."), options),
            }
        }
        "check" => {
            let mut path = None;
            let mut no_cache = false;
            let mut verbose = false;
            let mut json = false;
            for argument in args {
                let argument = argument.into_string().map_err(|_| CliError::usage())?;
                match argument.as_str() {
                    "--no-cache" => no_cache = true,
                    "--verbose" | "-v" => verbose = true,
                    "--json" => json = true,
                    flag if flag.starts_with('-') => return Err(CliError::usage()),
                    _ if path.is_none() => path = Some(argument),
                    _ => return Err(CliError::usage()),
                }
            }
            check_path(
                path.as_deref().map(Path::new).unwrap_or(Path::new(".")),
                no_cache,
                verbose,
                json,
            )
        }
        "build" => {
            let mut path = None;
            let mut output = None;
            let mut release = false;
            let mut strip = false;
            let mut debug_info = false;
            let mut no_cache = false;
            let mut verbose = false;
            let mut target = None;
            while let Some(argument) = args.next() {
                let argument = argument.into_string().map_err(|_| CliError::usage())?;
                match argument.as_str() {
                    "-o" if output.is_none() => {
                        output = Some(args.next().ok_or_else(CliError::usage)?)
                    }
                    "--target" if target.is_none() => {
                        target = Some(args.next().ok_or_else(CliError::usage)?);
                    }
                    "--release" if !release => release = true,
                    "--strip" if !strip => strip = true,
                    "-g" | "--debug-info" if !debug_info => debug_info = true,
                    "--no-cache" if !no_cache => no_cache = true,
                    "--verbose" | "-v" if !verbose => verbose = true,
                    flag if flag.starts_with('-') => return Err(CliError::usage()),
                    _ if path.is_none() => path = Some(argument),
                    _ => return Err(CliError::usage()),
                }
            }
            let target = match target.as_deref() {
                Some(value) => {
                    joky::AotTarget::from_triple(value.to_str().ok_or_else(CliError::usage)?)
                        .map_err(CliError::Message)?
                }
                None => joky::AotTarget::default(),
            };
            let options = joky::AotBuildOptions {
                target,
                release,
                debug_info,
            };
            let path = path.as_deref().map(Path::new).unwrap_or(Path::new("."));
            if debug_info && strip {
                return Err(CliError::Message(
                    "--debug-info (-g) cannot be combined with --strip".into(),
                ));
            }
            if path.is_dir() || path == Path::new(".") {
                build_project(
                    path,
                    output.as_deref().map(Path::new),
                    options,
                    strip,
                    no_cache,
                    verbose,
                )
            } else {
                let output = output
                    .as_deref()
                    .map(Path::new)
                    .map(Path::to_owned)
                    .unwrap_or_else(|| path.with_extension(""));
                build_file(path, &output, options, strip, no_cache, verbose)
            }
        }
        _ => Err(CliError::usage()),
    }
}

fn write_check_json(output: &CheckResult) {
    match serde_json::to_string(output) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("failed to serialize check diagnostics: {error}"),
    }
}

fn check_path(path: &Path, no_cache: bool, verbose: bool, json: bool) -> Result<(), CliError> {
    let entry;
    let path = if path.is_dir() || path == Path::new(".") {
        if !path.join("joky.toml").is_file() {
            return Err(CliError::Message(format!(
                "no joky.toml found in '{}'; run `joky check <FILE>` or start inside a package",
                path.display()
            )));
        }
        entry = path.join("src/main.jk");
        entry.as_path()
    } else {
        path
    };
    let root = package_root(path);
    let graph = match ModuleGraph::load(path, &root) {
        Ok(graph) => graph,
        Err(error) if json => {
            let diagnostic = match error {
                ModuleLoadError::Diagnostic {
                    path, diagnostic, ..
                } => FrontendDiagnostic {
                    path: Some(path),
                    stage: diagnostic.stage(),
                    message: diagnostic.message().to_owned(),
                    span: diagnostic.span(),
                },
                ModuleLoadError::Message(message) => FrontendDiagnostic {
                    path: None,
                    stage: joky::Stage::Codegen,
                    message,
                    span: None,
                },
            };
            write_check_json(&CheckResult {
                ok: false,
                report: None,
                diagnostics: vec![diagnostic],
            });
            return Err(CliError::Silent);
        }
        Err(error) => return Err(error.into()),
    };
    let cache_dir = module_cache_dir(&root);
    emit_cache_status(verbose, no_cache, &cache_dir);
    let source = std::fs::read_to_string(path).map_err(|error| {
        CliError::Message(format!("failed to read '{}': {error}", path.display()))
    })?;
    let mut frontend = joky::Frontend::new();
    if json {
        let result = frontend.check_all(&graph, (!no_cache).then_some(cache_dir.as_path()));
        if verbose {
            for event in frontend.take_module_events() {
                eprintln!("[cache] {event}");
            }
        }
        write_check_json(&result);
        return if result.ok {
            Ok(())
        } else {
            Err(CliError::Silent)
        };
    }
    let result = frontend.check(&graph, (!no_cache).then_some(cache_dir.as_path()));
    if verbose {
        for event in frontend.take_module_events() {
            eprintln!("[cache] {event}");
        }
    }
    match result {
        Ok(_) => {
            eprintln!("Checked '{}'", path.display());
            Ok(())
        }
        Err(diagnostic) => {
            let entry = graph.module(joky::module::ModuleId(0));
            let (path, source) = frontend
                .take_diagnostic_source()
                .unwrap_or_else(|| (entry.path.clone(), source));
            Err(CliError::Diagnostic {
                path,
                source,
                diagnostic,
            })
        }
    }
}

fn build_project(
    root: &Path,
    output: Option<&Path>,
    options: joky::AotBuildOptions,
    strip: bool,
    no_cache: bool,
    verbose: bool,
) -> Result<(), CliError> {
    if !root.join("joky.toml").is_file() {
        return Err(CliError::Message(format!(
            "no joky.toml found in '{}'; run `joky build <FILE>` or start inside a package",
            root.display()
        )));
    }
    let name = root
        .canonicalize()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_os_string()))
        .unwrap_or_else(|| "main".into());
    let default_output = root.join(".joky/bin").join(name);
    let output = output.unwrap_or(&default_output);
    build_file(
        &root.join("src/main.jk"),
        output,
        options,
        strip,
        no_cache,
        verbose,
    )
}

fn build_file(
    path: &Path,
    output: &Path,
    options: joky::AotBuildOptions,
    strip: bool,
    no_cache: bool,
    verbose: bool,
) -> Result<(), CliError> {
    let joky::AotBuildOptions {
        target,
        release,
        debug_info,
    } = options;
    let started = Instant::now();
    let profile = runtime_profile(release);
    eprintln!(
        "Building '{}' ({profile}, {})",
        path.display(),
        target.triple()
    );
    let source = std::fs::read_to_string(path).map_err(|error| {
        CliError::Message(format!("failed to read '{}': {error}", path.display()))
    })?;
    let mut compiler = Compiler::new().map_err(|diagnostic| CliError::Diagnostic {
        path: path.to_owned(),
        source: source.clone(),
        diagnostic,
    })?;
    let graph = ModuleGraph::load(path, &package_root(path)).map_err(CliError::from)?;
    let cache_dir = module_cache_dir(&package_root(path));
    emit_cache_status(verbose, no_cache, &cache_dir);
    let compiled = compiler.compile_object_module_graph_with_build_options(
        &graph,
        options,
        (!no_cache).then_some(cache_dir.as_path()),
    );
    if verbose {
        for event in compiler.take_module_events() {
            eprintln!("[cache] {event}");
        }
    }
    let (object, provider_metadata) = compiled.map_err(|diagnostic| {
        let (path, source) = compiler
            .take_diagnostic_source()
            .unwrap_or((path.to_owned(), source.clone()));
        CliError::Diagnostic {
            path,
            source,
            diagnostic,
        }
    })?;
    let runtime_archive = locate_runtime_archive(release).ok_or_else(|| {
        CliError::Message(format!(
            "cannot locate the Joky {} runtime archive; build it with `cargo build --manifest-path crates/joky-runtime/Cargo.toml{}` or set JOKY_RUNTIME_ARCHIVE",
            if release { "release" } else { "debug" },
            if release { " --release" } else { "" }
        ))
    })?;
    if !runtime_archive.is_file() {
        return Err(CliError::Message(format!(
            "Joky runtime archive not found at '{}'; build the library before running `joky build`",
            runtime_archive.display()
        )));
    }
    let runtime_archive = runtime_archive.canonicalize().map_err(|error| {
        CliError::Message(format!(
            "failed to resolve Joky runtime archive '{}': {error}",
            runtime_archive.display()
        ))
    })?;
    eprintln!(
        "Runtime  '{}' (AOT ABI v{})",
        runtime_archive.display(),
        joky_runtime_abi::AOT_RUNTIME_ABI_VERSION
    );
    let object_path = output.with_extension("joky.o");
    let launcher_path = output.with_extension("joky-launcher.c");
    let launcher = render_aot_launcher(&provider_metadata);
    let record_error = |error| CliError::Message(format!("build record failed: {error}"));
    let mut linker_command =
        std::process::Command::new(build_record::linker_path().map_err(record_error)?);
    linker_command
        .arg(&object_path)
        .arg(&runtime_archive)
        .arg(&launcher_path);
    if strip {
        linker_command.arg("-s");
    }
    linker_command
        .args(["-lpthread", "-ldl", "-lm", "-o"])
        .arg(output);
    let mut record = build_record::BuildRecord::new(
        options,
        strip,
        profile,
        build_record::FileRecord::read(&runtime_archive).map_err(record_error)?,
        build_record::LinkerRecord::capture(&linker_command).map_err(record_error)?,
        &object,
        &launcher,
    )
    .map_err(record_error)?;
    record.sources = provider_metadata.sources;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            CliError::Message(format!("failed to create '{}': {error}", parent.display()))
        })?;
    }
    std::fs::write(&object_path, object).map_err(|error| {
        CliError::Message(format!(
            "failed to write '{}': {error}",
            object_path.display()
        ))
    })?;
    std::fs::write(&launcher_path, launcher).map_err(|error| {
        let _ = std::fs::remove_file(&object_path);
        CliError::Message(format!("failed to write launcher: {error}"))
    })?;
    // Once the linker may replace the binary, its previous provenance is stale.
    if let Err(error) =
        build_record::invalidate(output).and_then(|()| debug_symbols::invalidate(output))
    {
        let _ = std::fs::remove_file(&object_path);
        let _ = std::fs::remove_file(&launcher_path);
        return Err(record_error(error));
    }
    let linker = match linker_command.output() {
        Ok(output) => output,
        Err(error) => {
            let _ = std::fs::remove_file(&object_path);
            let _ = std::fs::remove_file(&launcher_path);
            return Err(CliError::Message(format!(
                "failed to invoke C linker: {error}"
            )));
        }
    };
    let symbols = if linker.status.success() && debug_info {
        debug_symbols::publish(output)
    } else {
        Ok(None)
    };
    let _ = std::fs::remove_file(&object_path);
    let _ = std::fs::remove_file(&launcher_path);
    if !linker.status.success() {
        let stderr = String::from_utf8_lossy(&linker.stderr);
        if stderr.contains(joky_runtime_abi::AOT_RUNTIME_ABI_SYMBOL) {
            return Err(CliError::Message(format!(
                "Joky runtime archive '{}' is incompatible with this compiler (expected AOT ABI version {})",
                runtime_archive.display(),
                joky_runtime_abi::AOT_RUNTIME_ABI_VERSION
            )));
        }
        return Err(CliError::Message(format!(
            "linking failed: {}",
            stderr.trim()
        )));
    }
    if let Some(symbols) =
        symbols.map_err(|error| CliError::Message(format!("debug symbols failed: {error}")))?
    {
        record.debug_artifact =
            Some(build_record::FileRecord::read(&symbols).map_err(record_error)?);
        eprintln!("Symbols  '{}'", symbols.display());
    }
    let record_path = record.publish(output).map_err(record_error)?;
    eprintln!("Record   '{}'", record_path.display());
    let size = std::fs::metadata(output).map_err(|error| {
        CliError::Message(format!(
            "failed to inspect output '{}': {error}",
            output.display()
        ))
    })?;
    eprintln!(
        "Finished '{}' ({}) in {}",
        output.display(),
        format_file_size(size.len()),
        format_duration(started.elapsed())
    );
    Ok(())
}

fn format_file_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    if bytes >= MIB as u64 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= KIB as u64 {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn render_aot_launcher(metadata: &AotProviderMetadata) -> String {
    let runtime_abi_symbol = joky_runtime_abi::AOT_RUNTIME_ABI_SYMBOL;
    let machine_declarations = metadata
        .machine_entries
        .iter()
        .map(|(_, symbol)| format!("extern void {symbol}(void *);\n"))
        .collect::<String>();
    let has_providers = !metadata.providers.is_empty();
    let provider_declarations = if has_providers {
        "struct joky_provider_operation { const char *effect; const char *name; unsigned long long operation; };\nextern void *jk_provider_register(const char *, const struct joky_provider_operation *, size_t);\nextern void jk_provider_unregister(void *);\n"
    } else {
        ""
    };
    let provider_tables = metadata
        .providers
        .iter()
        .map(|provider| {
            let entries = provider
                .operations
                .iter()
                .map(|operation| {
                    format!(
                        "{{\"{}\", \"{}\", 0x{:016x}ULL}}",
                        operation.effect, operation.name, operation.operation
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "static const struct joky_provider_operation joky_{}_operations[{}] = {{ {} }};\n",
                provider.name,
                provider.operations.len(),
                entries
            )
        })
        .collect::<String>();
    let mut registration_call = format!("    {runtime_abi_symbol}();\n    if (!jk_aot_set_args(argc, (const char *const *)argv)) return 6;\n");
    #[cfg(feature = "runtime-test-support")]
    registration_call.push_str("    jk_test_aot_resources(0);\n");
    for (key, symbol) in &metadata.machine_entries {
        registration_call.push_str(&format!(
            "    if (!jk_aot_register_machine_entry({key}, (void *){symbol})) return 5;\n"
        ));
    }
    for (index, provider) in metadata.providers.iter().enumerate() {
        registration_call.push_str(&format!(
            "    void *joky_provider_registration_{index} = jk_provider_register(\"{}\", joky_{}_operations, {});\n    if (!joky_provider_registration_{index}) return {};\n",
            provider.name,
            provider.name,
            provider.operations.len(),
            2 + index
        ));
    }
    if metadata.main_is_suspending {
        registration_call.push_str("    int joky_status = jk_aot_run_pending_main(joky_main);\n");
    } else {
        registration_call.push_str("    joky_main();\n");
    }
    if metadata.main_returns_result && !metadata.main_is_suspending {
        registration_call.push_str("    int joky_status = jk_aot_main_status();\n");
    }
    for (index, _) in metadata.providers.iter().enumerate() {
        registration_call.push_str(&format!(
            "    jk_provider_unregister(joky_provider_registration_{index});\n"
        ));
    }
    registration_call.push_str("    jk_aot_shutdown();\n");
    let registration_declarations =
        format!("{provider_declarations}extern void jk_aot_shutdown(void);\n");
    #[cfg(feature = "runtime-test-support")]
    registration_call.push_str("    jk_test_aot_resources(1);\n");
    #[cfg(feature = "runtime-test-support")]
    let registration_declarations =
        format!("{registration_declarations}extern void jk_test_aot_resources(unsigned char);\n");
    format!(
        "#include <stddef.h>\n\nextern {} joky_main(void);\nextern void {runtime_abi_symbol}(void);\nextern unsigned char jk_aot_set_args(int, const char *const *);\n{}{}{}{}{}\nint main(int argc, char **argv) {{\n{}{}}}\n",
        if metadata.main_is_suspending { "unsigned char" } else { "void" },
        if metadata.main_returns_result && !metadata.main_is_suspending {
            "extern int jk_aot_main_status(void);\n"
        } else {
            ""
        },
        if metadata.main_is_suspending {
            "extern int jk_aot_run_pending_main(unsigned char (*)(void));\n"
        } else {
            ""
        },
        if metadata.machine_entries.is_empty() {
            "".to_owned()
        } else {
            format!(
                "extern unsigned char jk_aot_register_machine_entry(size_t, void *);\n{machine_declarations}"
            )
        },
        registration_declarations,
        provider_tables,
        registration_call,
        if metadata.main_returns_result || metadata.main_is_suspending {
            "    return joky_status;\n"
        } else {
            "    return 0;\n"
        },
    )
}

const RUNTIME_ARCHIVE_NAME: &str = "libjoky_runtime.a";

/// Find the standalone runtime archive for the compiler binary. Installed
/// binaries can place it beside the executable or one directory above;
/// development builds keep it under a Cargo target directory.
fn locate_runtime_archive(release: bool) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("JOKY_RUNTIME_ARCHIVE") {
        return Some(PathBuf::from(path));
    }
    let profile = runtime_profile(release);
    runtime_archive_candidates(
        std::env::current_exe().ok().as_deref(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        profile,
    )
    .into_iter()
    .find(|path| path.is_file())
}

fn runtime_profile(release: bool) -> &'static str {
    if release || !cfg!(debug_assertions) {
        "release"
    } else {
        "debug"
    }
}

fn runtime_archive_candidates(
    executable: Option<&Path>,
    manifest_dir: &Path,
    profile: &str,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let executable_directory = executable.and_then(Path::parent);
    let cargo_profile = executable_directory.and_then(|directory| {
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| *name == "debug" || *name == "release")
    });
    if let Some(directory) = executable_directory {
        if cargo_profile.is_none_or(|name| name == profile) {
            candidates.push(directory.join(RUNTIME_ARCHIVE_NAME));
            if cargo_profile.is_some() {
                candidates.extend(cargo_dependency_runtime_archives(directory));
            }
        }
        if cargo_profile.is_none() {
            if let Some(parent) = directory.parent() {
                candidates.push(parent.join(RUNTIME_ARCHIVE_NAME));
            }
        }
    }
    // `target/<profile>/deps/libjoky_runtime-<hash>.a` is Cargo's fingerprint
    // copy of the staticlib. Compilers launched from `target/debug` or
    // `target/release` still need it, so a debug `joky build --release` can
    // find a release archive that was never uplifted to `libjoky_runtime.a`.
    // An installed compiler (for example `/opt/joky/bin/joky`) is not in a
    // Cargo profile directory; listing those fingerprint archives would prefer
    // a source-tree intermediate over the standalone archive.
    if executable.is_none() || cargo_profile.is_some() {
        candidates.extend(cargo_dependency_runtime_archives(
            &manifest_dir.join("target").join(profile),
        ));
    }
    candidates.push(
        manifest_dir
            .join("crates/joky-runtime/target")
            .join(profile)
            .join(RUNTIME_ARCHIVE_NAME),
    );
    candidates.push(
        manifest_dir
            .join("target")
            .join(profile)
            .join(RUNTIME_ARCHIVE_NAME),
    );
    candidates
}

fn cargo_dependency_runtime_archives(target_profile_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(target_profile_dir.join("deps")) else {
        return Vec::new();
    };
    let mut archives = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if name.starts_with("libjoky_runtime-") && name.ends_with(".a") {
                Some((entry.metadata().ok()?.modified().ok(), path))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    archives.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    archives.into_iter().map(|(_, path)| path).collect()
}

fn new_project(name: &str) -> Result<(), CliError> {
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
        || !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
    {
        return Err(CliError::Message(format!(
            "invalid package name '{name}': use an ASCII name starting with a letter"
        )));
    }
    let root = Path::new(name);
    if root.exists() {
        return Err(CliError::Message(format!(
            "cannot create package '{}': path already exists",
            root.display()
        )));
    }
    std::fs::create_dir_all(root.join("src")).map_err(|error| {
        CliError::Message(format!(
            "failed to create package '{}': {error}",
            root.display()
        ))
    })?;
    std::fs::write(root.join("joky.toml"), format!("name = \"{name}\"\n"))
        .map_err(|error| CliError::Message(format!("failed to write joky.toml: {error}")))?;
    std::fs::write(
        root.join("src/main.jk"),
        "fn main() {\n    println(\"hello, package\")\n}\n",
    )
    .map_err(|error| CliError::Message(format!("failed to write src/main.jk: {error}")))?;
    println!("created package '{}'", root.display());
    Ok(())
}

#[derive(Default, Clone)]
struct RunOptions {
    legacy: bool,
    no_cache: bool,
    verbose: bool,
    args: Vec<std::ffi::OsString>,
}

fn run_project(root: &Path, options: RunOptions) -> Result<(), CliError> {
    let manifest = root.join("joky.toml");
    if !manifest.is_file() {
        return Err(CliError::Message(format!(
            "no joky.toml found in '{}'; run `joky run <FILE>` or start inside a package",
            root.display()
        )));
    }
    run_file(&root.join("src/main.jk"), options)
}

fn run_file(path: &Path, options: RunOptions) -> Result<(), CliError> {
    let root = package_root(path);
    let source = if options.legacy {
        load_module_source(path, &root)?
    } else {
        std::fs::read_to_string(path).map_err(|error| {
            CliError::Message(format!("failed to read '{}': {error}", path.display()))
        })?
    };
    let mut compiler = Compiler::new().map_err(|diagnostic| CliError::Diagnostic {
        path: path.to_owned(),
        source: source.clone(),
        diagnostic,
    })?;
    let result = if options.legacy {
        if options.verbose {
            eprintln!("[compiler] legacy whole-program pipeline");
        }
        compiler.run_program_with_args(&source, options.args.clone())
    } else {
        let graph = ModuleGraph::load(path, &root).map_err(CliError::from)?;
        let cache_dir = module_cache_dir(&root);
        emit_cache_status(options.verbose, options.no_cache, &cache_dir);
        compiler.run_modular_program_with_cache_and_args(
            &graph,
            (!options.no_cache).then_some(cache_dir.as_path()),
            options.args.clone(),
        )
    };
    if options.verbose {
        for event in compiler.take_module_events() {
            eprintln!("[cache] {event}");
        }
    }
    let result = result.map_err(|diagnostic| {
        let (path, source) = compiler
            .take_diagnostic_source()
            .unwrap_or((path.to_owned(), source));
        CliError::Diagnostic {
            path,
            source,
            diagnostic,
        }
    });
    for warning in compiler.take_warnings() {
        eprintln!("warning: {warning}");
    }
    result
}

fn module_cache_dir(root: &Path) -> PathBuf {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .join(".joky/cache")
}

fn emit_cache_status(verbose: bool, no_cache: bool, cache_dir: &Path) {
    if !verbose {
        return;
    }
    if no_cache {
        eprintln!("[cache] disabled (modular pipeline)");
    } else {
        eprintln!("[cache] {}", cache_dir.display());
    }
}

fn package_root(entry: &Path) -> PathBuf {
    let parent = entry.parent().unwrap_or_else(|| Path::new("."));
    if parent.file_name().is_some_and(|name| name == "src") {
        parent.parent().unwrap_or(parent).to_owned()
    } else {
        parent.to_owned()
    }
}

fn load_module_source(entry: &Path, package_root: &Path) -> Result<String, CliError> {
    let _module_graph = ModuleGraph::load(entry, package_root).map_err(CliError::from)?;
    let mut visiting = HashSet::new();
    let mut loaded = HashSet::new();
    expand_module(
        entry,
        package_root,
        &_module_graph,
        &mut visiting,
        &mut loaded,
        true,
    )
}

fn expand_module(
    path: &Path,
    package_root: &Path,
    module_graph: &ModuleGraph,
    visiting: &mut HashSet<PathBuf>,
    loaded: &mut HashSet<PathBuf>,
    is_entry: bool,
) -> Result<String, CliError> {
    let canonical = path.canonicalize().map_err(|error| {
        CliError::Message(format!(
            "failed to read module '{}': {error}",
            path.display()
        ))
    })?;
    if loaded.contains(&canonical) {
        return Ok(String::new());
    }
    if !visiting.insert(canonical.clone()) {
        return Err(CliError::Message(format!(
            "cyclic module import involving '{}'",
            path.display()
        )));
    }
    let source = std::fs::read_to_string(&canonical).map_err(|error| {
        CliError::Message(format!(
            "failed to read module '{}': {error}",
            path.display()
        ))
    })?;
    let mut expanded = String::new();
    let mut dependencies = String::new();
    let mut aliases = HashMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(module) = trimmed
            .strip_prefix("import ")
            .map(str::trim)
            .map(|value| value.trim_end_matches(';').trim())
        {
            let module_path = resolve_module_path(module, package_root)?;
            let alias = module.rsplit('/').next().expect("validated module path");
            let exports = exported_symbols(&module_path)?;
            let module_id = module_graph
                .entry_id(&module_path)
                .map(|id| id.0)
                .ok_or_else(|| {
                    CliError::Message(format!(
                        "module '{}' is missing from the module graph",
                        module_path.display()
                    ))
                })?;
            aliases.insert(alias.to_owned(), (format!("m{module_id}_"), exports));
            dependencies.push_str(&expand_module(
                &module_path,
                package_root,
                module_graph,
                visiting,
                loaded,
                false,
            )?);
            dependencies.push('\n');
            continue;
        }
        let trimmed = line.trim_start();
        let declaration = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
        expanded.push_str(&line[..line.len() - trimmed.len()]);
        expanded.push_str(declaration);
        expanded.push('\n');
    }
    let module_alias = (!is_entry)
        .then(|| {
            canonical
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| *name != "main")
        })
        .flatten();
    if module_alias.is_some() {
        let module_id = module_graph
            .entry_id(&canonical)
            .map(|id| id.0)
            .ok_or_else(|| {
                CliError::Message(format!(
                    "module '{}' is missing from the module graph",
                    canonical.display()
                ))
            })?;
        expanded = prefix_module_declarations(&expanded, &format!("m{module_id}_"));
    }
    expanded = rewrite_legacy_imports(expanded, &aliases, path)?;
    visiting.remove(&canonical);
    loaded.insert(canonical);
    // Imported modules already have their own declaration prefix. Applying
    // this module's prefix to them again breaks transitive imports.
    dependencies.push_str(&expanded);
    Ok(dependencies)
}

// Rewrite only expression fields, leaving handler operation patterns and
// strings alone. Each module owns its aliases; provider implementations must
// keep calling their raw effects even when a public helper shares the name.
fn rewrite_legacy_imports(
    mut source: String,
    aliases: &HashMap<String, (String, LegacyExports)>,
    path: &Path,
) -> Result<String, CliError> {
    use joky::syntax::{Expr, ExprKind, ExprVisitor, FieldAccess};
    for (alias, (_, exports)) in aliases {
        for name in &exports.types {
            source = source.replace(&format!("{alias}.{name}"), name);
        }
    }
    let program = parse_program(&source).map_err(|diagnostic| CliError::Diagnostic {
        path: path.to_owned(),
        source: source.clone(),
        diagnostic,
    })?;
    struct Rewrite<'a> {
        aliases: &'a HashMap<String, (String, LegacyExports)>,
        edits: Vec<(joky::Span, String)>,
    }
    impl ExprVisitor for Rewrite<'_> {
        type Output = ();
        fn default_output(&self) {}
        fn visit_expr(&mut self, expr: &Expr) {
            if let ExprKind::Field {
                value,
                access: FieldAccess::Name(member),
            } = &expr.kind
            {
                if let ExprKind::Name(alias) = &value.kind {
                    if let Some((prefix, exports)) = self.aliases.get(alias) {
                        if exports.values.contains(member) {
                            self.edits.push((expr.span, format!("{prefix}{member}")));
                            return;
                        }
                    }
                }
            }
            joky::syntax::walk_expr(self, expr);
        }
    }
    let mut rewrite = Rewrite {
        aliases,
        edits: Vec::new(),
    };
    joky::syntax::walk_program(&mut rewrite, &program);
    for value in program
        .structs
        .iter()
        .flat_map(|ty| &ty.fields)
        .filter_map(|field| field.default.as_ref())
    {
        rewrite.visit_expr(value);
    }
    for value in program
        .classes
        .iter()
        .flat_map(|ty| &ty.fields)
        .filter_map(|field| field.default.as_ref())
    {
        rewrite.visit_expr(value);
    }
    for implementation in &program.impls {
        for method in &implementation.methods {
            rewrite.visit_expr(&method.body);
        }
    }
    rewrite
        .edits
        .sort_by_key(|(span, _)| std::cmp::Reverse(span.start()));
    for (span, replacement) in rewrite.edits {
        source.replace_range(span.start()..span.end(), &replacement);
    }
    Ok(source)
}

struct LegacyExports {
    values: HashSet<String>,
    types: HashSet<String>,
}

fn exported_symbols(path: &Path) -> Result<LegacyExports, CliError> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        CliError::Message(format!(
            "failed to read module '{}': {error}",
            path.display()
        ))
    })?;
    let program = parse_program(&source).map_err(|diagnostic| CliError::Diagnostic {
        path: path.to_owned(),
        source: source.clone(),
        diagnostic,
    })?;
    let types = program
        .structs
        .into_iter()
        .map(|ty| ty.name)
        .chain(program.classes.into_iter().map(|ty| ty.name))
        .chain(program.enums.into_iter().map(|ty| ty.name))
        .chain(program.traits.into_iter().map(|ty| ty.name))
        .collect();
    let values = program
        .functions
        .into_iter()
        .filter(|function| matches!(function.visibility, joky::syntax::Visibility::Public))
        .map(|function| function.name)
        .chain(
            program
                .constants
                .into_iter()
                .filter(|constant| matches!(constant.visibility, joky::syntax::Visibility::Public))
                .map(|constant| constant.name),
        )
        .collect();
    Ok(LegacyExports { values, types })
}

fn prefix_module_declarations(source: &str, prefix: &str) -> String {
    // Prefix declaration names only, preserving explicit C symbol strings.
    // An extern attribute and its function can occupy separate lines.
    let source = source.to_owned();
    let mut brace_depth = 0usize;
    source
        .lines()
        .map(|line| {
            let at_module_scope = brace_depth == 0;
            let transformed = if at_module_scope {
                let trimmed = line.trim_start();
                if let Some(function) = trimmed.strip_prefix("fn ") {
                    if let Some(name_end) = function.find('(') {
                        let indentation = &line[..line.len() - trimmed.len()];
                        format!(
                            "{indentation}fn {prefix}{}{suffix}",
                            &function[..name_end],
                            suffix = &function[name_end..]
                        )
                    } else {
                        line.to_owned()
                    }
                } else if trimmed.starts_with("@extern(c,") {
                    if let Some(fn_pos) = trimmed.find(" fn ") {
                        let after = &trimmed[fn_pos + 4..];
                        if let Some(name_end) = after.find('(') {
                            let indentation = &line[..line.len() - trimmed.len()];
                            format!(
                                "{indentation}{} fn {prefix}{}{}",
                                &trimmed[..fn_pos],
                                &after[..name_end],
                                &after[name_end..]
                            )
                        } else {
                            line.to_owned()
                        }
                    } else {
                        line.to_owned()
                    }
                } else if let Some(constant) = trimmed.strip_prefix("const ") {
                    if let Some(name_end) = constant.find([':', '=']) {
                        let indentation = &line[..line.len() - trimmed.len()];
                        let name = constant[..name_end].trim_end();
                        format!(
                            "{indentation}const {prefix}{}{suffix}",
                            name,
                            suffix = &constant[name.len()..]
                        )
                    } else {
                        line.to_owned()
                    }
                } else {
                    line.to_owned()
                }
            } else {
                line.to_owned()
            };
            brace_depth = brace_depth
                .saturating_add(line.chars().filter(|character| *character == '{').count())
                .saturating_sub(line.chars().filter(|character| *character == '}').count());
            transformed
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolve_module_path(module: &str, package_root: &Path) -> Result<PathBuf, CliError> {
    resolve_path(module, package_root).map_err(CliError::Message)
}

enum CliError {
    Message(String),
    Silent,
    Diagnostic {
        path: PathBuf,
        source: String,
        diagnostic: Diagnostic,
    },
}

impl From<ModuleLoadError> for CliError {
    fn from(error: ModuleLoadError) -> Self {
        match error {
            ModuleLoadError::Message(message) => Self::Message(message),
            ModuleLoadError::Diagnostic {
                path,
                source,
                diagnostic,
            } => Self::Diagnostic {
                path,
                source,
                diagnostic,
            },
        }
    }
}

impl CliError {
    fn usage() -> Self {
        Self::Message(
            "usage: joky new <NAME> | joky run [FILE] [--no-cache] [--verbose] [--legacy] [-- ARGS...] | joky check [FILE|PROJECT] [--no-cache] [--verbose] [--json] | joky build [FILE|PROJECT] [-o OUTPUT] [--target TRIPLE] [--release] [-g|--debug-info] [--strip] [--no-cache] [--verbose]"
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_archive_candidates_only_include_the_standalone_archive() {
        let root = std::env::temp_dir().join(format!(
            "joky-standalone-archive-candidates-{}",
            std::process::id()
        ));
        let release_deps = root.join("target/release/deps");
        std::fs::create_dir_all(&release_deps).unwrap();
        let fingerprint_archive = release_deps.join("libjoky_runtime-deadbeef.a");
        std::fs::write(&fingerprint_archive, b"archive").unwrap();

        let candidates =
            runtime_archive_candidates(Some(Path::new("/opt/joky/bin/joky")), &root, "release");

        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/opt/joky/bin/libjoky_runtime.a"),
                PathBuf::from("/opt/joky/libjoky_runtime.a"),
                root.join("crates/joky-runtime/target/release/libjoky_runtime.a"),
                root.join("target/release/libjoky_runtime.a"),
            ]
        );
        assert!(candidates.iter().all(|path| {
            path.file_name()
                .is_some_and(|name| name == RUNTIME_ARCHIVE_NAME)
        }));
        assert!(!candidates.contains(&fingerprint_archive));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_archive_candidates_include_cargo_dependency_artifacts() {
        let root =
            std::env::temp_dir().join(format!("joky-runtime-candidates-{}", std::process::id()));
        let profile_dir = root.join("target/debug");
        std::fs::create_dir_all(profile_dir.join("deps")).unwrap();
        let dependency_archive = profile_dir.join("deps/libjoky_runtime-a1b2c3.a");
        std::fs::write(&dependency_archive, b"archive").unwrap();

        let candidates =
            runtime_archive_candidates(Some(&profile_dir.join("joky")), &root, "debug");

        assert_eq!(candidates[0], profile_dir.join(RUNTIME_ARCHIVE_NAME));
        assert_eq!(candidates[1], dependency_archive);
        assert!(candidates
            .iter()
            .all(|path| path.file_name() != Some("libjoky.a".as_ref())));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn release_candidates_do_not_include_debug_archives() {
        let root =
            std::env::temp_dir().join(format!("joky-release-candidates-{}", std::process::id()));
        let release_deps = root.join("target/release/deps");
        std::fs::create_dir_all(&release_deps).unwrap();
        let release_archive = release_deps.join("libjoky_runtime-a1b2c3.a");
        std::fs::write(&release_archive, b"archive").unwrap();

        let candidates = runtime_archive_candidates(None, &root, "release");
        assert_eq!(candidates[0], release_archive);
        assert!(candidates
            .iter()
            .all(|path| !path.to_string_lossy().contains("/debug/")));

        let debug_compiler = root.join("target/debug/joky");
        let candidates = runtime_archive_candidates(Some(&debug_compiler), &root, "release");
        assert!(candidates
            .iter()
            .all(|path| !path.to_string_lossy().contains("/debug/")));
        assert!(candidates.contains(&release_archive));
        std::fs::remove_dir_all(root).unwrap();
    }
}
