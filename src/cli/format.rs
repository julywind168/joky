//! Formatting does not initialize the VM, resolve imports, or execute code.
use super::{cli_argument_error, cli_error_with_code, diagnostic, Command, Error, ErrorCode};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub(super) struct Options {
    source: Option<PathBuf>,
    check: bool,
}

pub(super) fn parse(args: impl Iterator<Item = OsString>) -> Result<Command, Error> {
    let mut source = None;
    let mut check = false;
    let mut stdin = false;
    let mut positional_only = false;
    for arg in args {
        if !positional_only {
            if arg == "--help" || arg == "-h" {
                return Ok(Command::Help);
            }
            if arg == "--" {
                positional_only = true;
                continue;
            }
            if arg == "--check" || arg == "--stdin" {
                let flag = if arg == "--check" {
                    &mut check
                } else {
                    &mut stdin
                };
                if *flag {
                    return Err(cli_argument_error(format!(
                        "Option '{}' supplied more than once",
                        arg.to_string_lossy()
                    )));
                }
                *flag = true;
                continue;
            }
            if arg.to_string_lossy().starts_with('-') {
                return Err(cli_argument_error(format!(
                    "Unknown option '{}' for fmt",
                    arg.to_string_lossy()
                )));
            }
        }
        if source.replace(PathBuf::from(arg)).is_some() {
            return Err(cli_argument_error("fmt accepts exactly one source file"));
        }
    }
    if stdin && source.is_some() {
        return Err(cli_argument_error(
            "--stdin cannot be combined with a source file",
        ));
    }
    if !stdin && source.is_none() {
        return Err(cli_argument_error(
            "Missing source file; use fmt <file.jk> or fmt --stdin",
        ));
    }
    if source
        .as_ref()
        .is_some_and(|path| path.extension().is_none_or(|ext| ext != "jk"))
    {
        return Err(cli_argument_error("Source file must have a .jk extension"));
    }
    Ok(Command::Format(Options { source, check }))
}

pub(super) fn execute(options: &Options) -> u8 {
    let path = options.source.as_deref().unwrap_or(Path::new("<stdin>"));
    let mut source = String::new();
    let result = (|| -> Result<u8, Error> {
        if let Some(path) = &options.source {
            source = fs::read_to_string(path).map_err(|error| io_error("read", path, error))?;
        } else {
            io::stdin()
                .read_to_string(&mut source)
                .map_err(|error| io_error("read", path, error))?;
        }
        let formatted = joky::parser::format(&source)?;
        if options.check {
            if formatted != source {
                eprintln!("Needs formatting: {}", path.display());
                return Ok(1);
            }
        } else if let Some(path) = &options.source {
            if formatted != source {
                replace_file(path, &source, &formatted)?;
            }
        } else {
            io::stdout()
                .lock()
                .write_all(formatted.as_bytes())
                .map_err(|error| io_error("write", Path::new("<stdout>"), error))?;
        }
        Ok(0)
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            let rendered = diagnostic::render_error(path, &source, &error);
            eprintln!("error: {}: {rendered}", path.display());
            1
        }
    }
}

fn io_error(action: &str, path: &Path, error: io::Error) -> Error {
    cli_error_with_code(
        ErrorCode::IoError,
        format!("Cannot {action} '{}': {error}", path.display()),
    )
}

/// Publish only complete output, preserve permissions, and follow symlinks without
/// replacing the link itself. A failed write or rename leaves the source intact.
fn replace_file(path: &Path, original: &str, formatted: &str) -> Result<(), Error> {
    let target = fs::canonicalize(path).map_err(|error| io_error("resolve", path, error))?;
    let permissions = fs::metadata(&target)
        .map_err(|error| io_error("inspect", path, error))?
        .permissions();
    if permissions.readonly() {
        return Err(cli_error_with_code(
            ErrorCode::IoError,
            format!("Cannot format read-only file '{}'", path.display()),
        ));
    }
    let parent = target.parent().expect("a canonical file has a parent");
    let (temporary, mut file) = (0..100)
        .find_map(|attempt| {
            let temporary = parent.join(format!(".joky-fmt-{}-{attempt}.tmp", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => Some(Ok((TemporaryFile(temporary), file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(io_error(
                    "create formatting output beside",
                    path,
                    error,
                ))),
            }
        })
        .unwrap_or_else(|| {
            Err(cli_error_with_code(
                ErrorCode::IoError,
                "Cannot allocate a temporary formatting file",
            ))
        })?;
    file.set_permissions(permissions)
        .map_err(|error| io_error("set permissions for", path, error))?;
    file.write_all(formatted.as_bytes())
        .map_err(|error| io_error("write", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("flush", path, error))?;
    drop(file);
    if fs::read_to_string(&target).map_err(|error| io_error("read", path, error))? != original {
        return Err(cli_error_with_code(
            ErrorCode::IoError,
            format!(
                "Source changed while formatting '{}'; not overwritten",
                path.display()
            ),
        ));
    }
    fs::rename(&temporary.0, &target).map_err(|error| io_error("replace", path, error))?;
    Ok(())
}

struct TemporaryFile(PathBuf);
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
