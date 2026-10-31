//! Immutable front-end inputs shared by VM and object compilation.
use super::{
    ir::ModulePlan,
    resolver::{self, BindingRegistry},
};
use crate::{parser::ast::Module, Error, TypeRegistry};
use std::collections::HashMap;

/// Standalone front-end checking with the standard host declarations.
/// No VM, executable code, runtime services, or entry point is created.
pub struct Checker {
    types: TypeRegistry,
}

impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
}

impl Checker {
    /// Create a checker with the same standard declarations as the CLI backends.
    pub fn new() -> Self {
        Self {
            types: crate::stdlib::module_registry().types().clone(),
        }
    }

    /// Configure implicit imports for subsequent source resolution.
    pub fn set_prelude_enabled(&mut self, enabled: bool) {
        self.types.set_prelude_enabled(enabled);
    }

    /// Resolve source files or in-memory modules with this checker's declarations.
    pub fn type_registry(&self) -> &TypeRegistry {
        &self.types
    }

    /// Check a resolved module, including unused functions and generic templates.
    /// This runs the shared semantic and ownership passes without code generation.
    /// Custom host registration and CLI entry-point policy are outside this API.
    pub fn check_module(&self, module: &Module) -> Result<(), Error> {
        let registry = crate::stdlib::module_registry();
        let prepared = PreparedModule::new(module, &self.types, registry.metadata())?;
        let imports = registry
            .import_layouts(prepared.ast(), &HashMap::new())
            .map_err(|error| error.with_sources(prepared.ast().sources()))?;
        prepared.plan(&imports).map(|_| ())
    }
}

/// Owns the normalized AST and declaration environment for one compilation.
/// Backend output retains sources and code owners, never this preparation state.
pub(crate) struct PreparedModule {
    ast: Module,
    types: TypeRegistry,
    bindings: BindingRegistry,
}

impl PreparedModule {
    pub(crate) fn new(
        module: &Module,
        types: &TypeRegistry,
        bindings: &BindingRegistry,
    ) -> Result<Self, Error> {
        let normalized = types.resolve_aliases(module)?;
        let ast = resolver::resolve(&normalized, bindings)
            .map_err(|error| error.with_sources(normalized.sources()))?;
        bindings
            .validate_declarations(&ast)
            .map_err(|error| error.with_sources(ast.sources()))?;
        Ok(Self {
            ast,
            types: types.clone(),
            bindings: bindings.clone(),
        })
    }

    pub(crate) fn ast(&self) -> &Module {
        &self.ast
    }

    pub(crate) fn types(&self) -> &TypeRegistry {
        &self.types
    }

    pub(crate) fn bindings(&self) -> &BindingRegistry {
        &self.bindings
    }

    /// Host binding must supply complete generated CPS layouts before planning.
    pub(crate) fn plan(&self, imports: &HashMap<String, usize>) -> Result<ModulePlan, Error> {
        ModulePlan::with_import_layouts_and_types(&self.ast, imports, &self.types)
            .map_err(|error| error.with_sources(self.ast.sources()))
    }
}

/// Check source without generating code. Execution and entry-point parity have
/// dedicated tests; static diagnostic and complexity-limit matrices use this path.
#[cfg(test)]
pub(crate) fn check_source(source: &str, modules: &crate::ModuleSources) -> Result<(), Error> {
    // Reuse parsed standard-library sources across cases, just as VM loading
    // does. Type resolution creates its own compilation scope for each source.
    thread_local! {
        static REGISTRY: crate::modules::ModuleRegistry = Default::default();
    }
    REGISTRY.with(|registry| {
        let ast = crate::parser::parse(source)?;
        let ast = registry.types().resolve_with_modules(&ast, modules)?;
        PreparedModule::new(&ast, registry.types(), registry.metadata())?
            .plan(&HashMap::new())
            .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backends::{interp, jit},
        parser::ast::Item,
        runtime::suspension::ModuleId,
        Type,
    };

    #[test]
    fn preparation_snapshot_is_independent_of_later_source_edits() {
        let mut ast =
            crate::parser::parse("type Number = Int32 fn value() -> Number { 42 }").unwrap();
        let types = TypeRegistry::default();
        let bindings = BindingRegistry::default();
        let prepared = PreparedModule::new(&ast, &types, &bindings).unwrap();
        assert!(!ast.type_aliases.is_empty());
        assert!(prepared.ast().type_aliases.is_empty());
        let Item::FnDef(function) = ast.items.last_mut().unwrap() else {
            panic!("expected function")
        };
        function.ret_ty = Type::Bool;
        // A new preparation validates the caller's current AST; the previous
        // snapshot still describes a valid Int32 function for both backends.
        let changed = PreparedModule::new(&ast, &types, &bindings).unwrap();
        assert!(changed.plan(&HashMap::new()).is_err());
        let interpreted =
            interp::compile(&prepared, &HashMap::new(), HashMap::new(), ModuleId(1)).unwrap();
        assert!(matches!(
            interp::call_sync(&interpreted, "value", &[], Default::default()).unwrap(),
            interp::Value::Int32(42)
        ));
        let native = jit::compile(&prepared, &[], &HashMap::new(), vec![], ModuleId(2)).unwrap();
        let value: unsafe extern "C" fn() -> i32 =
            unsafe { std::mem::transmute(native.exports["value"].entry) };
        assert_eq!(unsafe { value() }, 42);
    }
}
