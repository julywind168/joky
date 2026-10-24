//! AOT object emission using the shared compiler pipeline.
use crate::core::ir::FunctionInfo;
use crate::{
    backends::native, modules::ModuleRegistry, parser::ast::Module, Error, ErrorCode, ErrorPhase,
    TypeRegistry,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::default_libcall_names;
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::{collections::HashMap, path::Path, rc::Rc};

pub struct AotCompiler {
    parallel: crate::core::ir::parallel::Effects,
    parallel_hosts: HashMap<String, (crate::FunctionSignature, usize)>,
    sources: std::sync::Arc<crate::SourceMap>,
    module: ObjectModule,
    exports: HashMap<String, FunctionInfo>,
    requires_runtime: bool,
    state: CompilationState,
    symbol_prefix: String,
    types: TypeRegistry,
    registry: Rc<ModuleRegistry>,
}

/// Immutable compiler evidence, separate from the loaded native code owner.
#[derive(Clone)]
pub struct ParallelEntry {
    symbol: String,
    name: String,
    info: FunctionInfo,
    sources: std::sync::Arc<crate::SourceMap>,
}
impl ParallelEntry {
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
    /// Render a standalone main only after compiler qualification of the full
    /// call graph. The descriptor contains link-time symbols, not host addresses.
    pub fn standalone_main_v1(&self, workers: usize, max_tasks: usize) -> Result<String, Error> {
        crate::runtime::parallel::ExecutionMode::Parallel { workers }.validate()?;
        if max_tasks == 0
            || !self.info.signature.params.is_empty()
            || !matches!(
                self.info.signature.ret,
                crate::Type::Unit | crate::Type::Int32
            )
        {
            return Err(compile_error(
                "Standalone parallel main requires () -> Unit or Int32",
            ));
        }
        let header = include_str!("../../../include/joky_executor.h");
        let symbol = &self.symbol;
        let slots = self.info.frame_slots;
        let kind = u32::from(self.info.signature.ret == crate::Type::Int32);
        Ok(format!("{header}
extern int32_t {symbol}(JokyCpsFrame *);
extern void joky_runtime_error_clear(void);
extern void joky_runtime_print_error(void);
int main(void) {{
    const JokyExecutionConfigV1 config = {{1, sizeof(JokyExecutionConfigV1), 1, {workers}, {max_tasks}}};
    const JokyTaskMainV1 entry = {{1, sizeof(JokyTaskMainV1), {symbol}, {slots}, {kind}, 0}};
    uint64_t value = 0;
    joky_runtime_error_clear();
    if (__joky_task_main_v1(&config, &entry, &value) != 0) {{ joky_runtime_print_error(); return 1; }}
    return {kind} ? (uint8_t)value : 0;
}}"))
    }

    /// Bind the exact emitted CPS symbol and retain its executable code.
    ///
    /// # Safety
    /// `entry` must be this descriptor's exact generated symbol, linked against
    /// this runtime ABI. `owner` must retain all executable pages and dependencies
    /// until the last task and cleanup finish, and support destruction on any
    /// thread. This does not validate an arbitrary native pointer or its library.
    pub unsafe fn prepare(
        &self,
        entry: crate::runtime::cps::CpsEntry,
        owner: std::sync::Arc<dyn std::any::Any + Send + Sync>,
        inputs: Vec<crate::vm::TaskValue>,
        budgets: Vec<crate::runtime::parallel::TaskBudget>,
    ) -> Result<crate::vm::PreparedParallelTask, Error> {
        let code =
            super::jit::FrozenEntry::aot(entry, self.info.clone(), self.sources.clone(), owner);
        crate::vm::PreparedParallelTask::native(
            code,
            &self.name,
            inputs,
            budgets,
            Default::default(),
        )
    }
}

/// Compiler-authored logical name, physical symbol, and complete C prototype.
#[derive(Clone, Debug)]
pub struct TextExportV2 {
    pub name: String,
    pub symbol: String,
    pub prototype: String,
    pub frame_slots: Option<usize>,
}

enum CompilationState {
    New,
    Failed,
    Compiled,
}

impl AotCompiler {
    pub fn new(name: &str) -> Result<Self, Error> {
        Self::with_symbol_prefix(name, "")
    }

    /// Prefix source-defined and ordinary extern symbols when linking into a host.
    /// Explicit registry symbols and runtime helpers retain their native names.
    pub fn with_symbol_prefix(name: &str, prefix: &str) -> Result<Self, Error> {
        Self::with_symbol_prefix_and_registry(name, prefix, crate::stdlib::module_registry())
    }

    /// Inject the same declaration/symbol snapshot used by a VM or module loader.
    pub(crate) fn with_symbol_prefix_and_registry(
        name: &str,
        prefix: &str,
        registry: Rc<ModuleRegistry>,
    ) -> Result<Self, Error> {
        let mut flags = settings::builder();
        flags
            .set("is_pic", "true")
            .map_err(|e| compile_error(e.to_string()))?;
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| compile_error(e.to_string()))?;
        let isa = cranelift_native::builder()
            .map_err(|e| compile_error(e.to_string()))?
            .finish(settings::Flags::new(flags))
            .map_err(|e| compile_error(e.to_string()))?;
        let builder = ObjectBuilder::new(isa, name, default_libcall_names())
            .map_err(|e| compile_error(e.to_string()))?;
        Ok(Self {
            parallel: Default::default(),
            parallel_hosts: Default::default(),
            sources: Default::default(),
            module: ObjectModule::new(builder),
            exports: HashMap::new(),
            requires_runtime: false,
            state: CompilationState::New,
            symbol_prefix: prefix.into(),
            types: registry.types().clone(),
            registry,
        })
    }

    /// Configure implicit imports before compiling a source module.
    pub fn set_prelude_enabled(&mut self, enabled: bool) -> Result<(), Error> {
        if !matches!(self.state, CompilationState::New) {
            return Err(Error::new_with_code(
                ErrorPhase::Compile,
                ErrorCode::InvalidState,
                "Cannot change the prelude after compilation has started",
            ));
        }
        self.types.set_prelude_enabled(enabled);
        Ok(())
    }

    pub fn prelude_enabled(&self) -> bool {
        self.types.prelude_enabled()
    }

    /// Register an opaque host type used by an `extern fn` declaration.
    /// Registration must happen before compiling the module.
    pub fn register_opaque_type(&mut self, name: impl Into<String>) -> Result<(), Error> {
        if !matches!(self.state, CompilationState::New) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::InvalidState,
                "Cannot register a type after compilation has started",
            ));
        }
        self.types.register_opaque(name)
    }

    /// Register a non-Copy external resource before compilation.
    pub fn register_resource_type(
        &mut self,
        name: impl Into<String>,
    ) -> Result<crate::Type, Error> {
        if !matches!(self.state, CompilationState::New) {
            return Err(compile_error(
                "Cannot register a resource after compilation",
            ));
        }
        self.types.register_resource(name)
    }
    /// Select a UInt64 -> resource factory. Link ResourceFactoryV1 at
    /// __joky_resource_factory_v1_<prefix><name>; no host addresses enter objects.
    pub fn register_resource_factory(&mut self, name: &str, resource: &str) -> Result<(), Error> {
        if !matches!(self.state, CompilationState::New) {
            return Err(compile_error("Cannot register a factory after compilation"));
        }
        crate::runtime::resource::validate_name(name)?;
        crate::runtime::tcp::validate_external_resource(resource)?;
        let result = self.types.resource_type(resource)?;
        self.types.resource_async.remove(name);
        self.types.resource_io.remove(name);
        self.types.resource_factories.insert(
            name.into(),
            crate::FunctionSignature::new(vec![crate::Type::UInt64], result),
        );
        Ok(())
    }

    /// Select a suspending UInt64 -> resource factory. Link ResourceFactoryCpsV1
    /// at __joky_resource_factory_cps_v1_<prefix><name>.
    pub fn register_resource_factory_async(
        &mut self,
        name: &str,
        resource: &str,
    ) -> Result<(), Error> {
        self.register_resource_factory(name, resource)?;
        self.types.resource_async.insert(name.into());
        Ok(())
    }

    /// Select the bounded resource I/O ABI before compilation. Link ResourceIoV1
    /// at __joky_resource_io_v1_<prefix><name>.
    pub fn register_resource_io(
        &mut self,
        name: &str,
        resource: &str,
        kind: crate::runtime::byte_request::ByteIoKind,
    ) -> Result<(), Error> {
        if !matches!(self.state, CompilationState::New) {
            return Err(compile_error(
                "Cannot register resource I/O after compilation",
            ));
        }
        crate::runtime::resource::validate_name(name)?;
        crate::runtime::tcp::validate_external_resource(resource)?;
        let signature = self.types.resource_io_signature(resource, kind)?;
        self.types.resource_factories.insert(name.into(), signature);
        self.types.resource_async.insert(name.into());
        self.types.resource_io.insert(name.into(), kind);
        Ok(())
    }

    /// Select the synchronous text FFI v1 descriptor ABI for an extern fn.
    /// Link a const TextFfiHostV1 as __joky_text_ffi_v1_<prefix><name>.
    /// Only symbolic metadata enters the object; descriptors are validated at run time.
    pub fn register_text_ffi_fn(
        &mut self,
        name: &str,
        signature: crate::FunctionSignature,
    ) -> Result<(), Error> {
        if !matches!(self.state, CompilationState::New) {
            return Err(compile_error(
                "Cannot register text FFI after compilation has started",
            ));
        }
        crate::core::ffi::validate_text_signature(&signature)?;
        if name.starts_with("__joky_")
            || name.is_empty()
            || !name
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
        {
            return Err(compile_error("Invalid text FFI name"));
        }
        self.types.text_ffi.insert(name.into(), signature);
        Ok(())
    }

    /// C declarations for managed factories and bounded byte operations.
    pub fn resource_header_v1(&self) -> String {
        let mut names: Vec<_> = self.types.resource_factories.keys().collect();
        names.sort();
        let mut header = String::from(
            "#include \"joky_resource.h\"\n#ifdef __cplusplus\nextern \"C\" {\n#endif\n",
        );
        for name in names {
            let symbol = format!("{}{name}", self.symbol_prefix);
            let (ty, symbol) = if self.types.resource_io.contains_key(name) {
                (
                    "JokyResourceIoV1",
                    crate::runtime::byte_request::symbol(&symbol),
                )
            } else if self.types.resource_async.contains(name) {
                (
                    "JokyResourceFactoryCpsV1",
                    crate::runtime::resource::cps_symbol(&symbol),
                )
            } else {
                (
                    "JokyResourceFactoryV1",
                    crate::runtime::resource::symbol(&symbol),
                )
            };
            header.push_str(&format!("extern const {ty} {symbol};\n"));
        }
        header.push_str("#ifdef __cplusplus\n}\n#endif\n");
        header
    }

    /// C descriptor declarations for the registered synchronous adapters.
    /// These describe the adapter ABI, never the wrapped foreign C prototypes.
    pub fn text_ffi_header_v1(&self) -> String {
        let mut names: Vec<_> = self.types.text_ffi.keys().collect();
        names.sort();
        let mut header = String::from(
            "#include \"joky_text_ffi.h\"\n#ifdef __cplusplus\nextern \"C\" {\n#endif\n",
        );
        for name in names {
            header.push_str(&format!(
                "extern const JokyTextFfiHostV1 {};\n",
                crate::runtime::text_ffi::symbol(&format!("{}{name}", self.symbol_prefix))
            ));
        }
        header.push_str("#ifdef __cplusplus\n}\n#endif\n");
        header
    }

    /// Declare an explicitly audited scalar CPS import before compilation.
    ///
    /// # Safety
    /// The final linker must bind this exact name/signature/slot count to code
    /// safe for simultaneous calls on arbitrary workers. Progress may use only
    /// declared scalar slots, with no retained waker, local context or stop hook.
    /// The owner passed to ParallelEntry::prepare must retain every dependency.
    pub unsafe fn declare_parallel_host(
        &mut self,
        name: &str,
        signature: crate::FunctionSignature,
        slots: usize,
    ) -> Result<(), Error> {
        if !matches!(self.state, CompilationState::New)
            || !signature
                .params
                .iter()
                .chain(std::iter::once(&signature.ret))
                .all(crate::core::instance::scalar)
            || slots < signature.params.len()
        {
            return Err(compile_error("Invalid parallel CPS import declaration"));
        }
        self.parallel_hosts.insert(name.into(), (signature, slots));
        Ok(())
    }

    /// Compile one source module. Built-in imports are resolved here.
    /// Use a fresh compiler for each object.
    pub fn compile_module(&mut self, ast: &Module) -> Result<Vec<String>, Error> {
        self.compile_with_import_layouts(ast, &HashMap::new())
    }

    /// Generated CPS imports require their complete frame sizes, not just parameters.
    pub fn compile_with_import_layouts(
        &mut self,
        ast: &Module,
        imports: &HashMap<String, usize>,
    ) -> Result<Vec<String>, Error> {
        if !matches!(self.state, CompilationState::New) {
            return Err(Error::new_with_code(
                ErrorPhase::Compile,
                ErrorCode::InvalidState,
                "An AOT compiler accepts one module; create a new compiler",
            ));
        }
        self.state = CompilationState::Failed;
        let prepared =
            crate::core::prepared::PreparedModule::new(ast, &self.types, self.registry.metadata())?;
        let ast = prepared.ast();
        let mut imports = imports.clone();
        for (name, (_, slots)) in &self.parallel_hosts {
            imports.insert(name.clone(), *slots);
        }
        let imports = self
            .registry
            .import_layouts(ast, &imports)
            .map_err(|error| error.with_sources(ast.sources()))?;
        let plan = prepared.plan(&imports)?;
        let compiled = native::compile(
            &mut self.module,
            &plan,
            &self.symbol_prefix,
            prepared.bindings(),
        )
        .map_err(|error| error.with_sources(ast.sources()))?;
        for (name, (signature, slots)) in &self.parallel_hosts {
            let info = plan
                .functions
                .get(name)
                .ok_or_else(|| compile_error("Unknown parallel host declaration"))?;
            if plan.bodies.contains_key(name)
                || !info.suspends
                || info.signature != *signature
                || info.frame_slots != *slots
            {
                return Err(compile_error(
                    "Parallel host declaration differs from compiled import",
                ));
            }
        }
        let mut safe_hosts = self
            .parallel_hosts
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        for name in plan.functions.keys() {
            if self
                .registry
                .binding(name)
                .and_then(|b| b.host())
                .as_ref()
                .and_then(crate::runtime::parallel::Host::from_binding)
                .is_some()
            {
                safe_hosts.insert(name.clone());
            }
        }
        self.parallel = crate::core::ir::parallel::analyse(&plan, &prepared, &safe_hosts);
        self.sources = ast.sources.clone();
        self.requires_runtime = compiled.requires_runtime;
        self.exports = compiled
            .exports
            .into_iter()
            .map(|(name, (_, info))| (name, info))
            .collect();
        self.state = CompilationState::Compiled;
        let mut names: Vec<_> = self.exports.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    pub fn function_entry_abi(&self, name: &str) -> Option<crate::EntryAbi> {
        self.exports.get(name).map(|f| {
            if f.signature.has_text() {
                crate::EntryAbi::OwnedTextV2
            } else {
                crate::EntryAbi::Scalar
            }
        })
    }

    /// Owning text roots use versioned symbols and compiler-generated prototypes.
    pub fn text_exports_v2(&self) -> Vec<TextExportV2> {
        fn c_type(ty: &crate::Type) -> &'static str {
            match ty {
                crate::Type::Int32 => "int32_t",
                crate::Type::Int64 => "int64_t",
                crate::Type::UInt8 | crate::Type::Bool => "uint8_t",
                crate::Type::UInt32 | crate::Type::Rune => "uint32_t",
                crate::Type::UInt64 | crate::Type::Named(_) => "uint64_t",
                crate::Type::String => "JokyOwnedTextV2",
                _ => unreachable!("checked scalar text export"),
            }
        }
        let mut exports: Vec<_> = self
            .exports
            .iter()
            .filter(|(_, f)| f.signature.has_text())
            .map(|(name, info)| {
                let symbol = format!("__joky_user_text_v2_{}{name}", self.symbol_prefix);
                let params = if info.suspends {
                    vec!["JokyCpsFrame *frame".to_owned()]
                } else {
                    let mut params = Vec::new();
                    for (index, ty) in info.signature.params.iter().enumerate() {
                        match ty {
                            crate::Type::String => {
                                params.push(format!("const uint8_t *arg{index}_data"));
                                params.push(format!("uint64_t arg{index}_byte_length"));
                            }
                            crate::Type::Unit => {}
                            ty => params.push(format!("{} arg{index}", c_type(ty))),
                        }
                    }
                    if info.signature.ret != crate::Type::Unit {
                        params.push(format!("{} *output", c_type(&info.signature.ret)));
                    }
                    params
                };
                TextExportV2 {
                    name: name.clone(),
                    symbol: symbol.clone(),
                    prototype: format!(
                        "int32_t {symbol}({});",
                        if params.is_empty() {
                            "void".into()
                        } else {
                            params.join(", ")
                        }
                    ),
                    frame_slots: info.suspends.then_some(info.frame_slots),
                }
            })
            .collect();
        exports.sort_by(|a, b| a.name.cmp(&b.name));
        exports
    }

    /// Emit prototypes rather than requiring hosts to infer a text ABI from names.
    pub fn text_header_v2(&self) -> String {
        let mut header = String::from("#pragma once\n#include <stdint.h>\n#include \"joky_vm.h\"\n#include \"joky_cps.h\"\n#ifdef __cplusplus\nextern \"C\" {\n#endif\n");
        for export in self.text_exports_v2() {
            header.push_str(&export.prototype);
            header.push('\n');
        }
        header.push_str("#ifdef __cplusplus\n}\n#endif\n");
        header
    }

    /// Compiler evidence for a pure generated CPS entry. Externs require an
    /// explicit runtime binding contract; an import slot count is not evidence.
    pub fn parallel_entry(&self, name: &str) -> Result<ParallelEntry, Error> {
        self.parallel.require(name)?;
        let info = self
            .exports
            .get(name)
            .filter(|info| info.suspends)
            .ok_or_else(|| {
                compile_error("Parallel entry must be an exported @suspends function")
            })?;
        Ok(ParallelEntry {
            symbol: if info.signature.has_text() {
                format!("__joky_user_text_v2_{}{name}", self.symbol_prefix)
            } else {
                format!("{}{name}", self.symbol_prefix)
            },
            name: name.into(),
            info: info.clone(),
            sources: self.sources.clone(),
        })
    }

    pub fn cps_frame_slots(&self, name: &str) -> Option<usize> {
        self.exports
            .get(name)
            .filter(|f| f.suspends)
            .map(|f| f.frame_slots)
    }

    /// Whether this module uses CPS or native error helpers from libjoky.
    /// Query after compilation and link the same runtime used by the caller.
    /// Covers private definitions too, because their machine code is emitted
    /// even when no export reaches them.
    pub fn requires_runtime(&self) -> bool {
        self.requires_runtime
    }

    pub fn write_object(self, path: &Path) -> Result<(), Error> {
        if !matches!(self.state, CompilationState::Compiled) {
            return Err(Error::new_with_code(
                ErrorPhase::Compile,
                ErrorCode::InvalidState,
                "Cannot emit an object before successful compilation",
            ));
        }
        let bytes = self
            .module
            .finish()
            .emit()
            .map_err(|e| compile_error(e.to_string()))?;
        std::fs::write(path, bytes).map_err(|e| io_error(e.to_string()))
    }
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}

fn io_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::IoError, message)
}

#[cfg(test)]
pub(crate) mod tests;
