//! AOT object emission using the shared compiler pipeline.
use crate::{
    compiler, modules::ModuleRegistry, parser::ast::Module, Error, ErrorCode, ErrorPhase,
    TypeRegistry,
};
use compiler::ir::FunctionInfo;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::default_libcall_names;
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::{collections::HashMap, path::Path, rc::Rc};

pub struct AotCompiler {
    module: ObjectModule,
    exports: HashMap<String, FunctionInfo>,
    state: CompilationState,
    symbol_prefix: String,
    types: TypeRegistry,
    registry: Rc<ModuleRegistry>,
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
            state: CompilationState::New,
            symbol_prefix: prefix.into(),
            types: registry.types().clone(),
            registry,
        })
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
        let ast = crate::resolver::resolve(ast, self.registry.as_ref())?;
        let imports = self.registry.import_layouts(&ast, imports)?;
        self.exports = compiler::compile_with_bindings(
            &mut self.module,
            &ast,
            &imports,
            &self.symbol_prefix,
            self.registry.metadata(),
            &self.types,
        )?
        .into_iter()
        .map(|(name, (_, info))| (name, info))
        .collect();
        self.state = CompilationState::Compiled;
        let mut names: Vec<_> = self.exports.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    pub fn cps_frame_slots(&self, name: &str) -> Option<usize> {
        self.exports
            .get(name)
            .filter(|f| f.suspends)
            .map(|f| f.frame_slots)
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
mod tests;
