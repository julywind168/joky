//! Compiler driver.
//!
//! `Compiler` chains Joky's frontend, HIR, the MIR verifier, the Cranelift JIT,
//! and the runtime ABI. Stages hand off only through explicit intermediate
//! representations, and diagnostics are converted to the unified
//! [`Diagnostic`](crate::Diagnostic) at stage boundaries.

use crate::codegen::CraneliftBackend;
use crate::mir::MirProgram;
use crate::module::{
    AotObjectArtifact, AotObjectCacheKey, ModuleCache, ModuleGraph, ModuleSourceUnit,
};
#[cfg(test)]
use crate::{
    frontend::select_target_externs,
    hir::CoreProgram,
    linker::Linker,
    mir::passes::MirPassManager,
    module::{ModuleArtifact, ModuleCompileContext, MODULE_MIR_FEATURES},
    syntax,
};
use crate::{sema, Diagnostic, Frontend};
#[cfg(test)]
use std::collections::HashMap;
use std::path::Path;

mod providers;

#[cfg(test)]
mod module_bench;

/// Native AOT options; source debug information is opt-in in either profile.
#[derive(Clone, Copy, Debug, Default)]
pub struct AotBuildOptions {
    pub target: crate::AotTarget,
    pub release: bool,
    pub debug_info: bool,
}

/// Source bytes actually consumed by an AOT compilation, identified by SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AotSourceRecord {
    pub path: std::path::PathBuf,
    pub sha256: String,
}

/// One `(effect, name)` operation a provider should serve. `operation` is the
/// encoded `(effect_id << 32) | operation_index` identity of the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOperationDescriptor {
    pub effect: String,
    pub name: String,
    pub operation: u64,
}

/// One native provider the program registers with the runtime: the provider
/// name plus the entries derived from its checked effect declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub name: String,
    pub operations: Vec<ProviderOperationDescriptor>,
}

/// Native provider information needed by an AOT launcher. Providers appear
/// only when the checked program declares their effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AotProviderMetadata {
    pub providers: Vec<ProviderDescriptor>,
    pub main_returns_result: bool,
    pub main_is_suspending: bool,
    /// Nonzero program-local keys and exported symbols, sorted by key. These
    /// describe one linked executable, not identities across program versions.
    pub machine_entries: Vec<(usize, String)>,
    pub sources: Vec<AotSourceRecord>,
}

impl AotProviderMetadata {
    fn from_types(types: &sema::TypeTable) -> Self {
        Self {
            providers: providers::provider_operations(types),
            main_returns_result: false,
            main_is_suspending: false,
            machine_entries: Vec::new(),
            sources: Vec::new(),
        }
    }

    fn from_mir(mir: &MirProgram) -> Self {
        let mut metadata = Self::from_types(mir.types());
        metadata.main_returns_result = mir.functions.iter().any(|function| {
            function.name == "main" && matches!(function.return_type, sema::Type::Result(_))
        });
        metadata.main_is_suspending = mir
            .functions
            .iter()
            .any(|function| function.name == "main" && function.is_suspending);
        metadata
    }
}

fn aot_source_records(graph: &ModuleGraph, units: &[ModuleSourceUnit]) -> Vec<AotSourceRecord> {
    use sha2::{Digest, Sha256};
    let mut sources = units
        .iter()
        .map(|unit| AotSourceRecord {
            path: graph.module(unit.id).path.clone(),
            sha256: hex::encode(Sha256::digest(unit.source.as_bytes())),
        })
        .collect::<Vec<_>>();
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    sources
}

fn aot_object_artifact(bytes: &[u8], metadata: &AotProviderMetadata) -> AotObjectArtifact {
    AotObjectArtifact {
        bytes: bytes.to_vec(),
        machine_entries: metadata.machine_entries.clone(),
        providers: metadata
            .providers
            .iter()
            .map(|provider| {
                (
                    provider.name.clone(),
                    provider
                        .operations
                        .iter()
                        .map(|operation| {
                            (
                                operation.effect.clone(),
                                operation.name.clone(),
                                operation.operation,
                            )
                        })
                        .collect(),
                )
            })
            .collect(),
        main_returns_result: metadata.main_returns_result,
        main_is_suspending: metadata.main_is_suspending,
    }
}

fn metadata_from_aot_object(
    artifact: AotObjectArtifact,
    sources: Vec<AotSourceRecord>,
) -> AotProviderMetadata {
    AotProviderMetadata {
        providers: artifact
            .providers
            .into_iter()
            .map(|(name, operations)| ProviderDescriptor {
                name,
                operations: operations
                    .into_iter()
                    .map(|(effect, name, operation)| ProviderOperationDescriptor {
                        effect,
                        name,
                        operation,
                    })
                    .collect(),
            })
            .collect(),
        main_returns_result: artifact.main_returns_result,
        main_is_suspending: artifact.main_is_suspending,
        machine_entries: artifact.machine_entries,
        sources,
    }
}

pub struct Compiler {
    backend: CraneliftBackend,
    warnings: Vec<String>,
    frontend: Frontend,
}

impl Compiler {
    pub fn new() -> Result<Self, Diagnostic> {
        Ok(Self {
            backend: CraneliftBackend::new().map_err(Diagnostic::from)?,
            warnings: Vec::new(),
            frontend: Frontend::new(),
        })
    }

    /// Drains accumulated compiler warnings. Continuation capability is a
    /// verifier/codegen concern and is no longer reported as a runtime
    /// fallback warning.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    pub fn take_module_events(&mut self) -> Vec<String> {
        self.frontend.take_module_events()
    }

    pub fn take_diagnostic_source(&mut self) -> Option<(std::path::PathBuf, String)> {
        self.frontend.take_diagnostic_source()
    }

    pub fn run_program(&mut self, source: &str) -> Result<(), Diagnostic> {
        self.run_program_with_args(source, Vec::new())
    }

    pub fn run_program_with_args(
        &mut self,
        source: &str,
        args: Vec<std::ffi::OsString>,
    ) -> Result<(), Diagnostic> {
        // Each run is independent: previous JIT code and string literals
        // must not accumulate when an embedding reuses a Compiler instance.
        self.backend = CraneliftBackend::new().map_err(Diagnostic::from)?;
        let mut mir = Frontend::lower_program(source)?;
        mir.retain_reachable_functions();
        let runtime_scope = joky_runtime::host::RuntimeScope::new_with_args(args);
        let _scope_guard = runtime_scope.enter();
        #[cfg(feature = "runtime-test-support")]
        joky_runtime::host::testing::report_resources(&runtime_scope, "startup");
        let _native_providers =
            providers::NativeProviderRegistry::install(mir.types(), &runtime_scope);
        let result = self
            .backend
            .compile_and_run_program(&mir, &runtime_scope)
            .map_err(Diagnostic::from);
        runtime_scope.close_and_wait();
        #[cfg(feature = "runtime-test-support")]
        {
            drop(_native_providers);
            joky_runtime::host::testing::report_resources(&runtime_scope, "drained");
        }
        #[cfg(test)]
        Self::assert_runtime_clean(&runtime_scope);
        result
    }

    /// Compile a non-running program into a native object file.
    ///
    /// The object path shares parsing, semantic analysis, MIR lowering and
    /// Cranelift function compilation with the JIT path. It intentionally
    /// stops before entry-point execution; the linker/launcher is layered on
    /// top of the returned object in the CLI.
    pub fn compile_object_program(&mut self, source: &str) -> Result<Vec<u8>, Diagnostic> {
        self.compile_object_program_with_metadata(source)
            .map(|(object, _)| object)
    }

    /// Compile an AOT object and return the provider operation IDs required by
    /// its launcher. The original [`compile_object_program`] API remains the
    /// compatibility entry point for callers that only need object bytes.
    pub fn compile_object_program_with_metadata(
        &mut self,
        source: &str,
    ) -> Result<(Vec<u8>, AotProviderMetadata), Diagnostic> {
        let mut mir = Frontend::lower_program(source)?;
        mir.retain_reachable_functions();
        let mut metadata = AotProviderMetadata::from_mir(&mir);
        let object =
            crate::codegen::emit_mir_object_with_entries(&mir, false).map_err(Diagnostic::from)?;
        metadata.machine_entries = object.machine_entries;
        Ok((object.bytes, metadata))
    }

    /// Compile a package module graph through the same MIR linker used by
    /// `run`, then emit a single AOT object and its provider metadata.
    pub fn compile_object_module_graph(
        &mut self,
        graph: &ModuleGraph,
    ) -> Result<(Vec<u8>, AotProviderMetadata), Diagnostic> {
        self.compile_object_module_graph_with_options(graph, false)
    }

    pub fn compile_object_module_graph_with_options(
        &mut self,
        graph: &ModuleGraph,
        release: bool,
    ) -> Result<(Vec<u8>, AotProviderMetadata), Diagnostic> {
        self.compile_object_module_graph_with_build_options(
            graph,
            AotBuildOptions {
                release,
                ..AotBuildOptions::default()
            },
            None,
        )
    }

    /// Compile a package to a native object. When `cache_dir` is set, module
    /// MIR is read and written with the same key as [`Self::run_modular_program`].
    /// A linked native object is cached separately by source graph and AOT profile.
    pub fn compile_object_module_graph_with_build_options(
        &mut self,
        graph: &ModuleGraph,
        options: AotBuildOptions,
        cache_dir: Option<&Path>,
    ) -> Result<(Vec<u8>, AotProviderMetadata), Diagnostic> {
        self.frontend.reset();
        let units = Frontend::source_units(graph, options.target, true)?;
        let cache = cache_dir
            .map(ModuleCache::new)
            .unwrap_or_else(ModuleCache::disabled);
        let object_key = AotObjectCacheKey::new(
            &units,
            env!("JOKY_BUILD_ID"),
            options.target.triple(),
            options.target.pointer_width(),
            options.release,
            options.debug_info,
        );
        let sources = aot_source_records(graph, &units);
        if let Some(cached) = cache
            .load_object(&object_key)
            .map_err(Diagnostic::codegen)?
        {
            self.frontend.module_cache_hits = 0;
            self.frontend.module_compilations = 0;
            self.frontend.module_events = vec!["hit aot object".into()];
            let bytes = cached.bytes.clone();
            return Ok((bytes, metadata_from_aot_object(cached, sources)));
        }
        let artifacts =
            self.frontend
                .compile_units(&units, graph, &graph.metadata_map(), &cache)?;
        let mut mir = Frontend::link(artifacts)?;
        mir.retain_reachable_functions();
        let mut metadata = AotProviderMetadata::from_mir(&mir);
        let debug = options.debug_info.then(|| {
            crate::codegen::debug::DebugInfo::new(
                units
                    .iter()
                    .map(|unit| crate::codegen::debug::DebugSource {
                        module: unit.metadata.stable_id,
                        path: graph.module(unit.id).path.clone(),
                        text: unit.source.clone(),
                    })
                    .collect(),
            )
        });
        let object = crate::codegen::emit_mir_object_with_debug(
            &mir,
            options.target,
            options.release,
            debug,
        )
        .map_err(Diagnostic::from)?;
        metadata.machine_entries = object.machine_entries;
        metadata.sources = sources;
        self.frontend.module_events.push(format!(
            "emit aot object: {}",
            cache.object_miss_reason(&object_key)
        ));
        cache
            .store_object(&object_key, &aot_object_artifact(&object.bytes, &metadata))
            .map_err(Diagnostic::codegen)?;
        Ok((object.bytes, metadata))
    }

    /// Compiles and links a multi-module program in dependency order, then runs
    /// the entry module
    pub fn run_modular_program(
        &mut self,
        graph: &ModuleGraph,
        cache_dir: &Path,
    ) -> Result<(), Diagnostic> {
        self.run_modular_program_with_cache(graph, Some(cache_dir))
    }

    /// Uses the same modular pipeline with or without persistent artifacts.
    pub fn run_modular_program_with_cache(
        &mut self,
        graph: &ModuleGraph,
        cache_dir: Option<&Path>,
    ) -> Result<(), Diagnostic> {
        self.run_modular_program_with_cache_and_args(graph, cache_dir, Vec::new())
    }

    pub fn run_modular_program_with_cache_and_args(
        &mut self,
        graph: &ModuleGraph,
        cache_dir: Option<&Path>,
        args: Vec<std::ffi::OsString>,
    ) -> Result<(), Diagnostic> {
        self.frontend.reset();
        let units = Frontend::source_units(graph, crate::AotTarget::native(), true)?;
        let metadata_map = graph.metadata_map();
        let cache = cache_dir
            .map(ModuleCache::new)
            .unwrap_or_else(ModuleCache::disabled);
        let artifacts = self
            .frontend
            .compile_units(&units, graph, &metadata_map, &cache)?;
        let mir = Frontend::link(artifacts)?;
        self.run_linked_program_with_args(mir, args)
    }

    fn run_linked_program_with_args(
        &mut self,
        mut mir: MirProgram,
        args: Vec<std::ffi::OsString>,
    ) -> Result<(), Diagnostic> {
        self.backend = CraneliftBackend::new().map_err(Diagnostic::from)?;
        mir.retain_reachable_functions();
        let file_types = mir.types().clone();
        let runtime_scope = joky_runtime::host::RuntimeScope::new_with_args(args);
        let _scope_guard = runtime_scope.enter();
        #[cfg(feature = "runtime-test-support")]
        joky_runtime::host::testing::report_resources(&runtime_scope, "startup");
        let _native_providers =
            providers::NativeProviderRegistry::install(&file_types, &runtime_scope);
        let result = self
            .backend
            .compile_and_run_program(&mir, &runtime_scope)
            .map_err(Diagnostic::from);
        runtime_scope.close_and_wait();
        #[cfg(feature = "runtime-test-support")]
        {
            drop(_native_providers);
            joky_runtime::host::testing::report_resources(&runtime_scope, "drained");
        }
        #[cfg(test)]
        Self::assert_runtime_clean(&runtime_scope);
        result
    }

    #[cfg(test)]
    fn assert_runtime_clean(scope: &joky_runtime::host::RuntimeScope) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let managed = joky_runtime::host::testing::managed_objects(scope);
            let resources = joky_runtime::host::testing::resource_counts(scope);
            if managed == 0 && resources == [0; 6] {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "JIT run did not release standalone resources: {resources:?}, managed={managed}, continuations={:?}\nlive objects:\n{}",
                joky_runtime::host::testing::describe_continuations(scope),
                joky_runtime::host::testing::describe_live_managed_objects().join("\n")
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Check independent source units without import context.
    pub fn analyze_module_units(&self, units: &[ModuleSourceUnit]) -> Result<(), Diagnostic> {
        self.frontend.analyze_module_units(units)
    }

    /// Check a module graph, including imports, generics and MIR validation.
    pub fn analyze_module_graph(
        &self,
        graph: &ModuleGraph,
        units: &[ModuleSourceUnit],
    ) -> Result<(), Diagnostic> {
        self.frontend.analyze_module_graph(graph, units)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod module_tests {
    use super::*;
    use crate::module::{
        source_fingerprint, ModuleCacheKey, ModuleMetadata, ModuleSourceUnit, StableId,
    };

    fn run_modules(name: &str, sources: &[(&str, &str)]) {
        try_run_modules(name, sources).unwrap();
    }

    fn try_run_modules(name: &str, sources: &[(&str, &str)]) -> Result<(), Diagnostic> {
        let root = std::env::temp_dir().join(format!("joky-abi-{name}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        for (file, source) in sources {
            std::fs::write(root.join("src").join(file), source).unwrap();
        }
        let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
        let mut compiler = Compiler::new().unwrap();
        let mut result = compiler.run_modular_program(&graph, &root.join("cache"));
        if result.is_ok() {
            let mut cached = Compiler::new().unwrap();
            result = cached.run_modular_program(&graph, &root.join("cache"));
            assert!(
                cached.frontend.module_cache_hits >= sources.len(),
                "second compilation must reuse every artifact"
            );
            assert_eq!(
                cached.frontend.module_compilations, 0,
                "cache reload must reuse module and generic instance artifacts"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
        result
    }

    fn write_shared_cache_package(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/util.jk"),
            "pub fn answer() -> Int32 { 42 }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.jk"),
            "import util\nfn main() { println(util.answer()) }\n",
        )
        .unwrap();
    }

    #[test]
    fn aot_and_jit_share_module_mir_cache() {
        let root =
            std::env::temp_dir().join(format!("joky-shared-mir-cache-{}", std::process::id()));
        write_shared_cache_package(&root);
        let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
        let jit_then_aot = root.join("cache-jit");
        let mut jit = Compiler::new().unwrap();
        jit.run_modular_program(&graph, &jit_then_aot).unwrap();
        assert!(
            jit.frontend.module_compilations >= 2,
            "cold JIT should compile the package modules"
        );
        let mut aot = Compiler::new().unwrap();
        aot.compile_object_module_graph_with_build_options(
            &graph,
            AotBuildOptions::default(),
            Some(&jit_then_aot),
        )
        .unwrap();
        assert_eq!(aot.frontend.module_compilations, 0);
        assert!(aot.frontend.module_cache_hits >= 2);

        let aot_then_jit = root.join("cache-aot");
        let mut aot = Compiler::new().unwrap();
        aot.compile_object_module_graph_with_build_options(
            &graph,
            AotBuildOptions::default(),
            Some(&aot_then_jit),
        )
        .unwrap();
        assert!(
            aot.frontend.module_compilations >= 2,
            "cold AOT should compile the package modules"
        );
        let mut jit = Compiler::new().unwrap();
        jit.run_modular_program(&graph, &aot_then_jit).unwrap();
        assert_eq!(jit.frontend.module_compilations, 0);
        assert!(jit.frontend.module_cache_hits >= 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn aot_object_cache_skips_emit_on_hit() {
        let root =
            std::env::temp_dir().join(format!("joky-aot-object-cache-{}", std::process::id()));
        write_shared_cache_package(&root);
        let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
        let cache = root.join("cache");
        let mut first = Compiler::new().unwrap();
        let (bytes, metadata) = first
            .compile_object_module_graph_with_build_options(
                &graph,
                AotBuildOptions::default(),
                Some(&cache),
            )
            .unwrap();
        assert!(first.frontend.module_compilations >= 2);
        assert!(first
            .take_module_events()
            .iter()
            .any(|event| event.starts_with("emit aot object")));

        let mut second = Compiler::new().unwrap();
        let (cached_bytes, cached_metadata) = second
            .compile_object_module_graph_with_build_options(
                &graph,
                AotBuildOptions::default(),
                Some(&cache),
            )
            .unwrap();
        assert_eq!(second.frontend.module_compilations, 0);
        assert_eq!(
            second.take_module_events(),
            vec!["hit aot object".to_owned()]
        );
        assert_eq!(cached_bytes, bytes);
        assert_eq!(cached_metadata, metadata);

        let mut release = Compiler::new().unwrap();
        release
            .compile_object_module_graph_with_build_options(
                &graph,
                AotBuildOptions {
                    release: true,
                    ..AotBuildOptions::default()
                },
                Some(&cache),
            )
            .unwrap();
        assert!(release
            .take_module_events()
            .iter()
            .any(|event| event.starts_with("emit aot object")));

        std::fs::write(
            root.join("src/util.jk"),
            "pub fn answer() -> Int32 { 43 }\n",
        )
        .unwrap();
        let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
        let mut edited = Compiler::new().unwrap();
        edited
            .compile_object_module_graph_with_build_options(
                &graph,
                AotBuildOptions::default(),
                Some(&cache),
            )
            .unwrap();
        assert!(edited
            .take_module_events()
            .iter()
            .any(|event| event.starts_with("emit aot object")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_program_drops_unreferenced_imported_functions() {
        let root =
            std::env::temp_dir().join(format!("joky-reachable-functions-{}", std::process::id()));
        write_shared_cache_package(&root);
        std::fs::write(
            root.join("src/util.jk"),
            "pub fn answer() -> Int32 { 42 }\npub fn unused() -> Int32 { 0 }\n",
        )
        .unwrap();
        let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
        let mut compiler = Compiler::new().unwrap();
        let artifacts = compiler
            .frontend
            .compile_with_cache(
                &graph
                    .source_units(
                        env!("JOKY_BUILD_ID"),
                        env!("JOKY_TARGET"),
                        usize::BITS as u8,
                        MODULE_MIR_FEATURES,
                    )
                    .unwrap(),
                &graph,
                &graph.metadata_map(),
                &root.join("cache"),
            )
            .unwrap();
        let mut linked = Linker::new(artifacts).link().unwrap();
        assert!(linked
            .mir
            .functions
            .iter()
            .any(|function| function.name == "unused"));
        linked.mir.retain_reachable_functions();
        assert!(linked
            .mir
            .functions
            .iter()
            .any(|function| function.name == "answer"));
        assert!(!linked
            .mir
            .functions
            .iter()
            .any(|function| function.name == "unused"));
        linked.mir.verify().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_indirect_calls_adopt_pending_closure_abi_from_other_modules() {
        let root =
            std::env::temp_dir().join(format!("joky-linked-pending-abi-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/util.jk"),
            "pub fn invoke(callback: fn() -> Unit) { callback() }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.jk"),
            "import util\n\
             class Gate { var started: Bool = false; fn mark() { self.started = true } }\n\
             fn main() {\n\
                 let gate = Cown.new(Gate())\n\
                 util.invoke(fn() -> Unit { when (gate) |state| { state.mark() } })\n\
             }\n",
        )
        .unwrap();
        let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
        let mut compiler = Compiler::new().unwrap();
        let artifacts = compiler
            .frontend
            .compile_with_cache(
                &graph
                    .source_units(
                        env!("JOKY_BUILD_ID"),
                        env!("JOKY_TARGET"),
                        usize::BITS as u8,
                        MODULE_MIR_FEATURES,
                    )
                    .unwrap(),
                &graph,
                &graph.metadata_map(),
                &root.join("cache"),
            )
            .unwrap();
        let linked = Linker::new(artifacts).link().unwrap();
        let invoke = linked
            .mir
            .functions
            .iter()
            .find(|function| function.name == "invoke")
            .unwrap();
        assert!(invoke.is_suspending);
        assert!(invoke
            .blocks
            .iter()
            .flat_map(|block| &block.statements)
            .any(|statement| matches!(
                statement,
                crate::mir::MirStatement::CallIndirect {
                    may_suspend: true,
                    continuation: Some(_),
                    ..
                }
            )));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn modular_intrinsic_effect_binding_preserves_defining_identity() {
        run_modules("intrinsic-effect-identity", &[
            ("api.jk", "@intrinsic(effect = io) class File { fn peek(&self) -> Int32 }\neff io { fn peek(handle: &File) -> Int32 }\npub fn read(handle: &File) -> Int32 effects { io } { handle.peek() }"),
            ("main.jk", "import api\neff io { fn peek(handle: &File) -> String }\nfn inspect(handle: &File) -> Int32 effects { api.io } { handle.peek() }\nfn main() {}"),
        ]);
    }

    #[test]
    fn modular_intrinsic_file_methods_run_through_cached_helpers() {
        let root =
            std::env::temp_dir().join(format!("joky-intrinsic-file-input-{}", std::process::id()));
        std::fs::write(&root, b"abcd").unwrap();
        let main = format!("import joky/file\nimport helper\nfn main() effects {{ file }} {{ let expected: UInt64 = 1; let handle = file.open(\"{}\", FileMode.Read)!; if helper.read_one(handle)!.length() != expected {{ panic(\"first read\") }} else {{}}; if helper.read_one(handle)!.length() != expected {{ panic(\"second read\") }} else {{}}; handle.close()! }}", root.display());
        run_modules("intrinsic-file-helpers", &[
            ("helper.jk", "import joky/file\npub fn read_one(handle: &File) -> Result(Bytes, String) effects { file } { handle.read_chunk(1) }"),
            ("main.jk", &main),
        ]);
        std::fs::remove_file(root).unwrap();
        let error = try_run_modules("intrinsic-file-missing-effect", &[
            ("helper.jk", "import joky/file\npub fn read_one(handle: &File) -> Result(Bytes, String) { handle.read_chunk(1) }"),
            ("main.jk", "import helper\nfn main() {}"),
        ]).unwrap_err();
        assert!(error.to_string().contains("not declared"), "{error}");
    }

    #[test]
    fn intrinsic_effect_binding_changes_invalidate_module_cache() {
        let root =
            std::env::temp_dir().join(format!("joky-intrinsic-cache-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.jk"), "import api\nfn main() {}").unwrap();
        let source = "@intrinsic(effect = first) class File { fn peek(&self) -> Int32 }\neff first { fn peek(handle: &File) -> Int32 }\neff second { @suspends fn peek(handle: &File) -> Int32 }";
        std::fs::write(root.join("src/api.jk"), source).unwrap();
        let run = || {
            let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
            let mut compiler = Compiler::new().unwrap();
            compiler
                .run_modular_program(&graph, &root.join("cache"))
                .unwrap();
            (
                compiler.frontend.module_cache_hits,
                compiler.frontend.module_compilations,
            )
        };
        assert_eq!(run(), (0, 2));
        assert_eq!(run(), (2, 0));
        std::fs::write(
            root.join("src/api.jk"),
            source.replace("effect = first", "effect = second"),
        )
        .unwrap();
        assert_eq!(run(), (0, 2));
        assert_eq!(run(), (2, 0));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imported_generic_constraints_preserve_associated_types() {
        run_modules("generic-associated-types", &[
            ("util.jk", "trait Source { type Item; fn next(&self) -> Option(Item) }\nstruct Items { let value: Int64 }\nimpl Source for Items { type Item = Int64; fn next() -> Option(Self.Item) { Some(self.value) } }\nfn Maybe(T: type) -> type { Option(T) }\npub fn make() -> Items { Items(value: 42) }\npub fn next(I: type + Source, iter: I) -> Maybe(I.Item) { iter.next() }"),
            ("main.jk", "import util\nfn main() { let value = util.next(util.make())!; let expected: Int64 = 42; if value != expected { panic(\"bad associated type\") } }")
        ]);
    }

    #[test]
    fn computed_struct_defaults_remain_local_with_a_deferred_import_error() {
        let declarations = "fn initial() -> Int32 { 42 }\n\
            struct Item { let value: Int32 = initial() }\n\
            pub fn make() -> Item { Item() }";
        let local = format!(
            "{declarations}\nfn main() {{ if make().value != 42 {{ panic(\"default\") }} else {{}} }}"
        );
        run_modules("local-computed-default", &[("main.jk", &local)]);
        // Merely exporting a type must not reject a legal local default.
        run_modules(
            "unused-computed-default",
            &[
                ("util.jk", declarations),
                ("main.jk", "import util\nfn main() {}"),
            ],
        );
        let error = try_run_modules(
            "imported-computed-default",
            &[
                ("util.jk", declarations),
                (
                    "main.jk",
                    "import util\nfn main() { let item = util.make() }",
                ),
            ],
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported exported constant expression"));
        assert!(error.span().is_some());
    }

    #[test]
    fn imported_type_functions_preserve_nested_constant_defaults() {
        run_modules("type-function-defaults", &[
            ("util.jk", "fn Box(T: type) -> type { struct { let value: T = 7 } }\npub fn Pair(T: type) -> type { struct { let left: Box(T) = Box(T)(value: 11); let right: Box(T) = Box(T)() } }"),
            ("main.jk", "import util\nfn main() { let pair = util.Pair(Int64)(); let left = pair.left; let right = pair.right; let a: Int64 = 11; let b: Int64 = 7; if left.value != a { panic(\"bad left default\") }; if right.value != b { panic(\"bad right default\") } }")
        ]);
    }

    #[test]
    fn modular_generic_internal_waits_and_entry_selection_are_explicit() {
        run_modules("generic-internal-wait", &[
            ("util.jk", "fn main() { panic(\"library main must not run\") }\npub fn copy(T: type, value: T) -> T { let done = parallel {\n| 1\n| 2\n}; value }"),
            ("main.jk", "import util\nfn main() { if util.copy(\"ready\") != \"ready\" { panic(\"bad internal wait\") } }")
        ]);
        assert!(
            try_run_modules("missing-main", &[("main.jk", "fn helper() {}")])
                .unwrap_err()
                .to_string()
                .contains("main")
        );
    }

    #[test]
    fn generated_type_imports_distinguish_concrete_types_and_tuple_shapes() {
        run_modules("generic-identity-shapes", &[
            ("util.jk", "pub fn Box(T: type) -> type { struct { let value: T } }\npub fn copy(T: type, value: T) -> T { value }"),
            ("main.jk", "import util\nfn main() { let a: util.Box(Int32) = util.Box(Int32)(value: 42); let b: util.Box(String) = util.Box(String)(value: \"ok\"); if a.value != 42 { panic(\"bad int\") }; if b.value != \"ok\" { panic(\"bad string\") }; let c = util.copy(((1,2),3)); let d = util.copy(((4,5,6),)); let x = c.0; let y = d.0; if x.0 != 1 { panic(\"bad shape\") }; if y.2 != 6 { panic(\"bad shape\") } }")
        ]);
    }

    #[test]
    fn modular_generics_link_static_trait_methods_and_caller_show_impls() {
        run_modules("generic-static-traits", &[
            ("util.jk", "trait Read { fn read(&self) -> Int32 }\nclass Box { let value: Int32 }\nimpl Read for Box { fn read(&self) -> Int32 { self.value } }\npub fn make() -> Box { Box(value: 42) }\npub fn read(T: type + Read, value: T) -> Int32 { value.read() }\npub fn display(T: type + Show, value: T) { println(value) }"),
            ("main.jk", "import util\nstruct Point { let value: Int32 }\nimpl Show for Point { fn show() -> String { \"point\" } }\nfn main() { let b = util.make(); if util.read(b) != 42 { panic(\"bad trait method\") }; util.display(Point(value: 7)); let other = util.make(); if other.read() != 42 { panic(\"bad imported method\") } }")
        ]);
    }

    #[test]
    fn modular_qualified_trait_calls_keep_implementation_identity() {
        run_modules(
            "qualified-trait-methods",
            &[
                (
                    "util.jk",
                    r#"
                trait First { fn read(&self) -> Int32 }
                trait Second { fn read(&self) -> Int32 }
                class Value { let value: Int32 }
                impl First for Value { fn read(&self) -> Int32 { self.value } }
                impl Second for Value { fn read(&self) -> Int32 { self.value + 1 } }
                pub fn make() -> Value { Value(value: 41) }
                pub fn first(T: type + First + Second, value: &T) -> Int32 { First.read(value) }
                pub fn second(T: type + First + Second, value: &T) -> Int32 { Second.read(value) }
                pub fn dynamic() -> Dyn(Second) { Dyn(Second)(Value(value: 41)) }
            "#,
                ),
                (
                    "main.jk",
                    r#"
                import util
                trait First { fn read(&self) -> Int32 }
                class Local { let value: Int32 }
                impl First for Local { fn read(&self) -> Int32 { 1 } }
                impl util.First for Local { fn read(&self) -> Int32 { 2 } }
                impl util.Second for Local { fn read(&self) -> Int32 { 3 } }
                fn main() {
                    let value = util.make()
                    if util.First.read(value) != 41 { panic("first") }
                    if util.Second.read(value) != 42 { panic("second") }
                    if util.first(value) != 41 { panic("generic first") }
                    if util.second(value) != 42 { panic("generic second") }
                    let local = Local(value: 0)
                    if First.read(local) != 1 { panic("local trait") }
                    if util.First.read(local) != 2 { panic("imported trait") }
                    if util.second(local) != 3 { panic("caller impl") }
                    let dynamic = util.dynamic()
                    if util.Second.read(dynamic) != 42 { panic("dynamic") }
                }
            "#,
                ),
            ],
        );
    }

    #[test]
    fn constant_abi_ignores_local_tuple_allocation_order() {
        let source =
            "pub const A = (1,2)\npub const B = (true,\"b\")\npub const C = ((1,2,3), false)";
        let metadata = ModuleMetadata {
            format_version: 5,
            stable_id: StableId(1),
            abi_hash: 7,
            exports: vec![],
            dependencies: vec![],
            signatures: vec![],
            runtime_initializers: vec![],
            drop_glue: vec![],
        };
        let expected = compile_test_artifact(source, metadata.clone())
            .metadata
            .abi_hash;
        for _ in 0..12 {
            assert_eq!(
                compile_test_artifact(source, metadata.clone())
                    .metadata
                    .abi_hash,
                expected
            );
        }
    }

    #[test]
    fn modular_type_functions_keep_private_helpers_and_nominal_identity() {
        run_modules("imported-type-functions", &[
            ("util.jk", "fn Inner(T: type) -> type { List(T) }\npub fn Values(T: type) -> type { Inner(T) }\npub fn Box(T: type) -> type { struct { let value: T } }\npub fn Number() -> type { Int64 }\npub fn first(T: type, values: Values(T)) -> Option(T) { values.head() }"),
            ("main.jk", "import util\nfn Inner(T: type) -> type { Option(T) }\nfn main() { let n: util.Number() = 42; let xs: util.Values(String) = List(\"item\"); if util.first(xs).unwrap_or(\"bad\") != \"item\" { panic(\"bad imported type function\") }; let box: util.Box(Int64) = util.Box(Int64)(value: n); let expected: Int64 = 42; if box.value != expected { panic(\"bad generated nominal type\") } }")
        ]);
    }

    #[test]
    fn standard_modules_preserve_effects_resources_and_generic_containers() {
        run_modules(
            "standard-library",
            &[(
                "main.jk",
                r#"import joky/list
import joky/map
import joky/set
import joky/option
import joky/result
import joky/string
import joky/bytes
import joky/file
import joky/time
fn main() effects { file, time } {
    time.sleep(1ms)
    let mode: FileMode = FileMode.Read
    let origin: SeekFrom = SeekFrom.Start
    if string.trim(" ok ") != "ok" { panic("bad string") }
    let xs = List("item")
    if list.head(xs).unwrap_or("bad") != "item" { panic("bad list") }
    if !option.is_some(Some(1)) { panic("bad option") }
    if !result.is_ok(Ok(1)) { panic("bad result") }
    let empty: Set(String) = set.empty(String)
    let added = set.insert(empty, "key")
    if !set.contains(added, "key") { panic("bad set") }
    let m = map.insert(Map(String, Int32)(), "key", 42)
    if map.get(m, "key").unwrap_or(0) != 42 { panic("bad map") }
}"#,
            )],
        );
    }

    #[test]
    fn source_sum_modules_share_canonical_types_and_cached_generic_methods() {
        run_modules(
            "source-sum-modules",
            &[
                (
                    "util.jk",
                    r#"
                import joky/option
                import joky/result
                pub fn optional(value: T) -> option.Option(T) { Some(value) }
                pub fn success(value: T) -> result.Result(T, String) { Ok(value) }
                pub fn get(value: Option(T), fallback: T) -> T { value.unwrap_or(fallback) }
            "#,
                ),
                (
                    "main.jk",
                    r#"
                import util
                import joky/option
                import joky/result
                class Box { let value: Int32 }
                fn main() {
                    let value: Option(Box) = util.optional(Box(42))
                    if !option.is_some(value) || option.is_none(value) { panic("option borrow") }
                    if !value.is_some() { panic("method borrow") }
                    let box = util.get(value, Box(0))
                    if box.value != 42 { panic("generic payload") }
                    let value: result.Result(Box, String) = util.success(Box(7))
                    if !result.is_ok(value) || result.is_err(value) { panic("result borrow") }
                    let box = result.unwrap_or(value, Box(0))
                    if box.value != 7 { panic("result payload") }
                    let none: option.Option(Int32) = None
                    if option.unwrap_or_else(none, fn () -> Int32 { 9 }) != 9 { panic("callback") }
                }
            "#,
                ),
            ],
        );
    }

    #[test]
    fn generic_instances_share_stable_identity_and_invalidate_private_helpers() {
        let root = std::env::temp_dir().join(format!("joky-generic-cache-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        for (file, source) in [
            (
                "a.jk",
                "import leaf\npub fn run() -> Int32 { leaf.answer(1) }",
            ),
            (
                "b.jk",
                "import leaf\npub fn run() -> Int32 { let other = (1,2,3); leaf.answer(2) }",
            ),
            (
                "main.jk",
                "import a\nimport b\nfn main() { println(a.run()); println(b.run()) }",
            ),
        ] {
            std::fs::write(root.join("src").join(file), source).unwrap();
        }
        let run = || {
            let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
            let mut compiler = Compiler::new().unwrap();
            compiler
                .run_modular_program(&graph, &root.join("cache"))
                .unwrap();
            (
                compiler.frontend.module_cache_hits,
                compiler.frontend.module_compilations,
            )
        };
        std::fs::write(
            root.join("src/leaf.jk"),
            "fn helper() -> Int32 { 1 }\npub fn answer(T: type, value: T) -> Int32 { helper() }",
        )
        .unwrap();
        assert_eq!(run(), (0, 5), "four source modules share one instance");
        assert_eq!(run(), (5, 0));
        std::fs::write(
            root.join("src/leaf.jk"),
            "fn helper() -> Int32 { 2 }\npub fn answer(T: type, value: T) -> Int32 { helper() }",
        )
        .unwrap();
        assert_eq!(
            run(),
            (0, 5),
            "private template dependencies invalidate callers and the instance"
        );
        assert_eq!(run(), (5, 0));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_dependencies_support_changed_callers_and_nested_type_queries() {
        let root =
            std::env::temp_dir().join(format!("joky-shared-import-context-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/leaf.jk"),
            "pub fn Box(T: type) -> type { struct { let value: T } }\n\
             pub fn copy(T: type, value: T) -> T { value }",
        )
        .unwrap();
        std::fs::write(
            root.join("src/middle.jk"),
            "import leaf\n\
             pub fn Box(T: type) -> type { leaf.Box(T) }\n\
             pub fn copy(T: type, value: T) -> T { leaf.copy(value) }",
        )
        .unwrap();
        // Reuse the driver across revisions; dependency snapshots must belong
        // to a single compilation and query-local type indices must not leak.
        let mut compiler = Compiler::new().unwrap();
        for (revision, (ty, value)) in [("Int32", "42"), ("String", "\"new\"")]
            .into_iter()
            .enumerate()
        {
            std::fs::write(
                root.join("src/main.jk"),
                format!(
                    r#"
                    import middle
                    struct Local {{ let value: {ty} }}
                    fn main() {{
                        let first = middle.Box(Int32)(value: 42)
                        let second = middle.Box(String)(value: "ok")
                        if first.value != 42 {{ panic("first query") }} else {{}}
                        if second.value != "ok" {{ panic("second query") }} else {{}}
                        let local = middle.copy(Local(value: {value}))
                        if local.value != {value} {{ panic("caller type") }} else {{}}
                    }}
                    "#
                ),
            )
            .unwrap();
            let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
            for warm in [false, true] {
                compiler
                    .run_modular_program(&graph, &root.join("cache"))
                    .unwrap();
                let expected = if warm {
                    (5, 0)
                } else if revision == 0 {
                    (0, 5)
                } else {
                    (2, 3)
                };
                assert_eq!(
                    (
                        compiler.frontend.module_cache_hits,
                        compiler.frontend.module_compilations
                    ),
                    expected,
                    "revision {revision}, warm={warm}"
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn modular_generics_preserve_structural_types_labels_and_constraints() {
        run_modules("generic-types", &[
            ("util.jk", "pub fn copy(T: type, value: T) -> T { value }\npub fn show(T: type + Show, value: T) { println(value) }\npub fn get(value: List(T)) -> Option(T) { value.head() }"),
            ("main.jk", "import util\nclass Box { let text: String }\nfn main() { let local = (1,2,3); let pair = util.copy(value: (\"ok\",(42,true))); if pair.0 != \"ok\" { panic(\"bad tuple\") }; let b = util.copy(Box(text: \"owned\")); if b.text != \"owned\" { panic(\"bad class\") }; util.show(42); let xs = List(\"item\"); let result: Option(String) = util.get(xs); if result.unwrap_or(\"bad\") != \"item\" { panic(\"bad list\") }; let explicit = util.copy(String, \"explicit\"); if explicit != \"explicit\" { panic(\"bad type argument\") } }")
        ]);
        assert!(try_run_modules(
            "generic-bound",
            &[
                (
                    "util.jk",
                    "pub fn show(T: type + Show, value: T) { println(value) }"
                ),
                (
                    "main.jk",
                    "import util\nclass Box { let x: Int32 }\nfn main() { util.show(Box(x: 1)) }"
                )
            ]
        )
        .is_err());
    }

    #[test]
    fn modular_generic_pending_results_survive_cache_reload() {
        run_modules("generic-pending", &[
            ("util.jk", "eff time { @suspends fn sleep(duration: Duration) -> Unit }\npub fn delay(T: type, value: T) -> T effects { time } { time.sleep(1ms); value }"),
            ("main.jk", "import util\nfn main() effects { util.time } { if util.delay(\"ready\") != \"ready\" { panic(\"bad generic pending result\") } }")
        ]);
    }

    #[test]
    fn modular_generics_use_defining_scope_and_nested_instances() {
        run_modules("generic-scope", &[
            ("leaf.jk", "const OFFSET = 2\nfn helper() -> Int32 { OFFSET }\nfn identity(T: type, value: T) -> T { value }\npub fn copy(T: type, value: T) -> T { identity(value) }\npub fn answer(T: type, value: T) -> Int32 { helper() }"),
            ("middle.jk", "import leaf\npub fn copy(T: type, value: T) -> T { leaf.copy(value) }"),
            ("main.jk", "import leaf\nimport middle\nfn helper() -> Int32 { 99 }\nfn main() { if middle.copy(42) != 42 { panic(\"bad int\") }; if leaf.copy(\"ok\") != \"ok\" { panic(\"bad string\") }; if leaf.answer(false) != 2 { panic(\"wrong defining scope\") } }")
        ]);
    }

    #[test]
    fn modular_generic_callbacks_reuse_structural_function_types() {
        let util = "pub fn apply(T: type, tag: T, callback: fn(value: Int32) -> Int32) -> Int32 { callback(value: 42) }";
        run_modules(
            "generic-callback-types",
            &[
                ("util.jk", util),
                (
                    "main.jk",
                    r#"
                    import util
                    fn main() {
                        let callback = fn (value: Int32) -> Int32 { value + 1 }
                        if util.apply("tag", callback) != 43 { panic("string instance") }
                        let other = fn (value: Int32) -> Int32 { value + 1 }
                        if util.apply(1, other) != 43 { panic("integer instance") }
                    }
                "#,
                ),
            ],
        );
        assert!(try_run_modules(
            "generic-callback-mismatch",
            &[
                ("util.jk", util),
                ("main.jk", "import util\nfn main() { let callback = fn (value: Int32) -> Bool { true }; util.apply(1, callback) }"),
            ],
        ).is_err());
    }

    #[test]
    fn modular_constants_preserve_types_and_transitive_values() {
        run_modules("constants", &[
            ("leaf.jk", "const OFFSET: Int64 = 2\npub const ANSWER: Int64 = 40 + OFFSET\npub const TEXT = \"ok\"\npub const PAIR = (true, (1.5, \"nested\"))\npub const DELAY = 1ms"),
            ("middle.jk", "import leaf\npub const ANSWER = leaf.ANSWER\npub const PAIR = leaf.PAIR\npub const TEXT = leaf.TEXT"),
            ("main.jk", "import middle\nfn main() { let collision = (1,2,3); let answer: Int64 = middle.ANSWER; let expected: Int64 = 42; if answer != expected { panic(\"bad answer\") }; if middle.TEXT != \"ok\" { panic(\"bad text\") }; let p = middle.PAIR; if !p.0 { panic(\"bad bool\") }; let nested = p.1; if nested.0 != 1.5 { panic(\"bad float\") }; if nested.1 != \"nested\" { panic(\"bad tuple\") } }")
        ]);
    }

    #[test]
    fn modular_private_constants_are_not_importable() {
        let result = try_run_modules(
            "private-constant",
            &[
                ("util.jk", "const SECRET = 42"),
                ("main.jk", "import util\nfn main() { println(util.SECRET) }"),
            ],
        );
        assert!(result.unwrap_err().to_string().contains("SECRET"));
        assert!(try_run_modules(
            "constant-mismatch",
            &[
                ("util.jk", "pub const TEXT = \"not an integer\""),
                (
                    "main.jk",
                    "import util\nfn main() { let wrong: Int32 = util.TEXT }"
                )
            ]
        )
        .unwrap_err()
        .to_string()
        .contains("expected Int32"));
    }

    #[test]
    fn constant_value_changes_invalidate_transitive_callers() {
        let root = std::env::temp_dir().join(format!("joky-constant-cache-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/middle.jk"),
            "import leaf\npub const VALUE = leaf.VALUE",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.jk"),
            "import middle\nfn main() { println(middle.VALUE) }",
        )
        .unwrap();
        let run = || {
            let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
            let mut compiler = Compiler::new().unwrap();
            compiler
                .run_modular_program(&graph, &root.join("cache"))
                .unwrap();
            compiler.frontend.module_cache_hits
        };
        std::fs::write(root.join("src/leaf.jk"), "pub const VALUE = 1").unwrap();
        assert_eq!(run(), 0);
        assert_eq!(run(), 3);
        std::fs::write(root.join("src/leaf.jk"), "pub const VALUE = 2").unwrap();
        assert_eq!(run(), 0);
        assert_eq!(run(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linker_relocates_nested_tuple_signatures_and_runtime_intrinsics() {
        run_modules("tuple", &[
            ("util.jk", "pub fn pair(value: (Int32,(String,Bool))) -> (String,Int32) { let unused = (false, 2); let nested = value.1; (nested.0,value.0) }"),
            ("main.jk", "import util\nfn main() { let local = (1,2,3); let result = util.pair((42,(\"ok\",true))); if result.0 != \"ok\" { panic(\"bad tuple string\") }; if result.1 != 42 { panic(\"bad tuple scalar\") }; let items = List(result); println(items.length()) }")
        ]);
    }

    #[test]
    fn linker_keeps_same_named_classes_in_distinct_modules() {
        run_modules("classes", &[
            ("util.jk", "class Box { let value: String; fn read() -> String { self.value } }\npub fn answer() -> String { let b = Box(value: \"ok\"); b.read() }"),
            ("main.jk", "import util\nclass Box { let value: Int32 }\nfn main() { let b = Box(value: 42); if util.answer() != \"ok\" { panic(\"bad class\") } }")
        ]);
    }

    #[test]
    fn modular_class_handles_support_borrow_transfer_and_nested_drop() {
        run_modules("class-handles", &[
            ("util.jk", "class Inner { let text: String }\nclass Box { let inner: Inner; fn read() -> String { self.inner.text } }\npub fn make() -> Box { Box(inner: Inner(text: \"owned\")) }\npub fn read(value: &Box) -> String { value.read() }\npub fn consume(value: Box) -> String { value.read() }"),
            ("main.jk", "import util\nclass Box { let value: Int32 }\nfn main() { let value: util.Box = util.make(); println(util.read(value)); println(util.read(value)); if util.consume(value) != \"owned\" { panic(\"bad class handle\") }; let dropped = util.make() }")
        ]);
    }

    #[test]
    fn modular_ownership_rejects_use_after_transfer_and_overlapping_borrows() {
        for (index, body) in [
            "util.consume(value); util.read(value)",
            "util.pair(value,value)",
        ]
        .iter()
        .enumerate()
        {
            let main = format!("import util\nfn main() {{ let value = util.make(); {body} }}");
            assert!(try_run_modules(&format!("invalid-ownership-{index}"), &[
                ("util.jk", "class Box {}\npub fn make() -> Box { Box() }\npub fn read(value: &Box) {}\npub fn consume(value: Box) {}\npub fn pair(a: &Box,b: &Box) {}"),
                ("main.jk", &main),
            ]).is_err());
        }
    }

    #[test]
    fn modular_pending_returns_managed_values_and_reuses_cached_mir() {
        run_modules("pending", &[
            ("util.jk", "eff time { @suspends fn sleep(duration: Duration) -> Unit }\npub fn wait() -> String effects { time } { time.sleep(1ms); \"resumed\" }"),
            ("main.jk", "import util\nfn main() effects { util.time } { let value = util.wait(); if value != \"resumed\" { panic(\"bad pending result\") } }")
        ]);
    }

    #[test]
    fn modular_normal_effect_dispatch_uses_defining_module_identity() {
        run_modules("effect", &[
            ("util.jk", "eff Value { fn get() -> Int32 }\npub fn get() -> Int32 effects { Value } { Value.get() }"),
            ("main.jk", "import util\neff Value { fn other() -> String }\nfn main() { let value = do { util.get() } with { util.Value.get() => 42 }; if value != 42 { panic(\"bad effect identity\") } }")
        ]);
    }

    #[test]
    fn imported_effect_operations_can_be_called_with_qualified_names() {
        run_modules(
            "qualified-effect",
            &[
                (
                    "util.jk",
                    "eff time { @suspends fn sleep(duration: Duration) -> Unit }",
                ),
                (
                    "main.jk",
                    "import util\nfn main() effects { util.time } { util.time.sleep(1ms) }",
                ),
            ],
        );
    }

    #[test]
    fn artifact_cache_invalidates_source_and_transitive_abi_and_recovers_corruption() {
        let root = std::env::temp_dir().join(format!("joky-abi-cache-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.jk"),
            "import middle\nfn main() { println(middle.value()) }",
        )
        .unwrap();
        std::fs::write(
            root.join("src/middle.jk"),
            "import leaf\npub fn value() -> Int32 { leaf.value() }",
        )
        .unwrap();
        std::fs::write(root.join("src/leaf.jk"), "pub fn value() -> Int32 { 1 }").unwrap();
        let run = || {
            let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
            let mut compiler = Compiler::new().unwrap();
            compiler
                .run_modular_program(&graph, &root.join("cache"))
                .unwrap();
            compiler.frontend.module_cache_hits
        };
        assert_eq!(run(), 0);
        assert_eq!(run(), 3);
        std::fs::write(root.join("src/leaf.jk"), "pub fn value() -> Int32 { 2 }").unwrap();
        assert_eq!(run(), 2, "body-only dependency edit reuses callers");
        std::fs::write(
            root.join("src/leaf.jk"),
            "pub fn value() -> Int32 { 2 }\npub fn added() {}",
        )
        .unwrap();
        assert_eq!(run(), 0, "public ABI change invalidates transitive callers");
        assert_eq!(run(), 3);
        for entry in std::fs::read_dir(root.join("cache")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "jmir") {
                std::fs::write(path, b"truncated").unwrap();
            }
        }
        assert_eq!(run(), 0);
        assert_eq!(run(), 3);
        // A previous format can have a valid payload checksum. It must still
        // rebuild rather than deserialize old source-node tables as layouts.
        for entry in std::fs::read_dir(root.join("cache")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "jmir") {
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[..8].copy_from_slice(b"JKMIR018");
                std::fs::write(path, bytes).unwrap();
            }
        }
        assert_eq!(
            run(),
            0,
            "old MIR formats rebuild even with intact payloads"
        );
        assert_eq!(run(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identical_module_sources_have_distinct_cache_entries() {
        run_modules(
            "identical",
            &[
                ("a.jk", "pub fn value() -> Int32 { 7 }"),
                ("b.jk", "pub fn value() -> Int32 { 7 }"),
                (
                    "main.jk",
                    "import a\nimport b\nfn main() { println(a.value() + b.value()) }",
                ),
            ],
        );
    }

    #[test]
    fn modular_forwarded_types_keep_the_original_owner_and_nested_layout() {
        run_modules("forwarded", &[
            ("leaf.jk","struct Payload { let text: String }\nclass Box { let payload: Payload; fn read() -> String { self.payload.text } }\npub fn make() -> Box { Box(payload: Payload(text: \"forwarded\")) }\npub fn read(value: &Box) -> String { value.read() }"),
            ("middle.jk","import leaf\npub fn make() -> leaf.Box { leaf.make() }\npub fn read(value: &leaf.Box) -> String { leaf.read(value) }"),
            ("main.jk","import middle\nfn main() { let b = middle.make(); if middle.read(b) != \"forwarded\" { panic(\"bad forwarded layout\") } }"),
        ]);
    }

    #[test]
    fn modular_pending_cancellation_releases_owned_frames() {
        run_modules("pending-cancel", &[
            ("util.jk","eff time { @suspends fn sleep(duration: Duration) -> Unit }\nclass Box { let text: String }\npub fn wait() -> Box effects { time } { let b = Box(text: \"cancel\"); time.sleep(30ms); b }\npub fn fast() -> Box { Box(text: \"ready\") }"),
            ("main.jk","import util\nfn main() effects { util.time } { let winner = race {\n| util.wait()\n| util.fast()\n} }"),
        ]);
    }

    fn compile_test_artifact(source: &str, metadata: ModuleMetadata) -> ModuleArtifact {
        let unit = ModuleSourceUnit {
            id: crate::module::ModuleId(0),
            cache_key: ModuleCacheKey::new(source.as_bytes(), &[], "r", "t", 64, ""),
            metadata,
            source: source.to_string(),
        };
        Compiler::new()
            .unwrap()
            .frontend
            .compile_module(&unit, &ModuleCompileContext::default())
            .unwrap()
    }

    #[test]
    fn imported_abi_preserves_borrowed_parameters_and_nested_types() {
        let mut context = ModuleCompileContext::default();
        context.imports.insert("util".into(), StableId(1));
        context.dependency_exports.insert(
            StableId(1),
            HashMap::from([
                ("inspect".into(), "fn(value:&MutBytes)->Int32!{}".into()),
                (
                    "pair".into(),
                    "fn(value:(Int32,(String,Bool)))->(Int32,String)!{}".into(),
                ),
            ]),
        );
        let source = "import util\nfn main() { let b = MutBytes.with_capacity(8); let _ = util.inspect(b); let _ = util.inspect(b); let p = util.pair((1,(\"x\",true))); println(p.0) }";
        let unit = ModuleSourceUnit {
            id: crate::module::ModuleId(0),
            source: source.into(),
            cache_key: ModuleCacheKey::new(source.as_bytes(), &[], "r", "t", 64, ""),
            metadata: ModuleMetadata {
                format_version: 3,
                stable_id: StableId(2),
                abi_hash: 1,
                exports: vec![],
                dependencies: vec![],
                signatures: vec![],
                runtime_initializers: vec![],
                drop_glue: vec![],
            },
        };
        let artifact = Compiler::new()
            .unwrap()
            .frontend
            .compile_module(&unit, &context)
            .unwrap();
        let stub = artifact
            .mir
            .functions()
            .iter()
            .find(|f| {
                f.external_symbol
                    .as_ref()
                    .is_some_and(|s| s.name == "inspect")
            })
            .unwrap();
        assert_eq!(
            stub.parameters[0].ownership,
            crate::mir::MirOwnership::Borrowed
        );
    }

    #[test]
    fn linker_passes_when_all_dependencies_are_present() {
        let artifact_a = compile_test_artifact(
            "fn helper() -> Int32 { 1 }\n",
            ModuleMetadata {
                format_version: 1,
                stable_id: StableId(1),
                abi_hash: 100,
                exports: vec![("helper".into(), crate::syntax::Visibility::Public)],
                dependencies: Vec::new(),
                signatures: vec![("helper".into(), "fn()->Int32!{}".into())],
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
        );

        let mut context = ModuleCompileContext::default();
        context.imports.insert("util".into(), StableId(1));
        context.dependency_exports.insert(
            StableId(1),
            HashMap::from([("helper".into(), "fn()->Int32!{}".into())]),
        );
        let artifact_b = {
            let source = "import util\nfn main() { let _ = util.helper(); }\n".to_string();
            let unit = ModuleSourceUnit {
                id: crate::module::ModuleId(1),
                cache_key: ModuleCacheKey::new(source.as_bytes(), &[], "r", "t", 64, ""),
                metadata: ModuleMetadata {
                    format_version: 1,
                    stable_id: StableId(2),
                    abi_hash: 200,
                    exports: Vec::new(),
                    dependencies: vec![(StableId(1), 100)],
                    signatures: Vec::new(),
                    runtime_initializers: Vec::new(),
                    drop_glue: Vec::new(),
                },
                source,
            };
            Compiler::new()
                .unwrap()
                .frontend
                .compile_module(&unit, &context)
                .unwrap()
        };

        let linked = Linker::new(vec![artifact_a, artifact_b]).link().unwrap();
        assert_eq!(linked.mir.functions().len(), 2);
        assert!(linked
            .mir
            .functions()
            .iter()
            .all(|function| function.external_symbol.is_none()));
    }

    #[test]
    fn linker_remaps_forward_function_calls() {
        let artifact = compile_test_artifact(
            "fn first() -> Int32 { second() }\nfn second() -> Int32 { 1 }\n",
            ModuleMetadata {
                format_version: 2,
                stable_id: StableId(1),
                abi_hash: 100,
                exports: Vec::new(),
                dependencies: Vec::new(),
                signatures: Vec::new(),
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
        );

        let linked = Linker::new(vec![artifact]).link().unwrap();
        assert_eq!(linked.mir.functions().len(), 2);
    }

    #[test]
    fn imported_calls_preserve_parameter_names() {
        let dependency = compile_test_artifact(
            "fn add(left: Int32, right: Int32) -> Int32 { left + right }\n",
            ModuleMetadata {
                format_version: 2,
                stable_id: StableId(1),
                abi_hash: 100,
                exports: vec![("add".into(), crate::syntax::Visibility::Public)],
                dependencies: Vec::new(),
                signatures: vec![("add".into(), "fn(left:Int32,right:Int32)->Int32!{}".into())],
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
        );
        let mut context = ModuleCompileContext::default();
        context.imports.insert("util".into(), StableId(1));
        context.dependency_exports.insert(
            StableId(1),
            HashMap::from([("add".into(), "fn(left:Int32,right:Int32)->Int32!{}".into())]),
        );
        let source =
            "import util\nfn main() { let _ = util.add(left: 1, right: 2); }\n".to_string();
        let dependent = Compiler::new()
            .unwrap()
            .frontend
            .compile_module(
                &ModuleSourceUnit {
                    id: crate::module::ModuleId(1),
                    cache_key: ModuleCacheKey::new(source.as_bytes(), &[], "r", "t", 64, ""),
                    metadata: ModuleMetadata {
                        format_version: 2,
                        stable_id: StableId(2),
                        abi_hash: 200,
                        exports: Vec::new(),
                        dependencies: vec![(StableId(1), 100)],
                        signatures: Vec::new(),
                        runtime_initializers: Vec::new(),
                        drop_glue: Vec::new(),
                    },
                    source,
                },
                &context,
            )
            .unwrap();

        assert!(Linker::new(vec![dependency, dependent]).link().is_ok());
    }

    #[test]
    fn linker_rejects_abi_hash_mismatch() {
        let artifact_a = compile_test_artifact(
            "fn helper() -> Int32 { 1 }\n",
            ModuleMetadata {
                format_version: 1,
                stable_id: StableId(1),
                abi_hash: 100,
                exports: Vec::new(),
                dependencies: Vec::new(),
                signatures: Vec::new(),
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
        );

        let artifact_b = compile_test_artifact(
            "fn main() {}\n",
            ModuleMetadata {
                format_version: 1,
                stable_id: StableId(2),
                abi_hash: 200,
                exports: Vec::new(),
                dependencies: vec![(StableId(1), 999)],
                signatures: Vec::new(),
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
        );

        let result = Linker::new(vec![artifact_a, artifact_b]).link();
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.contains("ABI hash mismatch"), "unexpected error: {err}");
    }

    #[test]
    fn compile_with_cache_writes_jabi_to_disk() {
        let cache_dir =
            std::env::temp_dir().join(format!("joky-cache-test-{}", std::process::id()));
        let source = "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n".to_string();
        let unit = ModuleSourceUnit {
            id: crate::module::ModuleId(0),
            cache_key: ModuleCacheKey::new(source.as_bytes(), &[], "runtime", "target", 64, ""),
            metadata: ModuleMetadata {
                format_version: 1,
                stable_id: StableId(1),
                abi_hash: 42,
                exports: vec![("add".into(), crate::syntax::Visibility::Public)],
                dependencies: Vec::new(),
                signatures: vec![("add".into(), "fn(Int32,Int32)->Int32!{}".into())],
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
            source,
        };
        let mut compiler = Compiler::new().unwrap();
        let graph =
            ModuleGraph::load(std::path::Path::new("/dev/null"), std::path::Path::new(".")).ok();
        let metadata_map = HashMap::new();
        let graph = graph.unwrap_or_else(|| {
            ModuleGraph::load(&std::env::temp_dir().join("missing"), &std::env::temp_dir())
                .unwrap_or_else(|_| {
                    let root = cache_dir.join("pkg");
                    let src = root.join("src");
                    std::fs::create_dir_all(&src).unwrap();
                    std::fs::write(src.join("main.jk"), &unit.source).unwrap();
                    ModuleGraph::load(&src.join("main.jk"), &root).unwrap()
                })
        });
        let artifacts = compiler
            .frontend
            .compile_with_cache(
                std::slice::from_ref(&unit),
                &graph,
                &metadata_map,
                &cache_dir,
            )
            .unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].metadata.abi_hash, 42);
        let cache = crate::module::ModuleCache::new(&cache_dir);
        let loaded = cache.load(&unit.cache_key).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().abi_hash, 42);
        std::fs::remove_dir_all(cache_dir).unwrap();
    }

    #[test]
    fn compile_module_produces_artifact_with_mir() {
        let source = "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n".to_string();
        let unit = ModuleSourceUnit {
            id: crate::module::ModuleId(0),
            cache_key: ModuleCacheKey::new(source.as_bytes(), &[], "runtime", "target", 64, ""),
            metadata: ModuleMetadata {
                format_version: 1,
                stable_id: StableId(1),
                abi_hash: 42,
                exports: vec![("add".into(), crate::syntax::Visibility::Public)],
                dependencies: Vec::new(),
                signatures: vec![("add".into(), "fn(Int32,Int32)->Int32!{}".into())],
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
            source,
        };
        let compiler = Compiler::new().unwrap();
        let artifact = compiler
            .frontend
            .compile_module(&unit, &ModuleCompileContext::default())
            .unwrap();
        assert_eq!(artifact.metadata.abi_hash, 42);
        assert!(!artifact.mir.functions().is_empty());
        assert!(artifact.exports.contains_key("add"));
    }

    #[test]
    fn analyzes_independent_module_source_units() {
        let source = "fn main() {}\n".to_string();
        let unit = ModuleSourceUnit {
            id: crate::module::ModuleId(0),
            cache_key: ModuleCacheKey::new(source.as_bytes(), &[], "runtime", "target", 64, ""),
            metadata: ModuleMetadata {
                format_version: 1,
                stable_id: StableId(1),
                abi_hash: 1,
                exports: Vec::new(),
                dependencies: Vec::new(),
                signatures: Vec::new(),
                runtime_initializers: Vec::new(),
                drop_glue: Vec::new(),
            },
            source,
        };
        let compiler = Compiler::new().unwrap();
        compiler.analyze_module_units(&[unit]).unwrap();
        assert_ne!(source_fingerprint(b"fn main() {}\n"), 0);
    }

    #[test]
    fn analyzes_module_graph_after_boundary_checks() {
        let root = std::env::temp_dir().join(format!("joky-compiler-graph-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.jk"), "import util\nfn main() {}\n").unwrap();
        std::fs::write(src.join("util.jk"), "fn util() {}\n").unwrap();
        let graph = ModuleGraph::load(&src.join("main.jk"), &root).unwrap();
        let units = graph.source_units("runtime", "target", 64, "jit").unwrap();
        Compiler::new()
            .unwrap()
            .analyze_module_graph(&graph, &units)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn debug_sources_preserve_generic_definitions_through_cache_and_linking() {
        let root = std::env::temp_dir().join(format!("joky-debug-origins-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let utility = "pub fn copy(T: type, value: T) -> T { value }\n";
        std::fs::write(root.join("src/util.jk"), utility).unwrap();
        std::fs::write(
            root.join("src/main.jk"),
            "import util\nfn main() { println(util.copy(42)) }\n",
        )
        .unwrap();
        let graph = ModuleGraph::load(&root.join("src/main.jk"), &root).unwrap();
        let units = graph
            .source_units("test", env!("JOKY_TARGET"), 64, "jit")
            .unwrap();
        let defining = units
            .iter()
            .find(|unit| unit.source == utility)
            .unwrap()
            .metadata
            .stable_id;
        let mut compiler = Compiler::new().unwrap();
        for cached in [false, true] {
            let artifacts = compiler
                .frontend
                .compile_with_cache(&units, &graph, &graph.metadata_map(), &root.join("cache"))
                .unwrap();
            if cached {
                assert_eq!(compiler.frontend.module_compilations, 0);
            }
            let linked = Linker::new(artifacts).link().unwrap();
            let instance = linked
                .mir
                .functions
                .iter()
                .find(|function| {
                    function.source.module == Some(defining)
                        && function.name.contains("copy")
                        && function.source.body.is_some()
                })
                .expect("generic body from defining module");
            let span = instance.source.body.unwrap();
            assert_eq!(&utility[span.start()..span.end()], "{ value }");
            assert!(instance
                .source
                .values
                .iter()
                .flatten()
                .any(|span| &utility[span.start()..span.end()] == "value"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn custom_map_keys_require_explicit_hash_across_modules() {
        let keys = r#"
            struct Key { let value: Int32 }
            pub fn KeyType() -> type { Key }
            impl Hash for Key {}
            impl Eq for Key {}
            impl PartialEq for Key {
                fn equals(&self, other: &Self) -> Bool { true }
            }
            pub fn make() -> Key { Key(value: 1) }
        "#;
        for (name, source) in [
            ("direct", "import keys\nfn main() { Map.empty(keys.KeyType(), Int32) }"),
            ("generic", "import keys\nfn insert(K: type + Hash + Eq, key: K) { Map.empty(K, Int32).insert(key, 1); () } fn main() { insert(keys.make()) }"),
            ("nested", "import keys\nstruct Outer { let key: keys.KeyType() } impl Hash for Outer {} impl Eq for Outer {} fn main() { Map.empty(Outer, Int32) }"),
        ] {
            let error = try_run_modules(&format!("partial-eq-key-{name}"), &[("keys.jk", keys), ("main.jk", source)]).expect_err(name).to_string();
            assert!(error.contains("PartialEq") || error.contains("__MapKey"), "{name}: {error}");
        }
    }
}
