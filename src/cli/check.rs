//! Static checking never creates a VM or publishes build output.
use super::{cli_argument_error, diagnostic, project, source, Command};
use joky::Error;
use std::ffi::OsString;

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
    let path = &options.path;
    let mut checker = joky::Checker::new();
    checker.set_prelude_enabled(options.prelude);
    let project = project::Project::load(path)?;
    let types = checker.type_registry();
    if let Some(project) = &project {
        if project.entry.is_none() {
            for file in project.source_files()? {
                let ast = project.resolve_source(types, &file, false)?;
                super::validate_module(&ast, types).map_err(|e| e.with_sources(ast.sources()))?;
                checker.check_module(&ast)?;
            }
            return Ok(());
        }
    }
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
}
