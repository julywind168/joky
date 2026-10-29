//! Static parallel eligibility reports do not execute or publish user code.
use super::{cli_argument_error, diagnostic, project, Command};
use joky::ParallelDisposition;
use std::{ffi::OsString, path::PathBuf};

pub(super) fn parse(args: impl Iterator<Item = OsString>) -> Result<Command, joky::Error> {
    let mut source = None;
    let mut prelude = true;
    let mut positional = false;
    for arg in args {
        if !positional && (arg == "--help" || arg == "-h") {
            return Ok(Command::Help);
        }
        if !positional && arg == "--" {
            positional = true;
            continue;
        }
        if !positional && arg == "--no-prelude" {
            if !prelude {
                return Err(cli_argument_error(
                    "--no-prelude was specified more than once",
                ));
            }
            prelude = false;
        } else if !positional && arg.to_string_lossy().starts_with('-') {
            return Err(cli_argument_error(format!(
                "Unknown explain-parallel option `{}`",
                arg.to_string_lossy()
            )));
        } else if source.replace(PathBuf::from(arg)).is_some() {
            return Err(cli_argument_error(
                "Expected exactly one source file or project",
            ));
        }
    }
    let source = source
        .or_else(project::discover)
        .ok_or_else(|| cli_argument_error("Missing source file or joky.toml project"))?;
    if !source.is_dir()
        && source.file_name().is_none_or(|name| name != "joky.toml")
        && source.extension().is_none_or(|ext| ext != "jk")
    {
        return Err(cli_argument_error(
            "Source files must have the .jk extension",
        ));
    }
    Ok(Command::Explain { source, prelude })
}

pub(super) fn execute(path: &std::path::Path, prelude: bool) -> u8 {
    let result = (|| {
        let mut vm = joky::vm::VmEngine::new();
        vm.set_prelude_enabled(prelude);
        let project = project::Project::load(path)?;
        let ast = match project {
            Some(project) => project.resolve(vm.type_registry())?,
            None => vm.type_registry().resolve_file(path)?,
        };
        super::validate_program(&ast, vm.type_registry())
            .map_err(|e| e.with_sources(ast.sources()))?;
        let report = joky::explain_parallel(&ast, vm.type_registry(), "main")?;
        println!("Parallel eligibility for main (static analysis; program not executed)");
        println!("Eligible describes code and packets, not observed execution. Worker count, dynamic isolate context and borrowed source provenance are runtime conditions.");
        if let Some(error) = &report.entry_error {
            println!("entry: rejected: {}", error.message());
        } else {
            println!("entry: eligible");
        }
        for site in &report.sites {
            let (file, line, column) = report.sources.get(site.span.source_id()).map_or_else(
                || (path.display().to_string(), 1, 1),
                |source| {
                    let offset = site.span.start().min(source.text().len());
                    let prefix = &source.text()[..offset];
                    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
                    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
                    (source.name().into(), line, column)
                },
            );
            let status = match site.disposition {
                ParallelDisposition::Eligible => "eligible",
                ParallelDisposition::RuntimeChecked => "runtime-check",
                ParallelDisposition::Local => "local",
                ParallelDisposition::Rejected => "rejected",
            };
            println!(
                "{file}:{line}:{column}: {} in {}: {status}: {}",
                site.construct, site.function, site.reason
            );
            if matches!(site.construct.as_str(), "join" | "race")
                && site.disposition == ParallelDisposition::Local
            {
                println!("  The whole group stays local; one unsupported branch is enough.");
            }
        }
        if report.sites.is_empty() {
            println!("No concrete reachable join/race/branch/isolate sites.");
        }
        Ok(if report.entry_error.is_some() { 1 } else { 0 })
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {}", diagnostic::render_error(path, "", &error));
            1
        }
    }
}
