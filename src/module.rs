//! Package module graph and symbol metadata.
//!
//! The graph is deliberately independent from lowering: it gives the front end
//! stable module identities, resolved import edges, and exported symbol names
//! before source modules are combined for the current compiler pipeline.

pub(crate) mod abi;
mod cache;
pub(crate) mod constants;
pub(crate) mod generics;
pub mod resources;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use petgraph::algo::{kosaraju_scc, toposort};
use petgraph::graph::{DiGraph, NodeIndex};

use crate::syntax::Visibility;

#[derive(Debug)]
pub enum ModuleLoadError {
    Message(String),
    Diagnostic {
        path: PathBuf,
        source: String,
        diagnostic: crate::Diagnostic,
    },
}

impl std::fmt::Display for ModuleLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => formatter.write_str(message),
            Self::Diagnostic {
                path, diagnostic, ..
            } => write!(
                formatter,
                "failed to parse module '{}': {diagnostic}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ModuleLoadError {}

impl From<String> for ModuleLoadError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

/// Persistent identity used by module metadata. This is deliberately separate
/// from the compact, discovery-order `ModuleId` used by the current compiler.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct StableId(pub u64);

/// Stable cross-module symbol identity. This replaces string-prefixed names such
/// as `m7_exposed` in the modular compilation pipeline.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SymbolId {
    pub module: StableId,
    pub name: String,
}

/// A symbol exported from a compiled module artifact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportedSymbol {
    Function { mir_index: usize, signature: String },
    Constant { type_annotation: String },
    Template { signature: String },
    TypeFunction { signature: String },
}

/// Link-time resolution of a symbol to a concrete MIR function index in the
/// merged program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSymbol {
    pub mir_index: usize,
}

/// Single-module compilation output: metadata boundary, lowered MIR, and symbol tables.
///
/// Indices inside [`crate::mir::MirProgram`] are valid only within this artifact.
/// Cross-module references use [`SymbolId`] and are resolved by the linker.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ModuleArtifact {
    pub metadata: ModuleMetadata,
    pub mir: crate::mir::MirProgram,
    pub interface: crate::sema::ModuleInterface,
    pub imports: Vec<SymbolId>,
    pub exports: HashMap<String, ExportedSymbol>,
    pub template_program: Option<crate::syntax::Program>,
}

/// Immutable dependency registry shared by import checkers. Nested type queries
/// use copy-on-write on the registry when installing their private type snapshot.
pub(crate) type DependencyTypes =
    std::sync::Arc<HashMap<StableId, std::sync::Arc<crate::sema::ModuleTypes>>>;

/// Import metadata supplied to per-module semantic analysis.
#[derive(Debug, Clone, Default)]
pub struct ModuleCompileContext {
    pub(crate) source_path: String,
    pub(crate) target_os: String,
    pub(crate) type_bindings: HashMap<String, (StableId, crate::sema::Type)>,
    pub(crate) definition_module: Option<StableId>,
    pub(crate) standard_modules: std::collections::HashSet<StableId>,
    pub(crate) type_query: Option<crate::syntax::TypeAnnotation>,
    pub(crate) type_query_depth: usize,
    pub(crate) dependency_types: DependencyTypes,
    /// Import alias (for example `util`) to dependency stable identity.
    pub imports: HashMap<String, StableId>,
    /// Public export signatures keyed by dependency stable identity.
    pub dependency_exports: HashMap<StableId, HashMap<String, String>>,
}

const ABI_METADATA_VERSION: u16 = 46;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub usize);

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub resources: BTreeMap<String, resources::EmbeddedResource>,
    pub id: ModuleId,
    pub path: PathBuf,
    pub imports: Vec<(String, ModuleId)>,
    pub exports: HashMap<String, Visibility>,
    pub export_signatures: HashMap<String, String>,
    pub stable_id: StableId,
    pub abi_hash: u64,
}

#[derive(Debug, Clone)]
pub struct ModuleSourceUnit {
    pub resources: BTreeMap<String, resources::EmbeddedResource>,
    pub id: ModuleId,
    pub source: String,
    pub metadata: ModuleMetadata,
    pub cache_key: ModuleCacheKey,
}

impl ModuleSourceUnit {
    pub fn verify(&self) -> Result<(), String> {
        if resources::fingerprints(&self.resources) != self.cache_key.resource_hashes {
            return Err(format!(
                "module {:?} resource fingerprint mismatch",
                self.id
            ));
        }
        let actual = source_fingerprint(self.source.as_bytes());
        if actual != self.cache_key.source_hash {
            return Err(format!(
                "module {:?} source fingerprint mismatch ({:016x} != {:016x})",
                self.id, actual, self.cache_key.source_hash
            ));
        }
        if self.metadata.stable_id == StableId(0) {
            return Err(format!(
                "module {:?} has an invalid stable identity",
                self.id
            ));
        }
        Ok(())
    }
}

/// Persistable metadata boundary for a compiled module. The textual encoding
/// is intentionally deterministic so it can be diffed in cache diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModuleMetadata {
    pub format_version: u16,
    pub stable_id: StableId,
    pub abi_hash: u64,
    pub exports: Vec<(String, Visibility)>,
    pub dependencies: Vec<(StableId, u64)>,
    pub signatures: Vec<(String, String)>,
    pub runtime_initializers: Vec<String>,
    pub drop_glue: Vec<String>,
}

/// Frontend token stored in [`ModuleCacheKey::features`].
///
/// JIT and AOT share parse, check, HIR and MIR, so they share this value and
/// the resulting `.jmir` artifacts. Backend choice (object vs JIT, opt level,
/// debug info) is not part of the MIR key.
pub const MODULE_MIR_FEATURES: &str = "mir";

/// Inputs that determine whether a compiled module artifact can be reused.
/// Keeping this explicit prevents accidental cache hits across ABI/runtime
/// or target changes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModuleCacheKey {
    pub resource_hashes: Vec<[u8; 32]>,
    pub module_identity: StableId,
    pub source_hash: u64,
    pub dependency_abi_hashes: Vec<u64>,
    pub compiler_abi_version: u16,
    pub runtime_version: String,
    pub target: String,
    pub pointer_width: u8,
    /// Frontend feature token. Production compiles use [`MODULE_MIR_FEATURES`].
    pub features: String,
}

/// Inputs that determine whether a linked AOT object can be reused.
///
/// The key is the closed source graph plus backend options. Generic instances
/// are a function of those sources, so they are not listed separately.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AotObjectCacheKey {
    pub runtime_version: String,
    pub compiler_abi_version: u16,
    pub target: String,
    pub pointer_width: u8,
    pub release: bool,
    pub debug_info: bool,
    pub unit_fingerprints: Vec<u64>,
}

/// Native object and launcher metadata stored beside `.jmir` artifacts.
pub(crate) type ProviderOperation = (String, String, u64);
pub(crate) type ProviderOperations = Vec<ProviderOperation>;
pub(crate) type ProviderMetadata = (String, ProviderOperations);
pub(crate) type ProviderMetadataList = Vec<ProviderMetadata>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AotObjectArtifact {
    pub bytes: Vec<u8>,
    pub machine_entries: Vec<(usize, String)>,
    pub providers: ProviderMetadataList,
    pub main_returns_result: bool,
    pub main_is_suspending: bool,
}

pub struct ModuleCache {
    root: PathBuf,
    enabled: bool,
}

impl ModuleCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            enabled: true,
        }
    }

    pub(crate) fn disabled() -> Self {
        Self {
            root: PathBuf::new(),
            enabled: false,
        }
    }

    pub(crate) fn miss_reason(&self, key: &ModuleCacheKey) -> &'static str {
        if !self.enabled {
            "cache disabled"
        } else if self.path(key).with_extension("jmir").exists() {
            "invalid or incompatible artifact"
        } else {
            "no artifact for current source, dependency ABI and build key"
        }
    }

    pub(crate) fn object_miss_reason(&self, key: &AotObjectCacheKey) -> &'static str {
        if !self.enabled {
            "cache disabled"
        } else if self.object_path(key).exists() {
            "invalid or incompatible artifact"
        } else {
            "no artifact for current source graph and AOT profile"
        }
    }

    fn object_path(&self, key: &AotObjectCacheKey) -> PathBuf {
        self.root.join(format!("{:016x}.jo", key.fingerprint()))
    }

    fn path(&self, key: &ModuleCacheKey) -> PathBuf {
        self.root.join(format!("{:016x}.jabi", key.fingerprint()))
    }

    pub fn load(&self, key: &ModuleCacheKey) -> Result<Option<ModuleMetadata>, String> {
        let path = self.path(key);
        match std::fs::read_to_string(&path) {
            Ok(contents) => ModuleMetadata::decode(&contents).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "failed to read module cache '{}': {error}",
                path.display()
            )),
        }
    }

    pub fn load_compatible(
        &self,
        key: &ModuleCacheKey,
        expected: &ModuleMetadata,
    ) -> Result<Option<ModuleMetadata>, String> {
        let Some(actual) = self.load(key)? else {
            return Ok(None);
        };
        if let Some(issue) = actual.compatibility_issue(expected) {
            return Err(format!("cached module ABI is incompatible: {issue}"));
        }
        Ok(Some(actual))
    }

    pub fn store(&self, key: &ModuleCacheKey, metadata: &ModuleMetadata) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        std::fs::create_dir_all(&self.root).map_err(|error| {
            format!(
                "failed to create module cache '{}': {error}",
                self.root.display()
            )
        })?;
        let path = self.path(key);
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temporary = path.with_extension(format!(
            "jabi.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&temporary, metadata.encode()).map_err(|error| {
            format!(
                "failed to write module cache '{}': {error}",
                temporary.display()
            )
        })?;
        std::fs::rename(&temporary, &path).map_err(|error| {
            format!(
                "failed to publish module cache '{}': {error}",
                path.display()
            )
        })
    }
}

impl ModuleCacheKey {
    pub fn new(
        source: &[u8],
        dependencies: &[ModuleMetadata],
        runtime_version: impl Into<String>,
        target: impl Into<String>,
        pointer_width: u8,
        features: impl Into<String>,
    ) -> Self {
        Self {
            module_identity: StableId(0),
            resource_hashes: Vec::new(),
            source_hash: source_fingerprint(source),
            dependency_abi_hashes: {
                let mut hashes = dependencies
                    .iter()
                    .map(|metadata| metadata.abi_hash)
                    .collect::<Vec<_>>();
                hashes.sort_unstable();
                hashes
            },
            compiler_abi_version: ABI_METADATA_VERSION,
            runtime_version: runtime_version.into(),
            target: target.into(),
            pointer_width,
            features: features.into(),
        }
    }

    pub fn fingerprint(&self) -> u64 {
        let mut bytes = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}|",
            self.module_identity.0,
            self.source_hash,
            self.compiler_abi_version,
            self.runtime_version,
            self.target,
            self.pointer_width,
            self.features,
            self.dependency_abi_hashes.len()
        )
        .into_bytes();
        for hash in &self.dependency_abi_hashes {
            bytes.extend_from_slice(&hash.to_le_bytes());
        }
        for hash in &self.resource_hashes {
            bytes.extend_from_slice(hash);
        }
        stable_id("cache", &bytes).0
    }
}

impl AotObjectCacheKey {
    pub(crate) fn new(
        units: &[ModuleSourceUnit],
        runtime_version: impl Into<String>,
        target: impl Into<String>,
        pointer_width: u8,
        release: bool,
        debug_info: bool,
    ) -> Self {
        let mut unit_fingerprints = units
            .iter()
            .map(|unit| unit.cache_key.fingerprint())
            .collect::<Vec<_>>();
        unit_fingerprints.sort_unstable();
        Self {
            runtime_version: runtime_version.into(),
            compiler_abi_version: ABI_METADATA_VERSION,
            target: target.into(),
            pointer_width,
            release,
            debug_info,
            unit_fingerprints,
        }
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        let mut bytes = format!(
            "{}:{}:{}:{}:{}:{}:{}|",
            self.runtime_version,
            self.compiler_abi_version,
            self.target,
            self.pointer_width,
            self.release,
            self.debug_info,
            self.unit_fingerprints.len()
        )
        .into_bytes();
        for fingerprint in &self.unit_fingerprints {
            bytes.extend_from_slice(&fingerprint.to_le_bytes());
        }
        stable_id("aot-object", &bytes).0
    }
}

pub fn source_fingerprint(source: &[u8]) -> u64 {
    stable_id("source", source).0
}

impl ModuleMetadata {
    pub fn from_info(info: &ModuleInfo) -> Self {
        let mut exports = info
            .exports
            .iter()
            .filter(|(_, visibility)| **visibility == Visibility::Public)
            .map(|(name, visibility)| (name.clone(), *visibility))
            .collect::<Vec<_>>();
        exports.sort_by(|left, right| left.0.cmp(&right.0));
        let mut signatures = info
            .export_signatures
            .iter()
            .filter(|(name, _)| info.exports.get(*name) == Some(&Visibility::Public))
            .map(|(name, sig)| (name.clone(), sig.clone()))
            .collect::<Vec<_>>();
        signatures.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            format_version: ABI_METADATA_VERSION,
            stable_id: info.stable_id,
            abi_hash: info.abi_hash,
            exports,
            dependencies: Vec::new(),
            signatures,
            runtime_initializers: Vec::new(),
            drop_glue: Vec::new(),
        }
    }

    pub fn encode(&self) -> String {
        let mut output = format!(
            "joky-module-abi {}\nmodule {:016x}\nabi {:016x}\n",
            self.format_version, self.stable_id.0, self.abi_hash
        );
        for (name, visibility) in &self.exports {
            let marker = if *visibility == Visibility::Public {
                "pub"
            } else {
                "private"
            };
            output.push_str(&format!("export {marker} {name}\n"));
        }
        for (name, signature) in &self.signatures {
            output.push_str(&format!("signature {name} {signature}\n"));
        }
        for (module, abi) in &self.dependencies {
            output.push_str(&format!("dependency {:016x} {:016x}\n", module.0, abi));
        }
        for initializer in &self.runtime_initializers {
            output.push_str(&format!("runtime-init {initializer}\n"));
        }
        for glue in &self.drop_glue {
            output.push_str(&format!("drop-glue {glue}\n"));
        }
        output
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let mut lines = input.lines();
        let header = lines.next().ok_or("missing ABI metadata header")?;
        let version = header
            .strip_prefix("joky-module-abi ")
            .ok_or("invalid ABI metadata header")?
            .parse::<u16>()
            .map_err(|_| "invalid ABI metadata version".to_string())?;
        let module = parse_hex_line(lines.next(), "module")?;
        let abi_hash = parse_hex_line(lines.next(), "abi")?;
        let mut exports = Vec::new();
        let mut dependencies = Vec::new();
        let mut signatures = Vec::new();
        let mut runtime_initializers = Vec::new();
        let mut drop_glue = Vec::new();
        for line in lines {
            let mut fields = line.splitn(3, ' ');
            match fields.next() {
                Some("export") => {
                    let visibility = match fields.next() {
                        Some("pub") => Visibility::Public,
                        Some("private") => Visibility::Private,
                        _ => return Err("invalid export visibility".into()),
                    };
                    let name = fields.next().ok_or("missing export name")?;
                    exports.push((name.to_string(), visibility));
                }
                Some("dependency") => {
                    let stable = fields.next().ok_or("missing dependency identity")?;
                    let hash = fields.next().ok_or("missing dependency ABI hash")?;
                    dependencies.push((StableId(parse_hex(stable)?), parse_hex(hash)?));
                }
                Some("signature") => {
                    let name = fields.next().ok_or("missing signature name")?;
                    let signature = fields.next().ok_or("missing signature value")?;
                    signatures.push((name.to_string(), signature.to_string()));
                }
                Some("runtime-init") => runtime_initializers.push(
                    fields
                        .next()
                        .ok_or("missing runtime initializer")?
                        .to_string(),
                ),
                Some("drop-glue") => {
                    drop_glue.push(fields.next().ok_or("missing drop glue")?.to_string())
                }
                Some(other) => return Err(format!("unknown ABI metadata record '{other}'")),
                None => {}
            }
        }
        Ok(Self {
            format_version: version,
            stable_id: StableId(module),
            abi_hash,
            exports,
            dependencies,
            signatures,
            runtime_initializers,
            drop_glue,
        })
    }

    pub fn is_compatible_with(&self, previous: &Self) -> bool {
        self.compatibility_issue(previous).is_none()
    }

    pub fn compatibility_issue(&self, previous: &Self) -> Option<String> {
        if self.format_version != previous.format_version {
            return Some(format!(
                "module ABI metadata version changed ({} -> {})",
                previous.format_version, self.format_version
            ));
        }
        if self.stable_id != previous.stable_id {
            return Some(format!(
                "module identity changed ({:016x} -> {:016x})",
                previous.stable_id.0, self.stable_id.0
            ));
        }
        if self.abi_hash != previous.abi_hash {
            return Some(format!(
                "public ABI changed ({:016x} -> {:016x})",
                previous.abi_hash, self.abi_hash
            ));
        }
        if self.runtime_initializers != previous.runtime_initializers {
            return Some("runtime initialization metadata changed".into());
        }
        if self.drop_glue != previous.drop_glue {
            return Some("drop glue metadata changed".into());
        }
        None
    }
}

fn parse_hex_line(line: Option<&str>, label: &str) -> Result<u64, String> {
    let line = line.ok_or_else(|| format!("missing {label} metadata"))?;
    let value = line
        .strip_prefix(&format!("{label} "))
        .ok_or_else(|| format!("invalid {label} metadata"))?;
    parse_hex(value)
}

fn parse_hex(value: &str) -> Result<u64, String> {
    u64::from_str_radix(value, 16).map_err(|_| format!("invalid hexadecimal value '{value}'"))
}

#[derive(Debug)]
pub struct ModuleGraph {
    package_root: PathBuf,
    graph: DiGraph<ModuleId, ()>,
    nodes: Vec<ModuleInfo>,
    by_path: HashMap<PathBuf, ModuleId>,
    /// Source snapshots supplied by an embedding frontend. Disk-backed graphs
    /// leave this empty; snapshots override files without writing them out.
    source_overrides: HashMap<PathBuf, String>,
}

/// Metadata retained by a long-lived frontend between source checks.
///
/// The source hash is part of the entry so an editor update invalidates only
/// the changed module. The stable identity guard keeps a cache entry from one
/// package layout from being reused for another layout at the same path.
#[derive(Debug, Clone)]
struct CachedModule {
    resource_paths: BTreeMap<String, crate::Span>,
    source_hash: u64,
    stable_id: StableId,
    exports: HashMap<String, Visibility>,
    export_signatures: HashMap<String, String>,
    abi_hash: u64,
    imports: Vec<String>,
}

/// In-memory module discovery cache used by reusable frontend services.
///
/// Disk MIR caching already avoids semantic and lowering work. This cache
/// avoids reparsing unchanged modules and rebuilding their export signatures
/// and import list before the MIR cache can be queried.
#[derive(Debug)]
pub(crate) struct ModuleGraphCache {
    modules: HashMap<PathBuf, CachedModule>,
    hits: usize,
    misses: usize,
    enabled: bool,
}

impl Default for ModuleGraphCache {
    fn default() -> Self {
        Self {
            modules: HashMap::new(),
            hits: 0,
            misses: 0,
            enabled: true,
        }
    }
}

impl ModuleGraphCache {
    fn disabled() -> Self {
        Self {
            modules: HashMap::new(),
            hits: 0,
            misses: 0,
            enabled: false,
        }
    }

    pub(crate) fn take_stats(&mut self) -> (usize, usize) {
        (
            std::mem::take(&mut self.hits),
            std::mem::take(&mut self.misses),
        )
    }

    pub(crate) fn clear(&mut self) {
        self.modules.clear();
        self.hits = 0;
        self.misses = 0;
    }

    fn load_or_parse(
        &mut self,
        path: &Path,
        source: &str,
        stable_id: StableId,
    ) -> Result<CachedModule, ModuleLoadError> {
        let source_hash = source_fingerprint(source.as_bytes());
        if self.enabled {
            if let Some(cached) = self
                .modules
                .get(path)
                .filter(|cached| cached.source_hash == source_hash && cached.stable_id == stable_id)
                .cloned()
            {
                self.hits += 1;
                return Ok(cached);
            }
        }

        self.misses += 1;
        let (program, resource_paths) = crate::syntax::parse_program_resources(
            source,
            &path.to_string_lossy(),
            BTreeMap::new(),
        )
        .map_err(|diagnostic| ModuleLoadError::Diagnostic {
            path: path.to_path_buf(),
            source: source.to_owned(),
            diagnostic,
        })?;
        let mut exports: HashMap<String, Visibility> = program
            .functions
            .iter()
            .map(|function| (function.name.clone(), function.visibility))
            .chain(
                program
                    .constants
                    .iter()
                    .map(|constant| (constant.name.clone(), constant.visibility)),
            )
            .collect();
        let mut export_signatures = program
            .functions
            .iter()
            .map(|function| {
                let params = function
                    .parameters
                    .iter()
                    .map(|parameter| {
                        format!(
                            "{}:{}{}",
                            parameter.name,
                            if parameter.borrowed { "&" } else { "" },
                            parameter.ty
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let result = function
                    .return_type
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "Unit".into());
                let effects = function
                    .effect_names
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                (
                    function.name.clone(),
                    format!("fn({params})->{result}!{{{effects}}}"),
                )
            })
            .chain(program.constants.iter().map(|constant| {
                (
                    constant.name.clone(),
                    constant
                        .annotation
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "inferred".into()),
                )
            }))
            .collect::<HashMap<_, _>>();
        for (name, definition) in abi::declaration_signatures(&program, stable_id) {
            exports.insert(name.clone(), Visibility::Public);
            export_signatures.insert(name, definition);
        }
        let abi_hash = module_abi_hash_with_signatures(&exports, &export_signatures);
        let imports = program
            .imports
            .iter()
            .map(|import| import.path.join("/"))
            .collect();
        let cached = CachedModule {
            resource_paths,
            source_hash,
            stable_id,
            exports,
            export_signatures,
            abi_hash,
            imports,
        };
        if self.enabled {
            self.modules.insert(path.to_path_buf(), cached.clone());
        }
        Ok(cached)
    }
}

impl ModuleGraph {
    pub fn load(entry: &Path, package_root: &Path) -> Result<Self, ModuleLoadError> {
        let mut cache = ModuleGraphCache::disabled();
        Self::load_with_overrides(entry, package_root, HashMap::new(), &mut cache)
    }

    /// Load a module graph while taking source text from memory for the given
    /// paths. Files not present in `sources` continue to be read from disk,
    /// which keeps the standard library usable by editor integrations.
    pub fn load_with_sources(
        entry: &Path,
        package_root: &Path,
        sources: &[(PathBuf, String)],
    ) -> Result<Self, ModuleLoadError> {
        let normalized_root = normalized_path(package_root);
        let overrides = sources
            .iter()
            .map(|(path, source)| {
                (
                    normalized_source_path(path, &normalized_root),
                    source.clone(),
                )
            })
            .collect();
        let mut cache = ModuleGraphCache::disabled();
        Self::load_with_overrides(entry, package_root, overrides, &mut cache)
    }

    pub(crate) fn load_with_sources_cached(
        entry: &Path,
        package_root: &Path,
        sources: &[(PathBuf, String)],
        cache: &mut ModuleGraphCache,
    ) -> Result<Self, ModuleLoadError> {
        let normalized_root = normalized_path(package_root);
        let overrides = sources
            .iter()
            .map(|(path, source)| {
                (
                    normalized_source_path(path, &normalized_root),
                    source.clone(),
                )
            })
            .collect();
        Self::load_with_overrides(entry, package_root, overrides, cache)
    }

    fn load_with_overrides(
        entry: &Path,
        package_root: &Path,
        source_overrides: HashMap<PathBuf, String>,
        cache: &mut ModuleGraphCache,
    ) -> Result<Self, ModuleLoadError> {
        let mut graph = Self {
            package_root: package_root
                .canonicalize()
                .unwrap_or_else(|_| package_root.to_path_buf()),
            graph: DiGraph::new(),
            nodes: Vec::new(),
            by_path: HashMap::new(),
            source_overrides,
        };
        let entry = if entry.is_absolute() || entry.exists() {
            entry.to_owned()
        } else {
            PathBuf::from(package_root).join(entry)
        };
        graph.discover(&entry, package_root, cache)?;
        graph.check_cycles()?;
        Ok(graph)
    }

    pub fn module(&self, id: ModuleId) -> &ModuleInfo {
        &self.nodes[id.0]
    }

    /// Return the current source snapshot for a discovered module.
    pub fn source(&self, id: ModuleId) -> Result<String, String> {
        self.read_source(&self.module(id).path)
    }

    pub fn metadata(&self, id: ModuleId) -> ModuleMetadata {
        let mut metadata = ModuleMetadata::from_info(self.module(id));
        metadata.dependencies = self
            .module(id)
            .imports
            .iter()
            .map(|(_, dependency)| {
                let info = self.module(*dependency);
                (info.stable_id, info.abi_hash)
            })
            .collect();
        metadata
            .dependencies
            .sort_by_key(|(stable_id, _)| stable_id.0);
        metadata
    }

    /// Returns modules in dependency-first order for independent compilation.
    pub fn compilation_order(&self) -> Result<Vec<ModuleId>, String> {
        let mut order =
            toposort(&self.graph, None).map_err(|_| "module graph contains a cycle".to_string())?;
        order.reverse();
        Ok(order.into_iter().map(|node| self.graph[node]).collect())
    }

    pub fn check_dependency_metadata(
        &self,
        id: ModuleId,
        available: &HashMap<StableId, ModuleMetadata>,
    ) -> Result<(), String> {
        for (_, dependency) in &self.module(id).imports {
            let expected = self.metadata(*dependency);
            let actual = available.get(&expected.stable_id).ok_or_else(|| {
                format!(
                    "missing ABI metadata for dependency {:016x}",
                    expected.stable_id.0
                )
            })?;
            if let Some(issue) = actual.compatibility_issue(&expected) {
                return Err(format!("dependency {:016x}: {issue}", expected.stable_id.0));
            }
        }
        Ok(())
    }

    pub fn metadata_map(&self) -> HashMap<StableId, ModuleMetadata> {
        self.nodes
            .iter()
            .map(|info| {
                let metadata = self.metadata(info.id);
                (metadata.stable_id, metadata)
            })
            .collect()
    }

    pub fn cache_key(
        &self,
        id: ModuleId,
        runtime_version: impl Into<String>,
        target: impl Into<String>,
        pointer_width: u8,
        features: impl Into<String>,
    ) -> Result<ModuleCacheKey, String> {
        let info = self.module(id);
        let source = self
            .read_source(&info.path)
            .map_err(|error| format!("failed to read module '{}': {error}", info.path.display()))?;
        let dependencies = info
            .imports
            .iter()
            .map(|(_, dependency)| self.metadata(*dependency))
            .collect::<Vec<_>>();
        let mut key = ModuleCacheKey::new(
            source.as_bytes(),
            &dependencies,
            runtime_version,
            target,
            pointer_width,
            features,
        );
        key.module_identity = info.stable_id;
        key.resource_hashes = resources::fingerprints(&info.resources);
        Ok(key)
    }

    pub fn source_units(
        &self,
        runtime_version: impl Into<String> + Clone,
        target: impl Into<String> + Clone,
        pointer_width: u8,
        features: impl Into<String> + Clone,
    ) -> Result<Vec<ModuleSourceUnit>, String> {
        let mut units = Vec::new();
        for id in self.compilation_order()? {
            let info = self.module(id);
            let source = self.read_source(&info.path).map_err(|error| {
                format!("failed to read module '{}': {error}", info.path.display())
            })?;
            let mut cache_key = self.cache_key(
                id,
                runtime_version.clone(),
                target.clone(),
                pointer_width,
                features.clone(),
            )?;
            cache_key.features.push_str(if id == ModuleId(0) {
                ":entry"
            } else {
                ":library"
            });
            units.push(ModuleSourceUnit {
                resources: info.resources.clone(),
                id,
                source,
                metadata: self.metadata(id),
                cache_key,
            });
        }
        Ok(units)
    }

    pub fn verify_source_units(&self, units: &[ModuleSourceUnit]) -> Result<(), String> {
        if units.len() != self.nodes.len() {
            return Err(format!(
                "module source unit count mismatch ({} != {})",
                units.len(),
                self.nodes.len()
            ));
        }
        for unit in units {
            unit.verify()?;
        }
        Ok(())
    }

    pub fn entry_id(&self, entry: &Path) -> Option<ModuleId> {
        entry
            .canonicalize()
            .ok()
            .and_then(|path| self.by_path.get(&path).copied())
    }

    fn discover(
        &mut self,
        path: &Path,
        package_root: &Path,
        cache: &mut ModuleGraphCache,
    ) -> Result<ModuleId, ModuleLoadError> {
        let canonical = self
            .canonical_path(path)
            .map_err(ModuleLoadError::Message)?;
        if let Some(id) = self.by_path.get(&canonical).copied() {
            return Ok(id);
        }

        let id = ModuleId(self.nodes.len());
        self.by_path.insert(canonical.clone(), id);
        let canonical_root = package_root
            .canonicalize()
            .unwrap_or_else(|_| package_root.to_path_buf());
        let identity_path = canonical
            .strip_prefix(&canonical_root)
            .unwrap_or(&canonical);
        self.nodes.push(ModuleInfo {
            resources: Default::default(),
            id,
            path: canonical.clone(),
            imports: Vec::new(),
            exports: HashMap::new(),
            export_signatures: HashMap::new(),
            stable_id: stable_id("module", identity_path.to_string_lossy().as_bytes()),
            abi_hash: 0,
        });
        self.graph.add_node(id);

        let source = self
            .read_source(&canonical)
            .map_err(|error| format!("failed to read module '{}': {error}", path.display()))?;
        let parsed = cache.load_or_parse(&canonical, &source, self.nodes[id.0].stable_id)?;
        self.nodes[id.0].exports = parsed.exports;
        self.nodes[id.0].export_signatures = parsed.export_signatures;
        self.nodes[id.0].abi_hash = parsed.abi_hash;
        self.nodes[id.0].resources =
            resources::snapshot(&canonical, &source, &parsed.resource_paths)?;

        for module_name in parsed.imports {
            let dependency_path = self.resolve_module_path(&module_name, package_root)?;
            let dependency = self.discover(&dependency_path, package_root, cache)?;
            self.nodes[id.0].imports.push((module_name, dependency));
            self.graph
                .add_edge(NodeIndex::new(id.0), NodeIndex::new(dependency.0), ());
        }
        Ok(id)
    }

    fn check_cycles(&self) -> Result<(), String> {
        for component in kosaraju_scc(&self.graph) {
            let self_cycle =
                component.len() == 1 && self.graph.find_edge(component[0], component[0]).is_some();
            if component.len() > 1 || self_cycle {
                let modules = component
                    .iter()
                    .map(|node| self.module(self.graph[*node]).path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" -> ");
                return Err(format!("module dependency cycle: {modules}"));
            }
        }
        Ok(())
    }

    fn canonical_path(&self, path: &Path) -> Result<PathBuf, String> {
        match path.canonicalize() {
            Ok(canonical) => Ok(canonical),
            Err(error) => {
                let normalized = normalized_path(path);
                if self.source_overrides.contains_key(&normalized) {
                    Ok(normalized)
                } else {
                    Err(format!(
                        "failed to read module '{}': {error}",
                        path.display()
                    ))
                }
            }
        }
    }

    fn read_source(&self, path: &Path) -> Result<String, String> {
        let normalized = normalized_path(path);
        self.source_overrides
            .get(&normalized)
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| {
                std::fs::read_to_string(&normalized).map_err(|error| format!("{error}"))
            })
    }

    fn resolve_module_path(&self, module: &str, package_root: &Path) -> Result<PathBuf, String> {
        let components = module.split('/').collect::<Vec<_>>();
        if components.is_empty()
            || components
                .iter()
                .any(|component| component.is_empty() || *component == "." || *component == "..")
        {
            return Err(format!("invalid module path '{module}'"));
        }
        let base = if components.first() == Some(&"joky") {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std")
        } else {
            package_root.join("src")
        };
        let path = components
            .into_iter()
            .fold(base, |path, component| path.join(component))
            .with_extension("jk");
        let normalized = normalized_path(&path);
        if self.source_overrides.contains_key(&normalized) || path.is_file() {
            Ok(normalized)
        } else {
            Err(format!(
                "module '{module}' not found at '{}'",
                path.display()
            ))
        }
    }
}

fn normalized_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn normalized_source_path(path: &Path, package_root: &Path) -> PathBuf {
    if path.is_absolute() || path.exists() {
        normalized_path(path)
    } else {
        normalized_path(&package_root.join(path))
    }
}

/// Versioned, deterministic fingerprint of the public surface of a module.
/// Export names and visibility are sorted before hashing, so source ordering
/// and hash-map iteration cannot invalidate downstream caches.
pub fn module_abi_hash(exports: &HashMap<String, Visibility>) -> u64 {
    module_abi_hash_with_signatures(exports, &HashMap::new())
}

pub fn module_abi_hash_with_signatures(
    exports: &HashMap<String, Visibility>,
    signatures: &HashMap<String, String>,
) -> u64 {
    let mut entries = exports
        .iter()
        .filter(|(_, visibility)| **visibility == Visibility::Public)
        .map(|(name, visibility)| {
            format!(
                "{}:{}:{}",
                name,
                if *visibility == Visibility::Public {
                    "pub"
                } else {
                    "private"
                },
                signatures
                    .get(name)
                    .map(String::as_str)
                    .unwrap_or("<unknown>")
            )
        })
        .collect::<Vec<_>>();
    entries.sort();
    let mut bytes = format!("joky-abi-v{}\0", ABI_METADATA_VERSION).into_bytes();
    for entry in entries {
        bytes.extend_from_slice(entry.as_bytes());
        bytes.push(0);
    }
    stable_id("abi", &bytes).0
}

fn stable_id(domain: &str, bytes: &[u8]) -> StableId {
    // FNV-1a with an explicit domain gives a small, portable identity without
    // depending on platform or process-randomized hash state.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in domain.as_bytes().iter().chain([0].iter()).chain(bytes) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    StableId(hash)
}

/// Canonical internal key for a [`SymbolId`].
pub fn symbol_key(symbol: &SymbolId) -> String {
    format!("@sym/{:016x}/{}", symbol.module.0, symbol.name)
}

impl ModuleGraph {
    pub fn compile_context(
        &self,
        id: ModuleId,
        metadata_map: &HashMap<StableId, ModuleMetadata>,
    ) -> ModuleCompileContext {
        let mut context = ModuleCompileContext {
            source_path: self
                .module(id)
                .path
                .strip_prefix(&self.package_root)
                .unwrap_or(&self.module(id).path)
                .to_string_lossy()
                .replace('\\', "/"),
            target_os: std::env::consts::OS.to_owned(),
            definition_module: Some(self.module(id).stable_id),
            ..Default::default()
        };
        for (module_path, dependency) in &self.module(id).imports {
            let alias = module_path.rsplit('/').next().unwrap_or(module_path);
            let dependency_metadata = metadata_map
                .get(&self.module(*dependency).stable_id)
                .cloned()
                .unwrap_or_else(|| self.metadata(*dependency));
            context
                .imports
                .insert(alias.to_owned(), dependency_metadata.stable_id);
            if module_path.starts_with("joky/") {
                context
                    .standard_modules
                    .insert(dependency_metadata.stable_id);
            }
            let exports = dependency_metadata
                .signatures
                .iter()
                .filter(|(name, _)| {
                    dependency_metadata
                        .exports
                        .iter()
                        .any(|(export, visibility)| {
                            export == name && *visibility == Visibility::Public
                        })
                })
                .cloned()
                .collect::<HashMap<_, _>>();
            context
                .dependency_exports
                .insert(dependency_metadata.stable_id, exports);
        }
        context
    }
}

pub fn resolve_module_path(module: &str, package_root: &Path) -> Result<PathBuf, String> {
    let components = module.split('/').collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        return Err(format!("invalid module path '{module}'"));
    }
    let base = if components.first() == Some(&"joky") {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std")
    } else {
        package_root.join("src")
    };
    let path = components
        .into_iter()
        .fold(base, |path, component| path.join(component))
        .with_extension("jk");
    if !path.is_file() {
        return Err(format!(
            "module '{module}' not found at '{}'",
            path.display()
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_assigns_ids_and_collects_exports() {
        let root = std::env::temp_dir().join(format!("joky-module-graph-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.jk"), "import util\nfn main() {}\n").unwrap();
        std::fs::write(
            src.join("util.jk"),
            "pub const MAX: Int32 = 3\npub fn exposed() {}\nconst HIDDEN: Int32 = 0\nfn hidden() {}\n",
        )
        .unwrap();

        let graph = ModuleGraph::load(&src.join("main.jk"), &root).unwrap();
        let entry = graph.entry_id(&src.join("main.jk")).unwrap();
        let util = graph.module(entry).imports[0].1;
        assert_eq!(graph.module(util).exports["exposed"], Visibility::Public);
        assert_eq!(graph.module(util).exports["hidden"], Visibility::Private);
        assert_eq!(graph.module(util).exports["MAX"], Visibility::Public);
        assert_eq!(graph.module(util).exports["HIDDEN"], Visibility::Private);
        assert_ne!(entry, util);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graph_rejects_import_cycles() {
        let root = std::env::temp_dir().join(format!("joky-module-cycle-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.jk"), "import a\nfn main() {}\n").unwrap();
        std::fs::write(src.join("a.jk"), "import b\nfn a() {}\n").unwrap();
        std::fs::write(src.join("b.jk"), "import a\nfn b() {}\n").unwrap();

        let error = ModuleGraph::load(&src.join("main.jk"), &root).unwrap_err();
        assert!(error.to_string().contains("module dependency cycle"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graph_preserves_parse_diagnostics_with_source_context() {
        let root = std::env::temp_dir().join(format!(
            "joky-module-parse-diagnostic-{}",
            std::process::id()
        ));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let source = "fn main() {\n    .broken()\n}\n";
        std::fs::write(src.join("main.jk"), source).unwrap();

        let error = ModuleGraph::load(&src.join("main.jk"), &root).unwrap_err();
        match error {
            ModuleLoadError::Diagnostic {
                path,
                source: actual,
                diagnostic,
            } => {
                assert_eq!(path, src.join("main.jk").canonicalize().unwrap());
                assert_eq!(actual, source);
                assert_eq!(diagnostic.stage(), crate::Stage::Parse);
                assert!(diagnostic.span().is_some());
            }
            ModuleLoadError::Message(message) => {
                panic!("expected a parse diagnostic, got: {message}");
            }
        }

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn module_identity_uses_package_relative_path() {
        let suffix = std::process::id();
        let first = std::env::temp_dir().join(format!("joky-id-a-{suffix}"));
        let second = std::env::temp_dir().join(format!("joky-id-b-{suffix}"));
        for root in [&first, &second] {
            let src = root.join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join("main.jk"), "fn main() {}\n").unwrap();
        }
        let left = ModuleGraph::load(&first.join("src/main.jk"), &first).unwrap();
        let right = ModuleGraph::load(&second.join("src/main.jk"), &second).unwrap();
        assert_eq!(
            left.module(left.entry_id(&first.join("src/main.jk")).unwrap())
                .stable_id,
            right
                .module(right.entry_id(&second.join("src/main.jk")).unwrap())
                .stable_id
        );
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn compilation_order_places_dependencies_before_importers() {
        let root = std::env::temp_dir().join(format!("joky-order-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.jk"), "import util\nfn main() {}\n").unwrap();
        std::fs::write(src.join("util.jk"), "fn util() {}\n").unwrap();
        let graph = ModuleGraph::load(&src.join("main.jk"), &root).unwrap();
        let order = graph.compilation_order().unwrap();
        let entry = graph.entry_id(&src.join("main.jk")).unwrap();
        let dependency = graph.module(entry).imports[0].1;
        assert!(
            order.iter().position(|id| *id == dependency)
                < order.iter().position(|id| *id == entry)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dependency_metadata_check_reports_missing_and_incompatible_abis() {
        let root = std::env::temp_dir().join(format!("joky-abi-check-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.jk"), "import util\nfn main() {}\n").unwrap();
        std::fs::write(src.join("util.jk"), "pub fn util() {}\n").unwrap();
        let graph = ModuleGraph::load(&src.join("main.jk"), &root).unwrap();
        let entry = graph.entry_id(&src.join("main.jk")).unwrap();
        let dependency = graph.module(entry).imports[0].1;
        let expected = graph.metadata(dependency);
        let empty = HashMap::new();
        assert!(graph.check_dependency_metadata(entry, &empty).is_err());
        let mut available = HashMap::new();
        available.insert(expected.stable_id, expected.clone());
        assert!(graph.check_dependency_metadata(entry, &available).is_ok());
        let mut changed = expected;
        changed.abi_hash += 1;
        available.insert(changed.stable_id, changed);
        let error = graph
            .check_dependency_metadata(entry, &available)
            .unwrap_err();
        assert!(error.contains("public ABI changed"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graph_provides_cache_keys_and_metadata_map_for_each_module() {
        let root = std::env::temp_dir().join(format!("joky-cache-key-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.jk"), "import util\nfn main() {}\n").unwrap();
        std::fs::write(src.join("util.jk"), "fn util() {}\n").unwrap();
        let graph = ModuleGraph::load(&src.join("main.jk"), &root).unwrap();
        let entry = graph.entry_id(&src.join("main.jk")).unwrap();
        let key = graph
            .cache_key(entry, "runtime", "target", 64, "jit")
            .unwrap();
        assert_ne!(key.source_hash, 0);
        assert_eq!(graph.metadata_map().len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_units_are_dependency_first_and_self_contained() {
        let root = std::env::temp_dir().join(format!("joky-source-units-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.jk"), "import util\nfn main() {}\n").unwrap();
        std::fs::write(src.join("util.jk"), "fn util() {}\n").unwrap();
        let graph = ModuleGraph::load(&src.join("main.jk"), &root).unwrap();
        let units = graph.source_units("runtime", "target", 64, "jit").unwrap();
        assert_eq!(units.len(), 2);
        assert!(units[0].source.contains("fn util"));
        assert_ne!(
            units[0].cache_key.source_hash,
            units[1].cache_key.source_hash
        );
        assert!(graph.verify_source_units(&units).is_ok());
        let mut tampered = units.clone();
        tampered[0].source.push_str("\nfn changed() {}\n");
        assert!(graph.verify_source_units(&tampered).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn abi_hash_is_independent_of_export_insertion_order() {
        let mut first = HashMap::new();
        first.insert("zeta".to_string(), Visibility::Private);
        first.insert("alpha".to_string(), Visibility::Public);
        let mut second = HashMap::new();
        second.insert("alpha".to_string(), Visibility::Public);
        second.insert("zeta".to_string(), Visibility::Private);
        assert_eq!(module_abi_hash(&first), module_abi_hash(&second));
    }

    #[test]
    fn abi_hash_changes_when_public_surface_changes() {
        let mut exports = HashMap::new();
        exports.insert("run".to_string(), Visibility::Public);
        let original = module_abi_hash(&exports);
        exports.insert("debug".to_string(), Visibility::Public);
        assert_ne!(original, module_abi_hash(&exports));
    }

    #[test]
    fn abi_hash_changes_when_public_signature_changes() {
        let mut exports = HashMap::new();
        exports.insert("run".to_string(), Visibility::Public);
        let mut signatures = HashMap::new();
        signatures.insert("run".to_string(), "fn(Int32)->Unit!{}".to_string());
        let original = module_abi_hash_with_signatures(&exports, &signatures);
        signatures.insert("run".to_string(), "fn(String)->Unit!{}".to_string());
        assert_ne!(
            original,
            module_abi_hash_with_signatures(&exports, &signatures)
        );
    }

    #[test]
    fn private_exports_do_not_change_abi_hash() {
        let mut exports = HashMap::new();
        exports.insert("run".to_string(), Visibility::Public);
        let original = module_abi_hash(&exports);
        exports.insert("helper".to_string(), Visibility::Private);
        assert_eq!(original, module_abi_hash(&exports));
    }

    #[test]
    fn metadata_encoding_is_deterministic_and_compatibility_is_strict() {
        let info = ModuleInfo {
            resources: Default::default(),
            id: ModuleId(0),
            path: PathBuf::from("src/main.jk"),
            imports: Vec::new(),
            exports: [
                ("z".into(), Visibility::Private),
                ("a".into(), Visibility::Public),
            ]
            .into_iter()
            .collect(),
            stable_id: StableId(7),
            abi_hash: 9,
            export_signatures: [("a".into(), "fn(Int32)->Unit!{}".into())]
                .into_iter()
                .collect(),
        };
        let metadata = ModuleMetadata::from_info(&info);
        assert_eq!(
            metadata.encode(),
            "joky-module-abi 46\nmodule 0000000000000007\nabi 0000000000000009\nexport pub a\nsignature a fn(Int32)->Unit!{}\n"
        );
        assert_eq!(
            ModuleMetadata::decode(&metadata.encode()).unwrap(),
            metadata
        );
        assert!(metadata.is_compatible_with(&metadata));
        let mut changed = metadata.clone();
        changed.abi_hash += 1;
        assert!(!metadata.is_compatible_with(&changed));
        assert_eq!(
            metadata.compatibility_issue(&changed).as_deref(),
            Some("public ABI changed (000000000000000a -> 0000000000000009)")
        );
    }

    #[test]
    fn cache_fingerprint_includes_dependencies_and_target_context() {
        let key = ModuleCacheKey {
            resource_hashes: Vec::new(),
            module_identity: StableId(1),
            source_hash: 1,
            dependency_abi_hashes: vec![2, 3],
            compiler_abi_version: 1,
            runtime_version: "runtime-1".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            pointer_width: 64,
            features: "jit".into(),
        };
        let mut changed = key.clone();
        changed.dependency_abi_hashes.reverse();
        assert_ne!(key.fingerprint(), changed.fingerprint());
        changed = key.clone();
        changed.pointer_width = 32;
        assert_ne!(key.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn aot_object_fingerprint_includes_profile_and_unit_set() {
        let key = AotObjectCacheKey {
            runtime_version: "runtime-1".into(),
            compiler_abi_version: 1,
            target: "x86_64-unknown-linux-gnu".into(),
            pointer_width: 64,
            release: false,
            debug_info: false,
            unit_fingerprints: vec![1, 2],
        };
        let mut changed = key.clone();
        changed.release = true;
        assert_ne!(key.fingerprint(), changed.fingerprint());
        changed = key.clone();
        changed.debug_info = true;
        assert_ne!(key.fingerprint(), changed.fingerprint());
        changed = key.clone();
        changed.unit_fingerprints.reverse();
        assert_ne!(key.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn cache_constructor_normalizes_dependency_order() {
        let a = ModuleMetadata {
            format_version: 1,
            stable_id: StableId(1),
            abi_hash: 11,
            exports: Vec::new(),
            dependencies: Vec::new(),
            signatures: Vec::new(),
            runtime_initializers: Vec::new(),
            drop_glue: Vec::new(),
        };
        let b = ModuleMetadata {
            format_version: 1,
            stable_id: StableId(2),
            abi_hash: 22,
            exports: Vec::new(),
            dependencies: Vec::new(),
            signatures: Vec::new(),
            runtime_initializers: Vec::new(),
            drop_glue: Vec::new(),
        };
        let left = ModuleCacheKey::new(b"source", &[a.clone(), b.clone()], "r", "target", 64, "");
        let right = ModuleCacheKey::new(b"source", &[b, a], "r", "target", 64, "");
        assert_eq!(left.fingerprint(), right.fingerprint());
        assert_eq!(left.source_hash, source_fingerprint(b"source"));
    }

    #[test]
    fn module_cache_roundtrips_and_invalidates_by_key() {
        let root = std::env::temp_dir().join(format!("joky-module-cache-{}", std::process::id()));
        let cache = ModuleCache::new(&root);
        let key = ModuleCacheKey::new(b"source", &[], "r", "target", 64, "");
        let metadata = ModuleMetadata {
            format_version: 1,
            stable_id: StableId(42),
            abi_hash: 99,
            exports: vec![("run".into(), Visibility::Public)],
            dependencies: Vec::new(),
            signatures: vec![("run".into(), "fn()->Unit!{}".into())],
            runtime_initializers: vec!["runtime.init".into()],
            drop_glue: vec!["drop.run".into()],
        };
        assert!(cache.load(&key).unwrap().is_none());
        cache.store(&key, &metadata).unwrap();
        assert_eq!(cache.load(&key).unwrap(), Some(metadata));
        let expected = cache.load(&key).unwrap().unwrap();
        assert!(cache.load_compatible(&key, &expected).unwrap().is_some());
        let mut incompatible = expected.clone();
        incompatible.abi_hash += 1;
        assert!(cache.load_compatible(&key, &incompatible).is_err());
        let changed = ModuleCacheKey::new(b"changed", &[], "r", "target", 64, "");
        assert!(cache.load(&changed).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
