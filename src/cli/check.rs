//! Static checking never creates a VM or publishes build output.
use super::{cli_argument_error, diagnostic, project, source, Command};
use joky::Error;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

enum Format {
    Text,
    Json,
}

pub(super) enum Options {
    Text(source::Options),
    Json(Result<source::Options, Error>),
}

pub(super) fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Command, Error> {
    let mut format = None;
    let mut input_args = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            input_args.push(arg);
            break;
        }
        if arg == "--" {
            input_args.push(arg);
            input_args.extend(args);
            break;
        }
        if arg == "--diagnostic-format" {
            if format.is_some() {
                return Err(cli_argument_error(
                    "--diagnostic-format was specified more than once",
                ));
            }
            let value = args.next();
            let selected = match value.as_deref().and_then(std::ffi::OsStr::to_str) {
                Some("text") => Format::Text,
                Some("json") => Format::Json,
                _ => {
                    return Err(cli_argument_error(
                        "--diagnostic-format requires text or json",
                    ))
                }
            };
            format = Some(selected);
        } else {
            input_args.push(arg);
        }
    }
    Ok(
        match (format, source::parse(input_args.into_iter(), "check")) {
            (_, Ok(None)) => Command::Help(super::help::CHECK),
            (Some(Format::Json), Ok(Some(input))) => Command::Check(Options::Json(Ok(input))),
            (Some(Format::Json), Err(error)) => Command::Check(Options::Json(Err(error))),
            (_, Ok(Some(input))) => Command::Check(Options::Text(input)),
            (_, Err(error)) => return Err(error),
        },
    )
}

pub(super) fn execute(options: &Options) -> u8 {
    match options {
        Options::Json(input) => {
            let result = match input {
                Ok(input) => check(input),
                Err(error) => Err(error.clone()),
            };
            if let Err(error) = diagnostic::write_json(result.as_ref().err()) {
                eprintln!("error: Cannot write JSON diagnostics: {error}");
                return 1;
            }
            u8::from(result.is_err())
        }
        Options::Text(input) => match check(input) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!(
                    "error: {}",
                    diagnostic::render_error(&input.path, "", &error)
                );
                1
            }
        },
    }
}

fn check(options: &source::Options) -> Result<(), Error> {
    let mut checker = joky::Checker::new();
    checker.set_prelude_enabled(options.prelude);
    visit_with_overrides(
        &options.path,
        &checker,
        &BTreeMap::new(),
        |ast, validation| {
            validation?;
            checker.check_module(ast)
        },
    )
}

pub(super) fn visit_with_overrides(
    path: &Path,
    checker: &joky::Checker,
    overrides: &BTreeMap<PathBuf, Arc<str>>,
    mut visit: impl FnMut(&joky::parser::ast::Module, Result<(), Error>) -> Result<(), Error>,
) -> Result<(), Error> {
    let project = project::Project::load(path)?;
    let types = checker.type_registry();
    let mut validate = |ast: &joky::parser::ast::Module, entry: bool| {
        let validation = if entry {
            super::validate_program(ast, types).map(Some)
        } else {
            super::validate_module(ast, types)
        };
        visit(
            ast,
            validation
                .map(|_| ())
                .map_err(|error| error.with_sources(ast.sources())),
        )
    };
    match project {
        Some(project) => {
            if let Some(entry) = &project.entry {
                return validate(
                    &project.resolve_with_overrides(types, entry, overrides)?,
                    true,
                );
            }
            for file in project.source_files()? {
                validate(
                    &project.resolve_with_overrides(types, &file, overrides)?,
                    false,
                )?;
            }
            Ok(())
        }
        None => validate(
            &types.resolve_file_with_overrides(path, None, overrides)?,
            false,
        ),
    }
}
