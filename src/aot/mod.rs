//! AOT object emission using the shared compiler pipeline.
use crate::{compiler, parser::ast::Module};
use compiler::ir::FunctionInfo;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::default_libcall_names;
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::{collections::HashMap, path::Path};

pub struct AotCompiler {
    module: ObjectModule,
    exports: HashMap<String, FunctionInfo>,
    state: CompilationState,
    symbol_prefix: String,
}

enum CompilationState {
    New,
    Failed,
    Compiled,
}

impl AotCompiler {
    pub fn new(name: &str) -> Result<Self, String> {
        Self::with_symbol_prefix(name, "")
    }

    /// Prefix source-defined and imported symbols when linking into a host.
    /// Source names and diagnostics are unchanged; runtime helpers are not prefixed.
    pub fn with_symbol_prefix(name: &str, prefix: &str) -> Result<Self, String> {
        let mut flags = settings::builder();
        flags.set("is_pic", "true").map_err(|e| e.to_string())?;
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let builder =
            ObjectBuilder::new(isa, name, default_libcall_names()).map_err(|e| e.to_string())?;
        Ok(Self {
            module: ObjectModule::new(builder),
            exports: HashMap::new(),
            state: CompilationState::New,
            symbol_prefix: prefix.into(),
        })
    }

    /// Compile one source module. Use a fresh compiler for each object.
    pub fn compile_module(&mut self, ast: &Module) -> Result<Vec<String>, String> {
        self.compile_with_import_layouts(ast, &HashMap::new())
    }

    /// Generated CPS imports require their complete frame sizes, not just parameters.
    pub fn compile_with_import_layouts(
        &mut self,
        ast: &Module,
        imports: &HashMap<String, usize>,
    ) -> Result<Vec<String>, String> {
        if !matches!(self.state, CompilationState::New) {
            return Err("An AOT compiler accepts one module; create a new compiler".into());
        }
        self.state = CompilationState::Failed;
        self.exports = compiler::compile(&mut self.module, ast, imports, &self.symbol_prefix)?
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

    pub fn write_object(self, path: &Path) -> Result<(), String> {
        if !matches!(self.state, CompilationState::Compiled) {
            return Err("Cannot emit an object before successful compilation".into());
        }
        let bytes = self.module.finish().emit().map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests;
