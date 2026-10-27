//! Project manifests, explicit module roots, and relocatable native bundles.
use super::{build, cli_argument_error, cli_error_with_code, EntryPoint};
use joky::{parser::ast::Module, runtime::parallel::ExecutionMode, Error, ErrorCode, TypeRegistry};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

const MANIFEST: &str = "joky.toml";
const MARKER: &str = ".joky-bundle";

pub(super) struct Project {
    pub root: PathBuf,
    pub entry: PathBuf,
    pub source_root: PathBuf,
    name: String,
    resources: BTreeSet<PathBuf>,
}

pub(super) fn discover() -> Option<PathBuf> {
    std::env::current_dir().ok()?.ancestors().find_map(|dir| {
        let path = dir.join(MANIFEST);
        path.is_file().then_some(path)
    })
}

impl Project {
    pub fn load(input: &Path) -> Result<Option<Self>, Error> {
        if !input.is_dir() && input.file_name().is_none_or(|n| n != MANIFEST) {
            return Ok(None);
        }
        let manifest = if input.is_dir() {
            input.join(MANIFEST)
        } else {
            input.into()
        };
        let text = fs::read_to_string(&manifest).map_err(|e| io(&manifest, e))?;
        let mut table = text
            .parse::<toml::Table>()
            .map_err(|e| cli_argument_error(format!("Invalid {}: {e}", manifest.display())))?;
        if let Some(version) = table.remove("version") {
            if version.as_integer() != Some(1) {
                return Err(cli_argument_error(
                    "Unsupported project manifest version; expected 1",
                ));
            }
        }
        let Some(toml::Value::Table(mut project)) = table.remove("project") else {
            return Err(cli_argument_error("joky.toml requires a [project] table"));
        };
        unknown(&table)?;
        let name = string(&mut project, "name", None)?;
        if !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(cli_argument_error(
                "Project name must contain only ASCII letters, digits, '_' or '-'",
            ));
        }
        let entry = string(&mut project, "entry", None)?;
        let source_root = string(&mut project, "source-root", Some("."))?;
        let resources = match project.remove("resources") {
            None => Vec::new(),
            Some(toml::Value::Array(values)) => values
                .into_iter()
                .map(|v| match v {
                    toml::Value::String(s) => Ok(s),
                    _ => Err(cli_argument_error(
                        "Project resources must be an array of paths",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(cli_argument_error(
                    "Project resources must be an array of paths",
                ))
            }
        };
        unknown(&project)?;
        let manifest = manifest.canonicalize().map_err(|e| io(&manifest, e))?;
        let root = manifest.parent().expect("manifest has a parent").to_owned();
        let source_root = contained(&root, &source_root, true)?;
        if !source_root.is_dir() {
            return Err(cli_argument_error(
                "Project source-root must be a directory",
            ));
        }
        let entry = contained(&root, &entry, false)?;
        if !entry.is_file() || entry.extension().is_none_or(|e| e != "jk") {
            return Err(cli_argument_error("Project entry must be a .jk file"));
        }
        if !entry.starts_with(&source_root) {
            return Err(cli_argument_error("Project entry is outside source-root"));
        }
        let mut paths = BTreeSet::new();
        for resource in resources {
            relative(&resource, false)?;
            let path: PathBuf = Path::new(&resource)
                .components()
                .filter(|c| *c != Component::CurDir)
                .collect();
            let first = path.components().next().unwrap().as_os_str();
            if first == name.as_str() || first == MARKER || first == MANIFEST {
                return Err(cli_argument_error(format!(
                    "Resource path conflicts with bundle metadata or executable: {resource}"
                )));
            }
            let mut prefix = root.clone();
            for part in path.components() {
                prefix.push(part);
                if fs::symlink_metadata(&prefix)
                    .map_err(|e| io(&prefix, e))?
                    .is_symlink()
                {
                    return Err(cli_argument_error(
                        "Project resources do not support symlinks",
                    ));
                }
            }
            collect_resources(&root, &path, &mut paths)?;
        }
        Ok(Some(Self {
            root,
            entry,
            source_root,
            name,
            resources: paths,
        }))
    }

    pub fn resolve(&self, types: &TypeRegistry) -> Result<Module, Error> {
        types.resolve_file_with_root(&self.entry, &self.source_root)
    }

    pub fn build(
        &self,
        ast: &Module,
        entry: EntryPoint,
        output: Option<&Path>,
        mode: ExecutionMode,
    ) -> Result<PathBuf, Error> {
        let output = output.map_or_else(|| self.root.join("dist"), PathBuf::from);
        let name = output
            .file_name()
            .ok_or_else(|| cli_argument_error("Project output must name a directory"))?;
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        let parent = parent.canonicalize().map_err(|e| io(parent, e))?;
        let output = parent.join(name);
        if self.root.starts_with(&output) || self.entry.starts_with(&output) {
            return Err(cli_argument_error(
                "Project output must not replace the project or entry directory",
            ));
        }
        let marker = format!("joky-project-v1\n{}\n", self.name);
        check_output(&output, &marker)?;
        // Include compiled inputs in the check: a build must never replace a source tree.
        let snapshots: BTreeMap<_, _> = ast
            .sources()
            .files()
            .filter(|f| Path::new(f.name()).is_absolute())
            .map(|f| (PathBuf::from(f.name()), f.text()))
            .collect();
        if snapshots.keys().any(|p| p.starts_with(&output)) {
            return Err(cli_argument_error(
                "Project output overlaps a compiled source",
            ));
        }
        for relative in &self.resources {
            let path = self.root.join(relative);
            if path.starts_with(&output) || output.starts_with(&path) {
                return Err(cli_argument_error(
                    "Project output overlaps runtime resources",
                ));
            }
        }
        let work = build::WorkDir::new(&parent)?;
        let bundle = work.0.join("bundle");
        fs::create_dir(&bundle).map_err(|e| io(&bundle, e))?;
        for relative in &self.resources {
            let source = self.root.join(relative);
            let destination = bundle.join(relative);
            if source.is_dir() {
                fs::create_dir_all(&destination).map_err(|e| io(&destination, e))?;
            } else {
                fs::create_dir_all(destination.parent().unwrap())
                    .map_err(|e| io(&destination, e))?;
                // If a runtime file was compiled into the host (e.g. its contract),
                // ship the exact compilation snapshot rather than rereading it.
                if let Some(text) = snapshots.get(&source) {
                    fs::write(&destination, text).map_err(|e| io(&destination, e))?;
                } else {
                    fs::copy(&source, &destination).map_err(|e| io(&source, e))?;
                }
            }
        }
        // Check on the destination filesystem too: case-insensitive filesystems
        // may treat differently spelled resource names as the executable/marker.
        if bundle.join(&self.name).exists() || bundle.join(MARKER).exists() {
            return Err(cli_argument_error(
                "Runtime resource conflicts with the executable or bundle metadata",
            ));
        }
        build::build(
            ast,
            entry,
            &self.entry,
            &bundle.join(&self.name),
            mode,
            true,
        )?;
        fs::write(bundle.join(MARKER), &marker).map_err(|e| io(&bundle, e))?;
        check_output(&output, &marker)?;
        let previous = work.0.join("previous");
        let replacing = output.exists();
        if replacing {
            fs::rename(&output, &previous).map_err(|e| io(&output, e))?;
        }
        if let Err(error) = fs::rename(&bundle, &output) {
            if replacing {
                if let Err(restore) = fs::rename(&previous, &output) {
                    // Preserve the last successful build if an external race also
                    // prevents rollback. Never let the temporary-directory guard delete it.
                    std::mem::forget(work);
                    return Err(cli_error_with_code(ErrorCode::IoError, format!(
                        "Cannot publish project: {error}; cannot restore output: {restore}. Previous build retained at {}",
                        previous.display()
                    )));
                }
            }
            return Err(io(&output, error));
        }
        Ok(output)
    }
}

fn check_output(output: &Path, marker: &str) -> Result<(), Error> {
    match fs::symlink_metadata(output) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(output, e)),
        Ok(meta)
            if meta.is_dir()
                && fs::read_to_string(output.join(MARKER)).ok().as_deref() == Some(marker) =>
        {
            Ok(())
        }
        Ok(_) => Err(cli_argument_error(format!(
            "Project output already exists and is not a bundle for this project: {}",
            output.display()
        ))),
    }
}

fn collect_resources(
    root: &Path,
    relative: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), Error> {
    if !paths.insert(relative.to_owned()) {
        return Ok(());
    }
    let path = root.join(relative);
    let meta = fs::symlink_metadata(&path).map_err(|e| io(&path, e))?;
    if meta.is_symlink() || !(meta.is_dir() || meta.is_file()) {
        return Err(cli_argument_error(format!(
            "Project resources must be regular files or directories, without symlinks: {}",
            path.display()
        )));
    }
    if meta.is_dir() {
        for entry in fs::read_dir(&path).map_err(|e| io(&path, e))? {
            let entry = entry.map_err(|e| io(&path, e))?;
            collect_resources(root, &relative.join(entry.file_name()), paths)?;
        }
    }
    Ok(())
}

fn relative(path: &str, allow_root: bool) -> Result<(), Error> {
    if path.is_empty()
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        || (!allow_root && Path::new(path).components().all(|c| c == Component::CurDir))
    {
        return Err(cli_argument_error(format!(
            "Project paths must stay relative to the project directory: {path}"
        )));
    }
    Ok(())
}
fn contained(root: &Path, path: &str, allow_root: bool) -> Result<PathBuf, Error> {
    relative(path, allow_root)?;
    let path = root.join(path);
    let canonical = path.canonicalize().map_err(|e| io(&path, e))?;
    if !canonical.starts_with(root) {
        return Err(cli_argument_error(format!(
            "Project path escapes the project directory: {}",
            path.display()
        )));
    }
    Ok(canonical)
}
fn string(table: &mut toml::Table, key: &str, default: Option<&str>) -> Result<String, Error> {
    match table.remove(key) {
        Some(toml::Value::String(s)) if !s.is_empty() => Ok(s),
        None if default.is_some() => Ok(default.unwrap().into()),
        _ => Err(cli_argument_error(format!(
            "Project {key} must be a nonempty string"
        ))),
    }
}
fn unknown(table: &toml::Table) -> Result<(), Error> {
    if let Some(key) = table.keys().next() {
        return Err(cli_argument_error(format!(
            "Unknown project manifest field: {key}"
        )));
    }
    Ok(())
}
fn io(path: &Path, error: std::io::Error) -> Error {
    cli_error_with_code(
        ErrorCode::IoError,
        format!("Cannot access '{}': {error}", path.display()),
    )
}
