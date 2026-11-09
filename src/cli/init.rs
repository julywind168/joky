//! Create a small application while preserving existing files.
use super::{cli_argument_error, cli_error_with_code, help, progress, Command, Error, ErrorCode};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

pub(super) struct Options {
    directory: PathBuf,
    name: Option<String>,
}

pub(super) fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Command, Error> {
    let mut directory = None;
    let mut name = None;
    let mut positional = false;
    while let Some(arg) = args.next() {
        if !positional && (arg == "--help" || arg == "-h") {
            return Ok(Command::Help(help::INIT));
        }
        if !positional && arg == "--" {
            positional = true;
        } else if !positional && arg == "--name" {
            if name.is_some() {
                return Err(cli_argument_error("--name was specified more than once"));
            }
            name = Some(
                args.next()
                    .and_then(|s| s.into_string().ok())
                    .ok_or_else(|| cli_argument_error("--name requires a UTF-8 project name"))?,
            );
        } else if !positional && arg.to_string_lossy().starts_with('-') {
            return Err(cli_argument_error(format!(
                "Unknown init option `{}`",
                arg.to_string_lossy()
            )));
        } else if arg.is_empty() || directory.replace(PathBuf::from(arg)).is_some() {
            return Err(cli_argument_error("init accepts one nonempty directory"));
        }
    }
    if name.as_ref().is_some_and(|name| !valid_name(name)) {
        return Err(cli_argument_error(
            "Project name must contain only ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(Command::Init(Options {
        directory: directory.unwrap_or_else(|| PathBuf::from(".")),
        name,
    }))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

pub(super) fn execute(options: &Options) -> u8 {
    let result = (|| -> Result<(), Error> {
        let io = |error: std::io::Error| {
            cli_error_with_code(
                ErrorCode::IoError,
                format!(
                    "Cannot initialize '{}': {error}",
                    options.directory.display()
                ),
            )
        };
        // Check both destinations before creating anything. create_new also
        // protects against another process creating a destination after this check.
        for path in [
            options.directory.join("joky.toml"),
            options.directory.join("src/main.jk"),
        ] {
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    return Err(cli_argument_error(format!(
                        "Refusing to overwrite existing file: {}",
                        path.display()
                    )))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io(error)),
            }
        }
        let source = options.directory.join("src");
        if fs::symlink_metadata(&source).is_ok_and(|meta| meta.is_symlink()) {
            return Err(cli_argument_error(
                "Project src directory must not be a symlink",
            ));
        }
        fs::create_dir_all(&source).map_err(io)?;
        let root = options.directory.canonicalize().map_err(io)?;
        let name = options.name.as_deref().unwrap_or_else(|| {
            root.file_name()
                .and_then(|s| s.to_str())
                .filter(|s| valid_name(s))
                .unwrap_or("joky-app")
        });
        let manifest = format!(
            "[project]\nname = \"{name}\"\nentry = \"src/main.jk\"\nsource-root = \"src\"\n"
        );
        let main = "fn main() {\n    println(\"Hello, Joky!\")\n}\n";
        let mut created = CreatedFiles(Vec::new());
        for (path, contents) in [
            (root.join("src/main.jk"), main),
            (root.join("joky.toml"), manifest.as_str()),
        ] {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(io)?;
            created.0.push(path);
            file.write_all(contents.as_bytes()).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        created.0.clear();
        progress::status("Created", options.directory.display());
        if options.directory == std::path::Path::new(".") {
            eprintln!("Next: joky run");
        } else {
            let directory = options.directory.to_string_lossy().replace('\'', "'\\''");
            eprintln!("Next: joky run -- '{directory}'");
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => super::report(&error),
    }
}

struct CreatedFiles(Vec<PathBuf>);
impl Drop for CreatedFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}
