//! Static checking never creates a VM or publishes build output.
use super::{diagnostic, project, source, Command};
use std::{ffi::OsString, path::Path};

pub(super) fn parse(args: impl Iterator<Item = OsString>) -> Result<Command, joky::Error> {
    Ok(match source::parse(args, "check")? {
        Some(source::Options { path, prelude }) => Command::Check {
            source: path,
            prelude,
        },
        None => Command::Help,
    })
}

pub(super) fn execute(path: &Path, prelude: bool) -> u8 {
    let result = (|| {
        let mut checker = joky::Checker::new();
        checker.set_prelude_enabled(prelude);
        let project = project::Project::load(path)?;
        let types = checker.type_registry();
        let ast = match &project {
            Some(project) => project.resolve(types)?,
            None => types.resolve_file(path)?,
        };
        let entry = if project.is_some() {
            super::validate_program(&ast, types).map(Some)
        } else {
            super::validate_module(&ast, types)
        };
        entry.map_err(|error| error.with_sources(ast.sources()))?;
        checker.check_module(&ast)
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {}", diagnostic::render_error(path, "", &error));
            1
        }
    }
}
