//! Per-compilation user module graph. Only the file provider performs I/O.
use super::*;
use crate::{SourceFile, SourceMap};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Default)]
pub(crate) struct FileOverrides<'a> {
    text: Option<&'a BTreeMap<PathBuf, Arc<str>>>,
    recover: bool,
}

impl<'a> FileOverrides<'a> {
    pub(crate) fn new(text: &'a BTreeMap<PathBuf, Arc<str>>, recover: bool) -> Self {
        Self {
            text: Some(text),
            recover,
        }
    }

    pub(super) fn get(self, path: &Path) -> Option<&'a Arc<str>> {
        self.text?.get(path)
    }

    pub(super) fn parse(self, file: &SourceFile) -> Result<Module, crate::parser::Diagnostic> {
        if self.recover && file.path().is_some_and(|path| self.get(path).is_some()) {
            crate::parser::parse_for_editor(file.text(), file.id())
        } else {
            crate::parser::parse_with_source(file.text(), file.id())
        }
    }
}

pub(super) struct FileLoader<'a> {
    pub(super) read: &'a dyn Fn(&str) -> Result<SourceFile, Error>,
    pub(super) rewrite: &'a dyn Fn(&str, &mut Module) -> Result<(), Error>,
    pub(super) overrides: FileOverrides<'a>,
}

pub(crate) fn expand_with(
    module: &Module,
    registry: &TypeRegistry,
    provider: &dyn Fn(&str) -> Result<SourceFile, Error>,
) -> Result<Module, Error> {
    expand_with_tests(
        module,
        registry,
        FileLoader {
            read: provider,
            rewrite: &|_, _| Ok(()),
            overrides: FileOverrides::default(),
        },
        false,
    )
}

pub(super) fn expand_with_tests(
    module: &Module,
    registry: &TypeRegistry,
    loader: FileLoader<'_>,
    include_tests: bool,
) -> Result<Module, Error> {
    check_expression_depth(module)?;
    if module.scope.is_some() {
        return Ok(module.clone());
    }
    let mut sources = module.sources.as_ref().clone();
    let result = (|| {
        let module = prepare_entry(module, include_tests)?;
        let mut graph = registry.standard.for_module(&module, registry.prelude)?;
        for import in &module.imports {
            load(
                &mut graph,
                import,
                registry,
                &loader,
                &mut Vec::new(),
                &mut sources,
            )?;
        }
        finish(module, registry, &graph)
    })();
    result.map_err(|error: Error| error.with_sources(&sources))
}

fn load(
    graph: &mut StandardLibrary,
    import: &Import,
    registry: &TypeRegistry,
    loader: &FileLoader<'_>,
    stack: &mut Vec<String>,
    sources: &mut SourceMap,
) -> Result<(), Error> {
    let path = &import.path;
    if graph.modules.contains_key(path) || registry.hosts.module(path).is_some() {
        return Ok(());
    }
    check_dependency_depth(import.span, path, stack)?;
    if crate::core::source::is_reserved_module_path(path) {
        return Err(error(
            import.span,
            ErrorCode::InvalidImport,
            format!("Unknown built-in module `{path}`"),
        ));
    }
    crate::core::source::validate_module_path(path).map_err(|e| e.with_span(import.span))?;
    let file = (loader.read)(path).map_err(|e| e.with_span(import.span))?;
    sources.insert(file.clone());
    let mut module = loader.overrides.parse(&file)?;
    Arc::make_mut(&mut module.sources).insert(file);
    module
        .items
        .retain(|item| !matches!(item, Item::FnDef(f) if f.is_test()));
    (loader.rewrite)(path, &mut module)?;
    crate::core::json_derive::expand(&mut module)?;
    let standard = registry.standard.for_module(&module, registry.prelude)?;
    graph.modules.extend(standard.modules);
    stack.push(path.clone());
    for dependency in &module.imports {
        load(graph, dependency, registry, loader, stack, sources)?;
    }
    graph.prepare(
        path,
        module,
        registry,
        false,
        sources::ImplicitImports::None,
        sources::SourceKind::User,
    )?;
    stack.pop();
    Ok(())
}

pub(crate) fn resolve_file(path: &Path, registry: &TypeRegistry) -> Result<Module, Error> {
    resolve_entry(path, None, registry, false, FileOverrides::default())
}

pub(crate) fn resolve_file_with_root(
    path: &Path,
    root: &Path,
    registry: &TypeRegistry,
) -> Result<Module, Error> {
    resolve_entry(path, Some(root), registry, false, FileOverrides::default())
}

pub(crate) fn resolve_test_file_with_root(
    path: &Path,
    root: &Path,
    registry: &TypeRegistry,
) -> Result<Module, Error> {
    resolve_entry(path, Some(root), registry, true, FileOverrides::default())
}

pub(crate) fn resolve_entry(
    path: &Path,
    root: Option<&Path>,
    registry: &TypeRegistry,
    include_tests: bool,
    overrides: FileOverrides<'_>,
) -> Result<Module, Error> {
    let canonical = path.canonicalize().map_err(|e| io_error(path, e))?;
    let read = |display: &Path, canonical: &Path| {
        if let Some(text) = overrides.get(canonical) {
            return Ok(SourceFile::from_path(display, canonical, text.clone()));
        }
        read_file(display, canonical)
    };
    let entry = read(path, &canonical)?;
    let path = canonical;
    let root = match root {
        Some(root) => root.canonicalize().map_err(|e| io_error(root, e))?,
        None => path
            .parent()
            .ok_or_else(|| io_error(&path, std::io::ErrorKind::InvalidInput.into()))?
            .to_owned(),
    };
    if !root.is_dir() {
        return Err(Error::new_with_code(
            ErrorPhase::Resolve,
            ErrorCode::InvalidImport,
            "Source root must be a directory",
        ));
    }
    if !path.starts_with(&root) {
        return Err(Error::new_with_code(
            ErrorPhase::Resolve,
            ErrorCode::InvalidImport,
            "Entry file is outside the source root",
        ));
    }
    let mut sources = SourceMap::default();
    sources.insert(entry.clone());
    let mut module = overrides
        .parse(&entry)
        .map_err(|e| Error::from(e).with_sources(&sources))?;
    module.sources = Arc::new(sources);
    let expanded = expand_with_tests(
        &module,
        registry,
        FileLoader {
            read: &|name| {
                let file = root.join(format!("{name}.jk"));
                let file = file.canonicalize().map_err(|e| io_error(&file, e))?;
                if !file.starts_with(&root) {
                    return Err(Error::new_with_code(
                        ErrorPhase::Resolve,
                        ErrorCode::InvalidImport,
                        format!(
                            "Module '{name}' is outside the source root '{}'",
                            root.display()
                        ),
                    ));
                }
                if file == path {
                    Ok(entry.clone())
                } else {
                    read(&file, &file)
                }
            },
            rewrite: &|_, _| Ok(()),
            overrides,
        },
        include_tests,
    )?;
    registry.resolve_aliases(&expanded)
}

fn read_file(path: &Path, canonical: &Path) -> Result<SourceFile, Error> {
    let text = std::fs::read_to_string(canonical).map_err(|e| io_error(path, e))?;
    Ok(SourceFile::from_path(path, canonical, text))
}

fn io_error(path: &Path, cause: std::io::Error) -> Error {
    Error::new_with_code(
        ErrorPhase::Resolve,
        ErrorCode::IoError,
        format!("Cannot read '{}': {cause}", path.display()),
    )
}
