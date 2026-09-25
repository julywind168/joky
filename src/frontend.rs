//! Reusable source-to-MIR frontend. No JIT, native emission or runtime setup.
use crate::hir::CoreProgram;
use crate::linker::Linker;
use crate::mir::passes::MirPassManager;
use crate::mir::MirProgram;
use crate::module::{
    ExportedSymbol, ModuleArtifact, ModuleCache, ModuleCompileContext, ModuleGraph,
    ModuleGraphCache, ModuleMetadata, ModuleSourceUnit, StableId, SymbolId, MODULE_MIR_FEATURES,
};
use crate::{sema, syntax, Diagnostic};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Debug builds keep huge frames for `check_expression` / `lower_expr`.
/// Deep `if` / `match` trees and leftover recursive walks need a dedicated
/// stack; left-associated operator chains are iterated and do not rely on this.
const COMPILER_STACK_SIZE: usize = 32 * 1024 * 1024;

fn with_compiler_stack<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    thread_local! {
        static ON_COMPILER_STACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if ON_COMPILER_STACK.with(std::cell::Cell::get) {
        return f();
    }
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("joky-compile".into())
            .stack_size(COMPILER_STACK_SIZE)
            .spawn_scoped(scope, || {
                ON_COMPILER_STACK.with(|flag| flag.set(true));
                f()
            })
            .expect("compiler stack thread")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}
mod generics;
mod module_context;
use module_context::ModuleTypeSnapshots;

/// Work performed by a successful check, including generic instances and
/// source-graph reuse in a long-lived frontend session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckReport {
    pub source_modules: usize,
    pub generic_instances: usize,
    pub cache_hits: usize,
    pub compilations: usize,
    pub graph_cache_hits: usize,
    pub graph_cache_misses: usize,
}

/// Result of a collecting frontend check. Independent module failures are
/// reported together; dependent modules are skipped after an upstream error.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckResult {
    pub ok: bool,
    pub report: Option<CheckReport>,
    pub diagnostics: Vec<FrontendDiagnostic>,
}

/// One source snapshot supplied to [`Frontend::check_sources`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
}

impl SourceFile {
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: text.into(),
        }
    }
}

/// One frontend diagnostic with its module identity attached.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FrontendDiagnostic {
    pub path: Option<PathBuf>,
    pub stage: crate::Stage,
    pub message: String,
    pub span: Option<crate::Span>,
}

/// Reusable frontend session. Each check replaces its diagnostics and cache events.
/// Constructing this service does not initialize a native backend or runtime.
#[derive(Default)]
pub struct Frontend {
    pub(crate) module_cache_hits: usize,
    pub(crate) module_compilations: usize,
    pub(crate) graph_cache_hits: usize,
    pub(crate) graph_cache_misses: usize,
    pub(crate) module_events: Vec<String>,
    graph_cache: ModuleGraphCache,
    diagnostic_source: Option<(PathBuf, String)>,
    last_diagnostic: Option<FrontendDiagnostic>,
}

impl Frontend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop parsed module metadata retained by this reusable frontend.
    /// Disk MIR artifacts are unaffected and can still be reused by the next
    /// check. This is useful when an editor session changes package roots.
    pub fn clear_incremental_cache(&mut self) {
        self.graph_cache.clear();
    }

    pub fn take_module_events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.module_events)
    }

    /// Source for the last diagnostic, when attributable to a source module.
    pub fn take_diagnostic_source(&mut self) -> Option<(PathBuf, String)> {
        self.diagnostic_source.take()
    }

    /// Drain the structured form of the last failed frontend operation.
    pub fn take_diagnostic(&mut self) -> Option<FrontendDiagnostic> {
        self.last_diagnostic.take()
    }

    pub(crate) fn reset(&mut self) {
        self.reset_compile_state();
        self.graph_cache_hits = 0;
        self.graph_cache_misses = 0;
    }

    fn reset_compile_state(&mut self) {
        self.module_cache_hits = 0;
        self.module_compilations = 0;
        self.module_events.clear();
        self.diagnostic_source = None;
        self.last_diagnostic = None;
    }

    fn load_sources_graph(
        &mut self,
        entry: &Path,
        package_root: &Path,
        snapshots: &[(PathBuf, String)],
    ) -> Result<ModuleGraph, crate::module::ModuleLoadError> {
        let graph = ModuleGraph::load_with_sources_cached(
            entry,
            package_root,
            snapshots,
            &mut self.graph_cache,
        );
        let (hits, misses) = self.graph_cache.take_stats();
        self.graph_cache_hits += hits;
        self.graph_cache_misses += misses;
        graph
    }

    /// Check a complete dependency graph through linked MIR verification.
    /// Libraries without `main` are accepted. `None` disables disk caching.
    /// Does not execute code, load foreign libraries, or emit native objects.
    pub fn check(
        &mut self,
        graph: &ModuleGraph,
        cache_dir: Option<&Path>,
    ) -> Result<CheckReport, Diagnostic> {
        self.reset();
        self.check_graph(graph, cache_dir)
    }

    /// Check an in-memory source snapshot. Paths use the same package/import
    /// rules as [`ModuleGraph::load`]; supplied text overrides disk files so an
    /// editor can check unsaved buffers without writing them out.
    pub fn check_sources(
        &mut self,
        entry: impl AsRef<Path>,
        package_root: impl AsRef<Path>,
        sources: &[SourceFile],
        cache_dir: Option<&Path>,
    ) -> Result<CheckReport, Diagnostic> {
        self.reset();
        let snapshots = sources
            .iter()
            .map(|source| (source.path.clone(), source.text.clone()))
            .collect::<Vec<_>>();
        let graph = match self.load_sources_graph(entry.as_ref(), package_root.as_ref(), &snapshots)
        {
            Ok(graph) => graph,
            Err(crate::module::ModuleLoadError::Diagnostic {
                path,
                source,
                diagnostic,
            }) => {
                self.diagnostic_source = Some((path, source));
                self.remember_diagnostic(&diagnostic, None);
                return Err(diagnostic);
            }
            Err(crate::module::ModuleLoadError::Message(message)) => {
                let diagnostic = Diagnostic::codegen(message);
                self.remember_diagnostic(
                    &diagnostic,
                    Some((entry.as_ref().to_path_buf(), String::new())),
                );
                return Err(diagnostic);
            }
        };
        self.check_graph(&graph, cache_dir)
    }

    /// Check while collecting diagnostics from independent modules. This is
    /// the machine-facing counterpart to [`Frontend::check`], which preserves
    /// its single `Result` error for existing embedders.
    pub fn check_all(&mut self, graph: &ModuleGraph, cache_dir: Option<&Path>) -> CheckResult {
        self.reset();
        self.check_all_graph(graph, cache_dir)
    }

    /// Collect diagnostics from an in-memory source snapshot.
    pub fn check_sources_all(
        &mut self,
        entry: impl AsRef<Path>,
        package_root: impl AsRef<Path>,
        sources: &[SourceFile],
        cache_dir: Option<&Path>,
    ) -> CheckResult {
        self.reset();
        let snapshots = sources
            .iter()
            .map(|source| (source.path.clone(), source.text.clone()))
            .collect::<Vec<_>>();
        let graph = match self.load_sources_graph(entry.as_ref(), package_root.as_ref(), &snapshots)
        {
            Ok(graph) => graph,
            Err(crate::module::ModuleLoadError::Diagnostic {
                path,
                source,
                diagnostic,
            }) => {
                self.diagnostic_source = Some((path.clone(), source));
                let structured = self.structured_diagnostic(&diagnostic, Some(path));
                self.last_diagnostic = Some(structured.clone());
                return CheckResult {
                    ok: false,
                    report: None,
                    diagnostics: vec![structured],
                };
            }
            Err(crate::module::ModuleLoadError::Message(message)) => {
                let diagnostic = Diagnostic::codegen(message);
                let structured =
                    self.structured_diagnostic(&diagnostic, Some(entry.as_ref().to_path_buf()));
                self.last_diagnostic = Some(structured.clone());
                return CheckResult {
                    ok: false,
                    report: None,
                    diagnostics: vec![structured],
                };
            }
        };
        self.check_all_graph(&graph, cache_dir)
    }

    fn check_all_graph(&mut self, graph: &ModuleGraph, cache_dir: Option<&Path>) -> CheckResult {
        let fallback = graph.source(crate::module::ModuleId(0)).ok().map(|source| {
            (
                graph.module(crate::module::ModuleId(0)).path.clone(),
                source,
            )
        });
        let units = match Self::source_units(graph, crate::AotTarget::native(), false) {
            Ok(units) => units,
            Err(diagnostic) => {
                let structured = self.structured_diagnostic(
                    &diagnostic,
                    fallback.as_ref().map(|(path, _)| path.clone()),
                );
                self.last_diagnostic = Some(structured.clone());
                return CheckResult {
                    ok: false,
                    report: None,
                    diagnostics: vec![structured],
                };
            }
        };
        let cache = cache_dir
            .map(ModuleCache::new)
            .unwrap_or_else(ModuleCache::disabled);
        let (artifacts, diagnostics) =
            self.compile_units_collect(&units, graph, &graph.metadata_map(), &cache);
        if !diagnostics.is_empty() {
            self.last_diagnostic = diagnostics.first().cloned();
            return CheckResult {
                ok: false,
                report: None,
                diagnostics,
            };
        }
        let report = CheckReport {
            source_modules: units.len(),
            generic_instances: artifacts.len() - units.len(),
            cache_hits: self.module_cache_hits,
            compilations: self.module_compilations,
            graph_cache_hits: self.graph_cache_hits,
            graph_cache_misses: self.graph_cache_misses,
        };
        match Self::link(artifacts) {
            Ok(_) => CheckResult {
                ok: true,
                report: Some(report),
                diagnostics: Vec::new(),
            },
            Err(diagnostic) => {
                let structured = self.structured_diagnostic(
                    &diagnostic,
                    fallback.as_ref().map(|(path, _)| path.clone()),
                );
                self.last_diagnostic = Some(structured.clone());
                CheckResult {
                    ok: false,
                    report: None,
                    diagnostics: vec![structured],
                }
            }
        }
    }

    fn check_graph(
        &mut self,
        graph: &ModuleGraph,
        cache_dir: Option<&Path>,
    ) -> Result<CheckReport, Diagnostic> {
        let fallback = graph.source(crate::module::ModuleId(0)).ok().map(|source| {
            (
                graph.module(crate::module::ModuleId(0)).path.clone(),
                source,
            )
        });
        let units = match Self::source_units(graph, crate::AotTarget::native(), false) {
            Ok(units) => units,
            Err(diagnostic) => {
                self.remember_diagnostic(&diagnostic, fallback.clone());
                return Err(diagnostic);
            }
        };
        let cache = cache_dir
            .map(ModuleCache::new)
            .unwrap_or_else(ModuleCache::disabled);
        let artifacts = match self.compile_units(&units, graph, &graph.metadata_map(), &cache) {
            Ok(artifacts) => artifacts,
            Err(diagnostic) => {
                self.remember_diagnostic(&diagnostic, fallback.clone());
                return Err(diagnostic);
            }
        };
        let report = CheckReport {
            source_modules: units.len(),
            generic_instances: artifacts.len() - units.len(),
            cache_hits: self.module_cache_hits,
            compilations: self.module_compilations,
            graph_cache_hits: self.graph_cache_hits,
            graph_cache_misses: self.graph_cache_misses,
        };
        match Self::link(artifacts) {
            Ok(_) => Ok(report),
            Err(diagnostic) => {
                self.remember_diagnostic(&diagnostic, fallback);
                Err(diagnostic)
            }
        }
    }

    fn remember_diagnostic(
        &mut self,
        diagnostic: &Diagnostic,
        fallback: Option<(PathBuf, String)>,
    ) {
        self.last_diagnostic = Some(
            self.structured_diagnostic(
                diagnostic,
                self.diagnostic_source
                    .as_ref()
                    .map(|(path, _)| path.clone())
                    .or_else(|| fallback.as_ref().map(|(path, _)| path.clone())),
            ),
        );
    }

    fn structured_diagnostic(
        &self,
        diagnostic: &Diagnostic,
        path: Option<PathBuf>,
    ) -> FrontendDiagnostic {
        FrontendDiagnostic {
            path,
            stage: diagnostic.stage(),
            message: diagnostic.message().to_owned(),
            span: diagnostic.span(),
        }
    }

    pub(crate) fn source_units(
        graph: &ModuleGraph,
        target: crate::AotTarget,
        require_main: bool,
    ) -> Result<Vec<ModuleSourceUnit>, Diagnostic> {
        if require_main
            && !graph
                .module(crate::module::ModuleId(0))
                .export_signatures
                .get("main")
                .is_some_and(|signature| signature.starts_with("fn("))
        {
            return Err(crate::diagnostic::SemanticError::MissingMainFunction.into());
        }
        let units = graph
            .source_units(
                env!("JOKY_BUILD_ID"),
                target.triple(),
                target.pointer_width(),
                MODULE_MIR_FEATURES,
            )
            .map_err(Diagnostic::codegen)?;
        graph
            .verify_source_units(&units)
            .map_err(Diagnostic::codegen)?;
        Ok(units)
    }

    pub(crate) fn link(artifacts: Vec<ModuleArtifact>) -> Result<MirProgram, Diagnostic> {
        let linked = Linker::new(artifacts).link().map_err(Diagnostic::codegen)?;
        let mut mir = linked.mir;
        drop(linked.artifacts);
        MirPassManager::default_pipeline().run(&mut mir)?;
        Ok(mir)
    }

    pub(crate) fn lower_program(source: &str) -> Result<MirProgram, Diagnostic> {
        with_compiler_stack(|| {
            let mut program = syntax::parse_program(source)?;
            select_target_externs(&mut program, std::env::consts::OS)?;
            let types = sema::check_program(&program)?;
            let core = CoreProgram::lower(program, types)?;
            let mut mir = MirProgram::lower(&core)?;
            drop(core);
            MirPassManager::default_pipeline().run(&mut mir)?;
            Ok(mir)
        })
    }
    /// Compiles a single module's source into a MIR artifact.
    /// MirFunctionId and type indices are only valid within this artifact;
    /// cross-module references are handled by the linker
    pub(crate) fn compile_module(
        &self,
        unit: &ModuleSourceUnit,
        context: &ModuleCompileContext,
    ) -> Result<ModuleArtifact, Diagnostic> {
        with_compiler_stack(|| {
            unit.verify().map_err(Diagnostic::codegen)?;
            let (mut program, _) = syntax::parse_program_resources(
                &unit.source,
                if context.source_path.is_empty() {
                    "<source>"
                } else {
                    &context.source_path
                },
                unit.resources
                    .iter()
                    .map(|(name, resource)| (name.clone(), resource.data.clone()))
                    .collect(),
            )?;
            select_target_externs(
                &mut program,
                if context.target_os.is_empty() {
                    std::env::consts::OS
                } else {
                    &context.target_os
                },
            )?;
            self.compile_module_program(unit, context, program)
        })
    }

    fn compile_module_program(
        &self,
        unit: &ModuleSourceUnit,
        context: &ModuleCompileContext,
        program: syntax::Program,
    ) -> Result<ModuleArtifact, Diagnostic> {
        let mut types = sema::check_module_with_context(&program, context)?;
        let definition_module = context.definition_module.unwrap_or(unit.metadata.stable_id);
        types.prepare_generic_symbols(definition_module);
        let template_program = (!types.interface.public_templates.is_empty()
            || types.interface.type_program.is_some())
        .then(|| program.clone());
        let core = CoreProgram::lower(program, types)?;
        let types = core.types();
        let mut mir = MirProgram::lower(&core)?;
        for function in &mut mir.functions {
            function.source.module = Some(definition_module);
        }
        if unit.metadata.stable_id != definition_module {
            if let Some(instance) = mir.functions.iter_mut().find(|f| f.name == "__instance") {
                instance.is_suspending = true;
            }
        }
        MirPassManager::default_pipeline().run(&mut mir)?;
        let imports = collect_imports(&mir);
        let region_contracts = crate::mir::regions::summarize(&mir)?;
        let mut interface = types.interface.clone();
        let mut exports = build_exports(&mir, &interface, &unit.metadata)?;
        let local_methods = mir
            .functions
            .iter()
            .filter(|f| f.name.starts_with("@method/") || f.name.starts_with("@static_method/"))
            .map(|f| crate::module::generics::MethodAbi {
                receiver: mir
                    .types
                    .static_method_target(&f.name)
                    .unwrap_or_else(|| f.parameters[0].ty),
                receiver_mode: if f.name.starts_with("@static_method/") {
                    syntax::ReceiverMode::Static
                } else if f.parameters[0].ownership == crate::mir::MirOwnership::Borrowed {
                    syntax::ReceiverMode::Borrowed
                } else {
                    syntax::ReceiverMode::Owned
                },
                name: f.name.rsplit('/').next().unwrap().into(),
                symbol: SymbolId {
                    module: definition_module,
                    name: f.name.clone(),
                },
                abi: sema::ExternalFunction {
                    region_contract: region_contracts.get(&f.id).cloned(),
                    parameters: f
                        .parameters
                        .iter()
                        .map(|p| (p.name.clone(), p.ty))
                        .collect(),
                    parameter_borrows: f
                        .parameters
                        .iter()
                        .map(|p| p.ownership == crate::mir::MirOwnership::Borrowed)
                        .collect(),
                    return_type: f.return_type,
                    suspends: f.is_suspending,
                },
                effects: core
                    .functions()
                    .iter()
                    .find(|function| function.name == f.name)
                    .into_iter()
                    .flat_map(|function| function.used_effects.iter())
                    .filter_map(|op| {
                        mir.types
                            .effects()
                            .effect(op.effect)
                            .map(|e| e.name.clone())
                    })
                    .collect(),
                declared_effects: f
                    .declared_effects
                    .iter()
                    .map(|group| mir.types.effects().effect(group).unwrap().name.clone())
                    .collect(),
            })
            .collect::<Vec<_>>();
        interface.methods.extend(local_methods);
        if definition_module == unit.metadata.stable_id {
            for f in mir
                .functions
                .iter()
                .filter(|f| f.name.starts_with("@method/") || f.name.starts_with("@static_method/"))
            {
                exports.insert(
                    f.name.clone(),
                    ExportedSymbol::Function {
                        mir_index: f.id.0,
                        signature: "method".into(),
                    },
                );
            }
        }
        mir.types.qualify_nominal_types(definition_module);
        interface.pending_functions = mir
            .functions
            .iter()
            .filter(|f| f.external_symbol.is_none() && f.receiver.is_none())
            .map(|f| (f.name.clone(), f.is_suspending))
            .collect();
        interface.public_abis = mir
            .functions
            .iter()
            .filter(|f| {
                f.external_symbol.is_none()
                    && f.receiver.is_none()
                    && f.visibility == syntax::Visibility::Public
            })
            .map(|f| {
                (
                    f.name.clone(),
                    sema::ExternalFunction {
                        region_contract: region_contracts.get(&f.id).cloned(),
                        parameters: f
                            .parameters
                            .iter()
                            .map(|p| (p.name.clone(), p.ty))
                            .collect(),
                        parameter_borrows: f
                            .parameters
                            .iter()
                            .map(|p| p.ownership == crate::mir::MirOwnership::Borrowed)
                            .collect(),
                        return_type: f.return_type,
                        suspends: f.is_suspending,
                    },
                )
            })
            .collect();
        let mut metadata = unit.metadata.clone();
        // A body change can change lifetime constraints without changing its
        // source signature. Include verified contracts in dependency cache keys.
        let mut contracts = mir
            .functions
            .iter()
            .filter(|f| f.external_symbol.is_none())
            .filter(|f| !region_contracts[&f.id].is_trivial())
            .map(|f| (&f.name, &region_contracts[&f.id]))
            .collect::<Vec<_>>();
        contracts.sort_by_key(|(name, _)| *name);
        if !contracts.is_empty() {
            let payload = bincode::serialize(&(metadata.abi_hash, contracts))
                .map_err(|error| Diagnostic::codegen(error.to_string()))?;
            metadata.abi_hash = crate::module::source_fingerprint(&payload);
        }
        if !interface.methods.is_empty() {
            let mut methods = interface
                .methods
                .iter()
                .map(|m| {
                    (
                        &m.name,
                        format!("{:?}", m.receiver_mode),
                        mir.types.stable_type_key(m.receiver, definition_module),
                        m.abi
                            .parameters
                            .iter()
                            .map(|(name, ty)| {
                                (name, mir.types.stable_type_key(*ty, definition_module))
                            })
                            .collect::<Vec<_>>(),
                        &m.abi.parameter_borrows,
                        mir.types
                            .stable_type_key(m.abi.return_type, definition_module),
                        m.abi.suspends,
                        &m.effects,
                        &m.declared_effects,
                    )
                })
                .collect::<Vec<_>>();
            methods.sort();
            let payload = bincode::serialize(&(metadata.abi_hash, methods))
                .map_err(|e| Diagnostic::codegen(e.to_string()))?;
            metadata.abi_hash = crate::module::source_fingerprint(&payload);
        }
        if !types.trait_implementations.is_empty() {
            let mut implementations = Vec::new();
            for (name, values) in &types.trait_implementations {
                for ty in values {
                    implementations.push((name, types.stable_type_key(*ty, definition_module)));
                }
            }
            implementations.sort();
            let payload = bincode::serialize(&(metadata.abi_hash, implementations))
                .map_err(|e| Diagnostic::codegen(e.to_string()))?;
            metadata.abi_hash = crate::module::source_fingerprint(&payload);
        }
        if let Some(program) = &template_program {
            let payload = bincode::serialize(&(metadata.abi_hash, program))
                .map_err(|error| Diagnostic::codegen(error.to_string()))?;
            metadata.abi_hash = crate::module::source_fingerprint(&payload);
        }
        if !types.interface.public_constants.is_empty() {
            let mut constants = types
                .interface
                .public_constants
                .iter()
                .map(|(name, value)| Ok((name, value.canonical_bytes(types, definition_module)?)))
                .collect::<Result<Vec<_>, Diagnostic>>()?;
            constants.sort_by_key(|(name, _)| *name);
            let payload = bincode::serialize(&(metadata.abi_hash, constants))
                .map_err(|error| Diagnostic::codegen(error.to_string()))?;
            metadata.abi_hash = crate::module::source_fingerprint(&payload);
        }
        metadata.drop_glue = mir.types.module_drop_glue();
        let mut pending = exports
            .keys()
            .filter(|name| interface.pending_functions.get(*name) == Some(&true))
            .cloned()
            .collect::<Vec<_>>();
        pending.sort();
        if !pending.is_empty() {
            metadata.abi_hash = crate::module::source_fingerprint(
                format!("{}:pending:{}", metadata.abi_hash, pending.join(",")).as_bytes(),
            );
        }
        if !metadata.dependencies.is_empty() {
            metadata.abi_hash = crate::module::source_fingerprint(
                format!("{}:{:?}", metadata.abi_hash, metadata.dependencies).as_bytes(),
            );
        }
        if unit.id != crate::module::ModuleId(0) || unit.metadata.stable_id != definition_module {
            for function in &mut mir.functions {
                if function.receiver.is_none() && function.name == "main" {
                    function.name = format!("@local/{:016x}/main", definition_module.0);
                }
            }
        }
        Ok(ModuleArtifact {
            metadata,
            mir,
            interface,
            imports,
            exports,
            template_program,
        })
    }

    /// Compiles each module in dependency order and writes `.jabi` metadata to `cache_dir`
    #[cfg(test)]
    pub(crate) fn compile_with_cache(
        &mut self,
        units: &[ModuleSourceUnit],
        graph: &ModuleGraph,
        metadata_map: &HashMap<StableId, ModuleMetadata>,
        cache_dir: &Path,
    ) -> Result<Vec<ModuleArtifact>, Diagnostic> {
        let cache = ModuleCache::new(cache_dir);
        self.compile_units(units, graph, metadata_map, &cache)
    }

    pub(crate) fn compile_units(
        &mut self,
        units: &[ModuleSourceUnit],
        graph: &ModuleGraph,
        metadata_map: &HashMap<StableId, ModuleMetadata>,
        cache: &ModuleCache,
    ) -> Result<Vec<ModuleArtifact>, Diagnostic> {
        self.reset_compile_state();
        let mut artifacts: Vec<ModuleArtifact> = Vec::with_capacity(units.len());
        let mut snapshots = ModuleTypeSnapshots::default();
        for unit in units {
            let mut unit = unit.clone();
            for (id, hash) in &mut unit.metadata.dependencies {
                if let Some(dependency) = artifacts.iter().find(|a| a.metadata.stable_id == *id) {
                    *hash = dependency.metadata.abi_hash;
                }
            }
            unit.cache_key.dependency_abi_hashes = unit
                .metadata
                .dependencies
                .iter()
                .map(|(_, hash)| *hash)
                .collect();
            unit.cache_key.dependency_abi_hashes.sort_unstable();
            let artifact = if let Some(artifact) = cache
                .load_artifact(&unit.cache_key)
                .map_err(Diagnostic::codegen)?
            {
                self.module_cache_hits += 1;
                self.module_events
                    .push(format!("hit {}", graph.module(unit.id).path.display()));
                artifact
            } else {
                self.module_compilations += 1;
                self.module_events.push(format!(
                    "compile {}: {}",
                    graph.module(unit.id).path.display(),
                    cache.miss_reason(&unit.cache_key)
                ));
                let mut context = graph.compile_context(unit.id, metadata_map);
                snapshots.populate(&mut context, &artifacts);
                let artifact = self.compile_module(&unit, &context).inspect_err(|_| {
                    self.diagnostic_source =
                        Some((graph.module(unit.id).path.clone(), unit.source.clone()));
                })?;
                cache
                    .store_artifact(&unit.cache_key, &artifact)
                    .map_err(Diagnostic::codegen)?;
                artifact
            };
            cache
                .store(&unit.cache_key, &artifact.metadata)
                .map_err(Diagnostic::codegen)?;
            artifacts.push(artifact);
        }
        self.compile_generic_instances(
            units,
            graph,
            metadata_map,
            cache,
            &mut artifacts,
            &mut snapshots,
        )?;
        Ok(artifacts)
    }

    fn compile_units_collect(
        &mut self,
        units: &[ModuleSourceUnit],
        graph: &ModuleGraph,
        metadata_map: &HashMap<StableId, ModuleMetadata>,
        cache: &ModuleCache,
    ) -> (Vec<ModuleArtifact>, Vec<FrontendDiagnostic>) {
        self.reset_compile_state();
        let mut artifacts = Vec::with_capacity(units.len());
        let mut failed = HashSet::<StableId>::new();
        let mut diagnostics = Vec::new();
        let mut snapshots = ModuleTypeSnapshots::default();

        for original in units {
            let mut unit = original.clone();
            if unit
                .metadata
                .dependencies
                .iter()
                .any(|(id, _)| failed.contains(id))
            {
                // The upstream module already has the useful diagnostic. Do
                // not turn its missing interface into a cascade of noise.
                continue;
            }
            for (id, hash) in &mut unit.metadata.dependencies {
                if let Some(dependency) = artifacts
                    .iter()
                    .find(|a: &&ModuleArtifact| a.metadata.stable_id == *id)
                {
                    *hash = dependency.metadata.abi_hash;
                }
            }
            unit.cache_key.dependency_abi_hashes = unit
                .metadata
                .dependencies
                .iter()
                .map(|(_, hash)| *hash)
                .collect();
            unit.cache_key.dependency_abi_hashes.sort_unstable();

            let mut should_store_artifact = false;
            let artifact = match cache.load_artifact(&unit.cache_key) {
                Ok(Some(artifact)) => {
                    self.module_cache_hits += 1;
                    self.module_events
                        .push(format!("hit {}", graph.module(unit.id).path.display()));
                    artifact
                }
                Ok(None) => {
                    should_store_artifact = true;
                    self.module_compilations += 1;
                    self.module_events.push(format!(
                        "compile {}: {}",
                        graph.module(unit.id).path.display(),
                        cache.miss_reason(&unit.cache_key)
                    ));
                    let mut context = graph.compile_context(unit.id, metadata_map);
                    snapshots.populate(&mut context, &artifacts);
                    match self.compile_module(&unit, &context) {
                        Ok(artifact) => artifact,
                        Err(diagnostic) => {
                            let path = graph.module(unit.id).path.clone();
                            if self.diagnostic_source.is_none() {
                                self.diagnostic_source = Some((path.clone(), unit.source.clone()));
                            }
                            diagnostics.push(self.structured_diagnostic(&diagnostic, Some(path)));
                            failed.insert(unit.metadata.stable_id);
                            continue;
                        }
                    }
                }
                Err(error) => {
                    let diagnostic = Diagnostic::codegen(error);
                    let path = graph.module(unit.id).path.clone();
                    if self.diagnostic_source.is_none() {
                        self.diagnostic_source = Some((path.clone(), unit.source.clone()));
                    }
                    diagnostics.push(self.structured_diagnostic(&diagnostic, Some(path)));
                    failed.insert(unit.metadata.stable_id);
                    continue;
                }
            };

            if should_store_artifact {
                if let Err(error) = cache.store_artifact(&unit.cache_key, &artifact) {
                    let diagnostic = Diagnostic::codegen(error);
                    let path = graph.module(unit.id).path.clone();
                    diagnostics.push(self.structured_diagnostic(&diagnostic, Some(path)));
                    failed.insert(unit.metadata.stable_id);
                    continue;
                }
            }
            if let Err(error) = cache.store(&unit.cache_key, &artifact.metadata) {
                let diagnostic = Diagnostic::codegen(error);
                let path = graph.module(unit.id).path.clone();
                diagnostics.push(self.structured_diagnostic(&diagnostic, Some(path)));
                failed.insert(unit.metadata.stable_id);
                continue;
            }
            artifacts.push(artifact);
        }

        if diagnostics.is_empty() {
            if let Err(diagnostic) = self.compile_generic_instances(
                units,
                graph,
                metadata_map,
                cache,
                &mut artifacts,
                &mut snapshots,
            ) {
                let path = self
                    .diagnostic_source
                    .as_ref()
                    .map(|(path, _)| path.clone());
                diagnostics.push(self.structured_diagnostic(&diagnostic, path));
            }
        }
        (artifacts, diagnostics)
    }

    pub(crate) fn analyze_module_units(
        &self,
        units: &[ModuleSourceUnit],
    ) -> Result<(), Diagnostic> {
        for unit in units {
            unit.verify().map_err(Diagnostic::codegen)?;
            let (mut program, _) = syntax::parse_program_resources(
                &unit.source,
                "<source>",
                unit.resources
                    .iter()
                    .map(|(name, resource)| (name.clone(), resource.data.clone()))
                    .collect(),
            )?;
            select_target_externs(&mut program, std::env::consts::OS)?;
            let types = sema::check_module(&program)?;
            let core = CoreProgram::lower(program, types)?;
            let mut mir = MirProgram::lower(&core)?;
            MirPassManager::default_pipeline().run(&mut mir)?;
        }
        Ok(())
    }

    pub(crate) fn analyze_module_graph(
        &self,
        graph: &ModuleGraph,
        units: &[ModuleSourceUnit],
    ) -> Result<(), Diagnostic> {
        graph
            .verify_source_units(units)
            .map_err(Diagnostic::codegen)?;
        let metadata = graph.metadata_map();
        for id in graph.compilation_order().map_err(Diagnostic::codegen)? {
            graph
                .check_dependency_metadata(id, &metadata)
                .map_err(Diagnostic::codegen)?;
        }
        let artifacts =
            Self::new().compile_units(units, graph, &metadata, &ModuleCache::disabled())?;
        Self::link(artifacts).map(|_| ())
    }
}
pub(crate) fn select_target_externs(
    program: &mut syntax::Program,
    target_os: &str,
) -> Result<(), Diagnostic> {
    use std::collections::HashMap;
    let mut specific = HashMap::<String, (usize, usize)>::new();
    for f in &program.functions {
        if let Some(ext) = &f.foreign {
            for binding in std::iter::once(ext).chain(ext.alternatives.iter()) {
                if let Some(os) = &binding.target_os {
                    let entry = specific.entry(f.name.clone()).or_default();
                    entry.0 += 1;
                    if os == target_os {
                        entry.1 += 1;
                    }
                }
            }
        }
    }
    for f in &program.functions {
        if let Some((_, matches)) = specific.get(&f.name) {
            if f.foreign.as_ref().is_some_and(|e| e.target_os.is_some()) {
                if *matches == 0 {
                    return Err(Diagnostic::semantic(
                        format!(
                            "no extern binding matches target OS '{target_os}' for '{}'",
                            f.name
                        ),
                        f.span,
                    ));
                }
                if *matches > 1 {
                    return Err(Diagnostic::semantic(
                        format!(
                            "multiple extern bindings match target OS '{target_os}' for '{}'",
                            f.name
                        ),
                        f.span,
                    ));
                }
            }
        }
    }
    program.functions.retain_mut(|f| match &mut f.foreign {
        Some(ext) if specific.contains_key(&f.name) => {
            let selected = std::iter::once(ext.clone())
                .chain(ext.alternatives.clone())
                .find(|e| e.target_os.as_deref() == Some(target_os));
            if let Some(selected) = selected {
                *ext = selected;
                true
            } else {
                false
            }
        }
        Some(ext) => ext.target_os.is_none(),
        None => true,
    });
    Ok(())
}

fn collect_imports(mir: &MirProgram) -> Vec<SymbolId> {
    let mut imports = mir
        .functions
        .iter()
        .filter_map(|function| function.external_symbol.clone())
        .collect::<Vec<_>>();
    imports.sort();
    imports.dedup();
    imports
}

fn build_exports(
    mir: &MirProgram,
    interface: &sema::ModuleInterface,
    metadata: &ModuleMetadata,
) -> Result<HashMap<String, ExportedSymbol>, Diagnostic> {
    let mut exports = HashMap::new();
    for (name, visibility) in &metadata.exports {
        if *visibility != syntax::Visibility::Public {
            continue;
        }
        let signature = metadata
            .signatures
            .iter()
            .find(|(export, _)| export == name)
            .map(|(_, signature)| signature.clone())
            .unwrap_or_else(|| "<unknown>".into());
        if signature.starts_with("fn(") {
            if interface.type_program.as_ref().is_some_and(|program| {
                program.functions.iter().any(|f| {
                    f.name == *name && f.return_type.as_ref().is_some_and(|ty| ty.is_name("type"))
                })
            }) {
                exports.insert(name.clone(), ExportedSymbol::TypeFunction { signature });
                continue;
            }
            if interface.public_templates.contains_key(name) {
                exports.insert(name.clone(), ExportedSymbol::Template { signature });
                continue;
            }
            let mir_index = mir
                .functions()
                .iter()
                .position(|function| function.name == *name && function.external_symbol.is_none())
                .ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "export '{name}' has no MIR function or generic template"
                    ))
                })?;
            exports.insert(
                name.clone(),
                ExportedSymbol::Function {
                    mir_index,
                    signature,
                },
            );
        } else {
            exports.insert(
                name.clone(),
                ExportedSymbol::Constant {
                    type_annotation: signature,
                },
            );
        }
    }
    Ok(exports)
}
