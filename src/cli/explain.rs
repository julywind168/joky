//! Static parallel eligibility reports do not execute or publish user code.
use super::{diagnostic, project, source, Command};
use joky::ParallelDisposition;
use std::ffi::OsString;

pub(super) fn parse(args: impl Iterator<Item = OsString>) -> Result<Command, joky::Error> {
    Ok(match source::parse(args, "explain-parallel")? {
        Some(source::Options { path, prelude }) => Command::Explain {
            source: path,
            prelude,
        },
        None => Command::Help,
    })
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
        if !report.sites.is_empty() {
            println!("Implicit alloc on parallel workers shares the root task budget until task completion. Use an explicit arena for per-batch reclamation.");
        }
        for site in &report.sites {
            let (file, line, column) = report.sources.get(site.span.source_id()).map_or_else(
                || (path.display().to_string(), 1, 1),
                |source| {
                    let (line, column) = site.span.line_column(source.text());
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
