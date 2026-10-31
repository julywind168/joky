//! Shared file/project arguments for static inspection commands.
use super::{cli_argument_error, project};
use std::{ffi::OsString, path::PathBuf};

pub(super) struct Options {
    pub path: PathBuf,
    pub prelude: bool,
}

/// None requests help; otherwise resolve an explicit input or discover a project.
pub(super) fn parse(
    args: impl Iterator<Item = OsString>,
    command: &str,
) -> Result<Option<Options>, joky::Error> {
    let mut source = None;
    let mut prelude = true;
    let mut positional = false;
    for arg in args {
        if !positional && (arg == "--help" || arg == "-h") {
            return Ok(None);
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
                "Unknown {command} option `{}`",
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
    Ok(Some(Options {
        path: source,
        prelude,
    }))
}
