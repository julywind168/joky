//! Ephemeral semantic facts consumed by AST/HIR lowering, never serialized.
use super::*;
use crate::sema::GenericCallInstance;
use crate::syntax::{Expr, NodeId};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosureCaptureBinding {
    pub(crate) name: String,
    pub(crate) declaration: Option<NodeId>,
    pub(crate) mutable: bool,
}

/// Checked source facts plus the persistent type table. MIR construction keeps
/// only `module.types`, so source-node maps cannot enter MIR or module caches.
#[derive(Debug, Default)]
pub(crate) struct CheckedTypes {
    pub(crate) prelude_functions: Vec<crate::syntax::Function>,
    pub(crate) module: ModuleTypes,
    expressions: HashMap<NodeId, Type>,
    struct_defaults: Vec<Vec<Option<Expr>>>,
    generic_calls: HashMap<NodeId, GenericCallInstance>,
    type_value_calls: HashMap<NodeId, Vec<Type>>,
    constants: HashMap<String, Expr>,
    constant_references: HashMap<NodeId, String>,
    function_declared_effects: HashMap<String, crate::sema::effects::EffectGroupSet>,
    function_effects: HashMap<String, crate::sema::effects::EffectSet>,
    handler_operations: HashMap<NodeId, crate::sema::effects::EffectOperationId>,
    effect_operations: HashMap<NodeId, crate::sema::effects::EffectOperationId>,
    closure_captures: HashMap<NodeId, Vec<(String, Type)>>,
    pub(in crate::sema) closure_capture_bindings: HashMap<NodeId, Vec<ClosureCaptureBinding>>,
    compile_time_bindings: HashSet<NodeId>,
    pub(crate) local_bindings: HashMap<NodeId, NodeId>,
    annotation_types: HashMap<crate::Span, Type>,
    external_symbols: HashMap<NodeId, SymbolId>,
    external_signatures: HashMap<SymbolId, ExternalFunction>,
    pub(crate) imported_constants: HashMap<NodeId, crate::module::constants::ConstantValue>,
    pub(crate) imported_templates: HashMap<String, SymbolId>,
    pub(crate) queried_type: Option<Type>,
    pub(crate) resolved_method_calls: HashMap<NodeId, crate::sema::methods::ResolvedMethodCall>,
    pub(crate) for_into_cursor: HashMap<NodeId, Type>,
}

impl Deref for CheckedTypes {
    type Target = ModuleTypes;
    fn deref(&self) -> &ModuleTypes {
        &self.module
    }
}

impl DerefMut for CheckedTypes {
    fn deref_mut(&mut self) -> &mut ModuleTypes {
        &mut self.module
    }
}

impl CheckedTypes {
    pub(crate) fn validate_c_layouts(&mut self) -> Result<(), String> {
        for (layout, defaults) in self.module.types.structs.iter().zip(&self.struct_defaults) {
            if layout.repr_c && defaults.iter().any(Option::is_some) {
                return Err(format!(
                    "@repr(c) struct '{}' cannot have field defaults",
                    layout.name
                ));
            }
        }
        self.module.types.validate_c_layouts()
    }

    /// Persist defaults without source NodeIds. Unsupported expressions keep the
    /// previous deferred import diagnostic; ordinary local defaults remain legal.
    pub(crate) fn prepare_struct_defaults(&mut self) {
        let defaults = self
            .struct_defaults
            .iter()
            .map(|fields| {
                fields
                    .iter()
                    .map(|default| {
                        default
                            .as_ref()
                            .filter(|expr| self.get_optional(expr).is_some())
                            .map(|expr| {
                                crate::module::constants::ConstantValue::from_expression(expr, self)
                                    .map_err(|error| error.to_string())
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        self.module.interface.struct_defaults = defaults;
    }

    /// Creates a new type table
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        expressions: HashMap<NodeId, Type>,
        tuples: Vec<Vec<Type>>,
        c_pointers: Vec<Type>,
        c_arrays: Vec<(Type, u64)>,
        structs: Vec<StructLayout>,
        struct_defaults: Vec<Vec<Option<Expr>>>,
        classes: Vec<ClassLayout>,
        enums: Vec<EnumLayout>,
        options: Vec<Type>,
        results: Vec<(Type, Type)>,
        lists: Vec<Type>,
        maps: Vec<MapInfo>,
        cowns: Vec<Type>,
        function_types: Vec<crate::sema::symbol_table::FunctionTypeInfo>,
        generic_calls: HashMap<NodeId, GenericCallInstance>,
        type_value_calls: HashMap<NodeId, Vec<Type>>,
        constants: HashMap<String, Expr>,
        constant_references: HashMap<NodeId, String>,
        show_impls: HashSet<Type>,
        associated_types: Vec<AssociatedTypeInfo>,
        trait_associated_impls: HashMap<(String, Type), HashMap<String, Type>>,
        effects: crate::sema::effects::EffectRegistry,
        function_declared_effects: HashMap<String, crate::sema::effects::EffectGroupSet>,
        function_effects: HashMap<String, crate::sema::effects::EffectSet>,
        handler_operations: HashMap<NodeId, crate::sema::effects::EffectOperationId>,
        effect_operations: HashMap<NodeId, crate::sema::effects::EffectOperationId>,
        closure_captures: HashMap<NodeId, Vec<(String, Type)>>,
        compile_time_bindings: HashSet<NodeId>,
        annotation_types: HashMap<crate::Span, Type>,
        external_symbols: HashMap<NodeId, SymbolId>,
        external_signatures: HashMap<SymbolId, ExternalFunction>,
    ) -> Self {
        Self {
            prelude_functions: Vec::new(),
            expressions,
            struct_defaults,
            generic_calls,
            type_value_calls,
            constants,
            constant_references,
            function_declared_effects,
            function_effects,
            handler_operations,
            effect_operations,
            closure_captures,
            closure_capture_bindings: HashMap::new(),
            local_bindings: HashMap::new(),
            compile_time_bindings,
            annotation_types,
            external_symbols,
            external_signatures,
            imported_constants: HashMap::new(),
            imported_templates: HashMap::new(),
            queried_type: None,
            resolved_method_calls: HashMap::new(),
            for_into_cursor: HashMap::new(),
            module: ModuleTypes {
                types: TypeTable {
                    tuples,
                    c_pointers,
                    c_arrays,
                    structs,
                    classes,
                    enums,
                    options,
                    results,
                    lists,
                    maps,
                    cowns,
                    function_types,
                    show_impls,
                    associated_types,
                    trait_associated_impls,
                    effects,
                    trait_implementations: HashMap::new(),
                    explicit_hash_impls: HashSet::new(),
                    dynamic_types: Vec::new(),
                },
                interface: ModuleInterface {
                    pending_functions: HashMap::new(),
                    public_abis: HashMap::new(),
                    public_constants: HashMap::new(),
                    public_templates: HashMap::new(),
                    generic_requests: HashMap::new(),
                    generic_symbols: HashMap::new(),
                    intrinsic_types: Vec::new(),
                    type_program: None,
                    module_imports: HashMap::new(),
                    module_exports: HashMap::new(),
                    standard_modules: HashSet::new(),
                    module_identity: None,
                    methods: Vec::new(),
                    method_symbols: HashMap::new(),
                    trait_definitions: HashMap::new(),
                    struct_defaults: Vec::new(),
                },
            },
        }
    }

    pub(crate) fn prepare_generic_symbols(&mut self, module: crate::module::StableId) {
        for (key, request) in &self.module.interface.generic_requests {
            let arguments = request
                .arguments
                .iter()
                .map(|ty| self.stable_type_key(*ty, module))
                .collect::<Vec<_>>();
            let identity = bincode::serialize(&(&request.definition, arguments))
                .expect("serializable generic identity");
            let symbol = SymbolId {
                module: crate::module::StableId(crate::module::source_fingerprint(&identity)),
                name: "__instance".into(),
            };
            self.module
                .interface
                .generic_symbols
                .insert(key.clone(), symbol.clone());
            self.external_signatures.insert(symbol, request.abi.clone());
        }
    }

    pub(crate) fn external_symbol(&self, id: NodeId) -> Option<&SymbolId> {
        self.external_symbols.get(&id)
    }

    pub(crate) fn external_symbols(&self) -> &HashMap<NodeId, SymbolId> {
        &self.external_symbols
    }

    pub(crate) fn external_signature(&self, symbol: &SymbolId) -> Option<&ExternalFunction> {
        self.external_signatures.get(symbol)
    }

    /// Gets the type of an expression
    ///
    /// # Panics
    /// Panics if the expression has no type info (an internal error)
    #[allow(dead_code)]
    pub(crate) fn get(&self, expression: &Expr) -> Type {
        *self
            .expressions
            .get(&expression.id)
            .expect("each checked expression has a type")
    }

    /// Gets the type of an expression; nodes used only for syntactic
    /// positioning such as call targets may not have their own type
    pub(crate) fn get_optional(&self, expression: &Expr) -> Option<Type> {
        self.expressions.get(&expression.id).copied()
    }

    pub(crate) fn function_effects(&self, name: &str) -> Option<&crate::sema::effects::EffectSet> {
        self.function_effects.get(name)
    }

    pub(crate) fn function_declared_effects(
        &self,
        name: &str,
    ) -> Option<&crate::sema::effects::EffectGroupSet> {
        self.function_declared_effects.get(name)
    }

    pub(crate) fn handler_operation(
        &self,
        id: NodeId,
    ) -> Option<crate::sema::effects::EffectOperationId> {
        self.handler_operations.get(&id).copied()
    }

    pub(crate) fn effect_operation(
        &self,
        id: NodeId,
    ) -> Option<crate::sema::effects::EffectOperationId> {
        self.effect_operations.get(&id).copied()
    }

    pub(crate) fn closure_captures(&self, id: NodeId) -> &[(String, Type)] {
        self.closure_captures
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn closure_capture_bindings(&self, id: NodeId) -> &[ClosureCaptureBinding] {
        self.closure_capture_bindings
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn is_compile_time_binding(&self, id: NodeId) -> bool {
        self.compile_time_bindings.contains(&id)
    }

    pub(crate) fn generic_call(&self, expression: &Expr) -> Option<&GenericCallInstance> {
        self.generic_calls.get(&expression.id)
    }

    pub(crate) fn type_value_call(&self, id: NodeId) -> Option<&[Type]> {
        self.type_value_calls.get(&id).map(Vec::as_slice)
    }

    pub(crate) fn generic_calls(&self) -> impl Iterator<Item = &GenericCallInstance> {
        self.generic_calls.values()
    }

    pub(crate) fn constant_value(&self, name: &str) -> Option<&Expr> {
        self.constants.get(name)
    }

    pub(crate) fn constant_reference(&self, id: NodeId) -> Option<&str> {
        self.constant_references.get(&id).map(String::as_str)
    }

    pub(crate) fn checked_annotation(
        &self,
        annotation: &crate::syntax::TypeAnnotation,
        substitutions: &[Type],
        receiver: Option<Type>,
    ) -> Option<Type> {
        self.instantiate(
            *self.annotation_types.get(&annotation.span)?,
            substitutions,
            receiver,
        )
    }

    pub(crate) fn struct_defaults(&self) -> &[Vec<Option<Expr>>] {
        &self.struct_defaults
    }

    pub(crate) fn cursor_type(&self, id: NodeId) -> Option<Type> {
        self.for_into_cursor.get(&id).copied()
    }
}
