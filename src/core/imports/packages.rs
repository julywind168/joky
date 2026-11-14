//! Explicit package roots and scoped imports, shared by all compilation backends.
use super::{files, FileOverrides, Module, TypeRegistry};
use crate::{EditorSources, Error, ErrorCode, ErrorPhase, SourceMap};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

/// One source package. Dependency aliases refer to indices in `PackageSources`.
#[derive(Clone, Debug)]
pub struct SourcePackage {
    /// Stable package name, independent of dependency aliases and graph order.
    pub name: String,
    pub source_root: PathBuf,
    pub dependencies: BTreeMap<String, usize>,
}

/// A validated local source graph. Index zero is the package being compiled.
/// No network access, implicit package discovery, or runtime capability is added.
#[derive(Clone, Debug)]
pub struct PackageSources {
    packages: Vec<SourcePackage>,
    keys: BTreeMap<String, usize>,
}

fn identity(name: &str) -> String {
    // An injective identifier encoding keeps names stable across graph order,
    // aliases and checkout locations. Raw imports never bypass scoped rewriting.
    format!(
        "__joky_package_{}",
        name.replace('_', "_u").replace('-', "_h")
    )
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Resolve, ErrorCode::InvalidImport, message)
}

fn canonical(path: &Path) -> Result<PathBuf, Error> {
    path.canonicalize()
        .map_err(|error| invalid(format!("Cannot access '{}': {error}", path.display())))
}

impl PackageSources {
    /// Canonicalize roots and validate isolation, dependency aliases and cycles.
    pub fn new(mut packages: Vec<SourcePackage>) -> Result<Self, Error> {
        if packages.len() > 128 {
            return Err(invalid("Package graph exceeds 128 packages"));
        }
        if packages.is_empty() {
            return Err(invalid("A package source graph requires a root package"));
        }
        let mut keys = BTreeMap::new();
        for (index, package) in packages.iter_mut().enumerate() {
            if package.name.is_empty()
                || !package
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                return Err(invalid("Invalid package name"));
            }
            let key = identity(&package.name);
            if keys.insert(key, index).is_some() {
                return Err(invalid(format!("Duplicate package name: {}", package.name)));
            }
            package.source_root = canonical(&package.source_root)?;
            if !package.source_root.is_dir() {
                return Err(invalid("Package source root must be a directory"));
            }
        }
        for (index, package) in packages.iter().enumerate() {
            for other in &packages[..index] {
                if package.source_root.starts_with(&other.source_root)
                    || other.source_root.starts_with(&package.source_root)
                {
                    return Err(invalid(format!(
                        "Package source roots overlap: '{}' and '{}'",
                        package.source_root.display(),
                        other.source_root.display()
                    )));
                }
            }
            for (alias, target) in &package.dependencies {
                crate::core::source::validate_module_path(alias)?;
                if alias.contains('/') || *target >= packages.len() {
                    return Err(invalid(format!(
                        "Invalid package dependency alias or target: {alias}"
                    )));
                }
                if [
                    package.source_root.join(alias),
                    package.source_root.join(format!("{alias}.jk")),
                ]
                .iter()
                .any(|path| std::fs::symlink_metadata(path).is_ok())
                {
                    return Err(invalid(format!(
                        "Dependency alias '{alias}' conflicts with a local module in '{}'",
                        package.source_root.display()
                    )));
                }
            }
        }
        fn visit(
            index: usize,
            packages: &[SourcePackage],
            states: &mut [u8],
            depth: usize,
        ) -> Result<(), Error> {
            if states[index] == 1 {
                return Err(invalid("Cyclic package dependency"));
            }
            if depth >= 128 {
                return Err(invalid("Package dependency depth exceeds 128 levels"));
            }
            if states[index] == 2 {
                return Ok(());
            }
            states[index] = 1;
            for target in packages[index].dependencies.values() {
                visit(*target, packages, states, depth + 1)?;
            }
            states[index] = 2;
            Ok(())
        }
        let mut states = vec![0; packages.len()];
        for index in 0..packages.len() {
            visit(index, &packages, &mut states, 0)?;
        }
        Ok(Self { packages, keys })
    }

    /// Resolve an entry in package zero, excluding test functions.
    pub fn resolve_file(
        &self,
        path: impl AsRef<Path>,
        registry: &TypeRegistry,
    ) -> Result<Module, Error> {
        self.resolve(path.as_ref(), registry, false, FileOverrides::default())
    }

    /// Resolve an entry in package zero, retaining only that file's test functions.
    pub fn resolve_test_file(
        &self,
        path: impl AsRef<Path>,
        registry: &TypeRegistry,
    ) -> Result<Module, Error> {
        self.resolve(path.as_ref(), registry, true, FileOverrides::default())
    }

    /// Resolve existing package sources with text overrides keyed by canonical path.
    pub fn resolve_file_with_overrides(
        &self,
        path: &Path,
        registry: &TypeRegistry,
        overrides: &BTreeMap<PathBuf, Arc<str>>,
    ) -> Result<Module, Error> {
        self.resolve(path, registry, false, FileOverrides::new(overrides, false))
    }

    /// Resolve an editor snapshot, recovering damaged functions in overridden files.
    /// The returned module is for analysis and may contain synthesized bodies.
    pub fn resolve_file_for_editor(
        &self,
        path: &Path,
        registry: &TypeRegistry,
        overrides: &BTreeMap<PathBuf, Arc<str>>,
    ) -> Result<Module, Error> {
        self.resolve(path, registry, false, FileOverrides::new(overrides, true))
    }

    /// Resolve a tracked editor snapshot with reusable parses.
    pub fn resolve_editor_snapshot(
        &self,
        path: &Path,
        registry: &TypeRegistry,
        sources: EditorSources<'_>,
        recover: bool,
    ) -> Result<Module, Error> {
        self.resolve(
            path,
            registry,
            false,
            FileOverrides::editor(sources, recover),
        )
    }

    fn module_path(
        &self,
        package: usize,
        path: &str,
        overrides: FileOverrides<'_>,
    ) -> Result<(String, PathBuf), Error> {
        crate::core::source::validate_module_path(path)?;
        let root = &self.packages[package].source_root;
        let candidate = root.join(format!("{path}.jk"));
        overrides.observe(&candidate);
        let file = canonical(&candidate)?;
        if !file.starts_with(root)
            || !file.is_file()
            || file.extension().is_none_or(|ext| ext != "jk")
        {
            return Err(invalid(format!(
                "Module '{path}' is outside its package source root or is not a .jk file"
            )));
        }
        let relative = file
            .strip_prefix(root)
            .expect("checked containment")
            .with_extension("");
        let relative = relative
            .to_str()
            .ok_or_else(|| invalid("Module paths must be UTF-8"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        crate::core::source::validate_module_path(&relative)?;
        Ok((
            format!("{}/{relative}", identity(&self.packages[package].name)),
            file,
        ))
    }

    fn rewrite(
        &self,
        owner: usize,
        module: &mut Module,
        overrides: FileOverrides<'_>,
    ) -> Result<(), Error> {
        for import in &mut module.imports {
            if crate::core::source::is_reserved_module_path(&import.path) {
                continue;
            }
            let result = (|| {
                let (first, remainder) = import.path.split_once('/').unwrap_or((&import.path, ""));
                let (target, path) =
                    if let Some(target) = self.packages[owner].dependencies.get(first) {
                        if remainder.is_empty() {
                            return Err(invalid(format!(
                                "Package import '{first}' must name a module: {first}/<module>"
                            )));
                        }
                        (*target, remainder)
                    } else {
                        (owner, import.path.as_str())
                    };
                self.module_path(target, path, overrides)
                    .map(|(key, _)| key)
            })()
            .map_err(|error| error.with_span(import.span))?;
            // A canonical file path may differ after following a contained symlink.
            // Keep the source spelling's default alias, not the identity's suffix.
            if import.alias.is_none() {
                import.alias = import.path.rsplit('/').next().map(str::to_owned);
            }
            import.path = result;
        }
        Ok(())
    }

    fn key<'a>(&self, key: &'a str) -> Result<(usize, &'a str), Error> {
        let (prefix, path) = key
            .split_once('/')
            .ok_or_else(|| invalid("Invalid resolved package module"))?;
        let index = *self
            .keys
            .get(prefix)
            .ok_or_else(|| invalid("Invalid resolved package identity"))?;
        Ok((index, path))
    }

    fn resolve(
        &self,
        path: &Path,
        registry: &TypeRegistry,
        tests: bool,
        overrides: FileOverrides<'_>,
    ) -> Result<Module, Error> {
        overrides.observe(path);
        let path = canonical(path)?;
        if !path.starts_with(&self.packages[0].source_root) || !path.is_file() {
            return Err(invalid(
                "Entry file is outside the root package source root",
            ));
        }
        let read = |path: &Path| {
            overrides
                .read(path, path)
                .map_err(|error| invalid(format!("Cannot read '{}': {error}", path.display())))
        };
        let entry = read(&path)?;
        let mut sources = SourceMap::default();
        sources.insert(entry.clone());
        let result = (|| {
            let mut module = overrides.parse(&entry)?;
            module.sources = Arc::new(sources.clone());
            self.rewrite(0, &mut module, overrides)?;
            let expanded = files::expand_with_tests(
                &module,
                registry,
                files::FileLoader {
                    read: &|key| {
                        let (index, module) = self.key(key)?;
                        let (_, file) = self.module_path(index, module, overrides)?;
                        if file == path {
                            Ok(entry.clone())
                        } else {
                            read(&file)
                        }
                    },
                    rewrite: &|key, module| self.rewrite(self.key(key)?.0, module, overrides),
                    overrides,
                },
                tests,
            )?;
            registry.resolve_aliases(&expanded)
        })();
        result.map_err(|error: Error| error.with_sources(&sources))
    }
}
