//! Per-compilation user module graph. Only the file provider performs I/O.
use super::*;
use crate::{SourceFile, SourceMap};
use std::path::Path;

pub(crate) fn expand_with(
    module: &Module,
    registry: &TypeRegistry,
    provider: &dyn Fn(&str) -> Result<SourceFile, Error>,
) -> Result<Module, Error> {
    expand_with_tests(module, registry, provider, &|_, _| Ok(()), false)
}

pub(super) fn expand_with_tests(
    module: &Module,
    registry: &TypeRegistry,
    provider: &dyn Fn(&str) -> Result<SourceFile, Error>,
    rewrite: &dyn Fn(&str, &mut Module) -> Result<(), Error>,
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
                provider,
                rewrite,
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
    provider: &dyn Fn(&str) -> Result<SourceFile, Error>,
    rewrite: &dyn Fn(&str, &mut Module) -> Result<(), Error>,
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
    let file = provider(path).map_err(|e| e.with_span(import.span))?;
    sources.insert(file.clone());
    let mut module = crate::parser::parse_with_source(file.text(), file.id())?;
    Arc::make_mut(&mut module.sources).insert(file);
    module
        .items
        .retain(|item| !matches!(item, Item::FnDef(f) if f.is_test()));
    rewrite(path, &mut module)?;
    crate::core::json_derive::expand(&mut module)?;
    let standard = registry.standard.for_module(&module, registry.prelude)?;
    graph.modules.extend(standard.modules);
    stack.push(path.clone());
    for dependency in &module.imports {
        load(
            graph, dependency, registry, provider, rewrite, stack, sources,
        )?;
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
    let canonical = path.canonicalize().map_err(|e| io_error(path, e))?;
    resolve_file_with_root(
        path,
        canonical.parent().expect("a source file has a parent"),
        registry,
    )
}

pub(crate) fn resolve_file_with_root(
    path: &Path,
    root: &Path,
    registry: &TypeRegistry,
) -> Result<Module, Error> {
    resolve_entry(path, root, registry, false)
}

pub(crate) fn resolve_test_file_with_root(
    path: &Path,
    root: &Path,
    registry: &TypeRegistry,
) -> Result<Module, Error> {
    resolve_entry(path, root, registry, true)
}

fn resolve_entry(
    path: &Path,
    root: &Path,
    registry: &TypeRegistry,
    include_tests: bool,
) -> Result<Module, Error> {
    let entry = read_file(path)?;
    let path = path.canonicalize().map_err(|e| io_error(path, e))?;
    let root = root.canonicalize().map_err(|e| io_error(root, e))?;
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
    let mut module = crate::parser::parse_with_source(entry.text(), entry.id())
        .map_err(|e| Error::from(e).with_sources(&sources))?;
    module.sources = Arc::new(sources);
    let expanded = expand_with_tests(
        &module,
        registry,
        &|name| {
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
                read_file(&file)
            }
        },
        &|_, _| Ok(()),
        include_tests,
    )?;
    registry.resolve_aliases(&expanded)
}

fn read_file(path: &Path) -> Result<SourceFile, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    Ok(SourceFile::new(path.display().to_string(), text))
}

fn io_error(path: &Path, cause: std::io::Error) -> Error {
    Error::new_with_code(
        ErrorPhase::Resolve,
        ErrorCode::IoError,
        format!("Cannot read '{}': {cause}", path.display()),
    )
}
