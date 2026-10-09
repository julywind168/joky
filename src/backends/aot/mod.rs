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
    module: ObjectModule,
    exports: HashMap<String, FunctionInfo>,
    requires_runtime: bool,
    state: CompilationState,
    symbol_prefix: String,
    types: TypeRegistry,
    registry: Rc<ModuleRegistry>,
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
        let imports = self
            .registry
            .import_layouts(ast, imports)
            .map_err(|error| error.with_sources(ast.sources()))?;
        let plan = prepared.plan(&imports)?;
        let compiled = native::compile(
            &mut self.module,
            &plan,
            &self.symbol_prefix,
            prepared.bindings(),
        )
        .map_err(|error| error.with_sources(ast.sources()))?;
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
