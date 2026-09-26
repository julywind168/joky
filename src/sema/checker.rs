//! Type checker implementation

use std::collections::{HashMap, HashSet};

use crate::syntax::NodeId;

use crate::module::{StableId, SymbolId};

use super::pattern::PatternTypes;
use super::scope::Binding;
use super::symbol_table::{
    AssociatedTypeInfo, ClassInfo, EnumInfo, EnumVariantInfo, FunctionSignature, StructInfo,
    TraitInfo, TraitMethodInfo,
};
use super::types::Type;

#[derive(Clone, Copy)]
pub(super) struct ConstantInfo {
    pub(super) ty: Option<Type>,
    pub(super) span: crate::Span,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ConstantState {
    Unresolved,
    Resolving,
    Resolved,
}

/// The type checker
pub(super) struct Checker {
    pub(super) dynamic_types: Vec<super::dynamic::DynamicType>,
    /// Scope stack, each scope maps names to bindings
    pub(super) scopes: Vec<HashMap<String, Binding>>,
    pub(super) compile_time_bindings: HashSet<NodeId>,
    pub(super) local_bindings: HashMap<NodeId, NodeId>,
    /// Map from expression to type (keyed by NodeId)
    pub(super) types: HashMap<NodeId, Type>,
    pub(super) annotation_types: HashMap<crate::Span, Type>,
    /// Break type stack for loops
    pub(super) loop_break_types: Vec<Option<Type>>,
    pub(super) return_types: Vec<Type>,
    /// Function signature table
    pub(super) functions: HashMap<String, FunctionSignature>,
    pub(super) type_functions: HashMap<String, crate::syntax::Function>,
    pub(super) intrinsic_type_arities: HashMap<String, usize>,
    pub(super) intrinsic_parameter_names: HashMap<String, Vec<String>>,
    pub(super) intrinsic_methods: HashMap<String, HashMap<String, crate::syntax::IntrinsicMethod>>,
    pub(super) intrinsic_effect_methods: HashMap<(Type, String), super::EffectOperationId>,
    pub(super) constants: HashMap<String, ConstantInfo>,
    pub(super) constant_definitions: HashMap<String, crate::syntax::Constant>,
    pub(super) constant_states: HashMap<String, ConstantState>,
    pub(super) constant_references: HashMap<NodeId, String>,
    pub(super) imported_constants: HashMap<NodeId, crate::module::constants::ConstantValue>,
    pub(super) imported_templates: HashMap<String, SymbolId>,
    pub(super) generic_requests: Vec<(String, crate::module::generics::GenericRequest)>,
    pub(super) type_bindings: HashMap<String, Type>,
    pub(super) definition_module: Option<StableId>,
    pub(super) standard_modules: HashSet<StableId>,
    pub(super) type_query_depth: usize,
    pub(super) resolved_method_calls: HashMap<NodeId, super::methods::ResolvedMethodCall>,
    pub(super) method_symbols: HashMap<(Type, String), SymbolId>,
    pub(super) imported_method_abis: Vec<crate::module::generics::MethodAbi>,
    pub(super) next_import_node: u32,
    pub(super) current_constant: Option<String>,
    pub(super) current_module_prefix: Option<String>,
    pub(super) import_aliases: std::collections::HashMap<String, StableId>,
    pub(super) dependency_types: crate::module::DependencyTypes,
    pub(super) imported_types: HashMap<(StableId, Type), Type>,
    pub(super) dependency_exports:
        std::collections::HashMap<StableId, std::collections::HashMap<String, String>>,
    pub(super) external_symbols: std::collections::HashMap<crate::syntax::NodeId, SymbolId>,
    pub(super) external_signatures:
        std::collections::HashMap<SymbolId, super::type_table::ExternalFunction>,
    /// Functions declared `@extern` and callable as native C code.
    pub(super) foreign_functions: std::collections::HashSet<String>,
    pub(super) function_types: Vec<super::symbol_table::FunctionTypeInfo>,
    pub(super) current_type_parameters: Vec<String>,
    pub(super) current_type_parameter_bounds: Vec<Vec<String>>,
    pub(super) current_associated_bounds: Vec<(Type, Vec<String>)>,
    pub(super) for_into_cursor: HashMap<NodeId, Type>,
    pub(super) generic_calls: HashMap<crate::syntax::NodeId, super::GenericCallInstance>,
    pub(super) generic_call_spans: HashMap<crate::syntax::NodeId, crate::Span>,
    pub(super) type_value_calls: HashMap<crate::syntax::NodeId, Vec<Type>>,
    pub(super) tuple_types: Vec<Vec<Type>>,
    pub(super) c_pointers: Vec<Type>,
    pub(super) c_arrays: Vec<(Type, u64)>,
    pub(super) option_types: Vec<Type>,
    pub(super) result_types: Vec<(Type, Type)>,
    pub(super) list_types: Vec<Type>,
    pub(super) maps: Vec<super::types::MapInfo>,
    pub(super) cowns: Vec<Type>,
    pub(super) associated_types: Vec<AssociatedTypeInfo>,
    pub(super) current_trait_context: Option<String>,
    pub(super) structs: HashMap<String, StructInfo>,
    pub(super) generated_structs: HashMap<(String, NodeId, Vec<Type>), usize>,
    pub(super) generated_enums: HashMap<(String, NodeId, Vec<Type>), usize>,
    pub(super) classes: HashMap<String, ClassInfo>,
    pub(super) enums: HashMap<String, EnumInfo>,
    pub(super) show_impls: HashSet<Type>,
    pub(super) traits: HashMap<String, TraitInfo>,
    pub(super) trait_impls: HashMap<String, HashSet<Type>>,
    pub(super) trait_associated_impls: HashMap<(String, Type), HashMap<String, Type>>,
    pub(super) effects: super::effects::EffectRegistry,
    /// Source-level aliases that expand to one or more effect groups.
    pub(super) effect_aliases: HashMap<String, Vec<String>>,
    pub(super) current_effects: super::effects::EffectSet,
    pub(super) current_effect_groups: Vec<super::effects::EffectId>,
    pub(super) handler_operations: HashMap<NodeId, super::effects::EffectOperationId>,
    pub(super) effect_operations: HashMap<NodeId, super::effects::EffectOperationId>,
    pub(super) closure_captures: HashMap<NodeId, Vec<(String, Type)>>,
    pub(super) closure_capture_bindings: HashMap<NodeId, Vec<super::ClosureCaptureBinding>>,
    pub(super) borrowed_mutable_captures: HashSet<NodeId>,
    pub(super) when_depth: usize,
    pub(super) expr_depth: usize,
    pub(super) handler_abort_types: Vec<Type>,
}

impl Checker {
    /// Creates a new type checker
    pub(super) fn new() -> Self {
        let mut traits = HashMap::new();
        traits.insert(
            "Show".to_owned(),
            TraitInfo {
                associated_types: HashSet::new(),
                methods: HashMap::from([(
                    "show".to_owned(),
                    TraitMethodInfo {
                        effect_names: Vec::new(),
                        receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                        parameter_borrows: Vec::new(),
                        parameters: Vec::new(),
                        return_type: Type::String,
                    },
                )]),
            },
        );
        traits.insert(
            "Drop".to_owned(),
            TraitInfo {
                associated_types: HashSet::new(),
                methods: HashMap::from([(
                    "drop".to_owned(),
                    TraitMethodInfo {
                        effect_names: Vec::new(),
                        receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                        parameter_borrows: vec![],
                        parameters: vec![],
                        return_type: Type::Unit,
                    },
                )]),
            },
        );
        let mut debug_trait = traits["Show"].clone();
        let method = debug_trait
            .methods
            .remove("show")
            .expect("builtin Show method");
        debug_trait.methods.insert("debug".into(), method);
        traits.insert("Debug".into(), debug_trait);
        traits.insert(
            "Hash".to_owned(),
            TraitInfo {
                associated_types: HashSet::new(),
                methods: HashMap::from([(
                    "hash".into(),
                    TraitMethodInfo {
                        effect_names: vec![],
                        receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                        parameter_borrows: vec![true],
                        parameters: vec![("state".into(), Type::Hasher)],
                        return_type: Type::Unit,
                    },
                )]),
            },
        );
        traits.insert(
            "Eq".into(),
            TraitInfo {
                associated_types: HashSet::new(),
                methods: HashMap::new(),
            },
        );
        traits.insert(
            "PartialEq".into(),
            TraitInfo {
                associated_types: HashSet::new(),
                methods: HashMap::from([(
                    "equals".into(),
                    TraitMethodInfo {
                        effect_names: Vec::new(),
                        receiver_mode: crate::syntax::ReceiverMode::Borrowed,
                        parameter_borrows: vec![true],
                        parameters: vec![("other".into(), Type::SelfType)],
                        return_type: Type::Bool,
                    },
                )]),
            },
        );
        // Ordering has one canonical identity across source modules.
        let ordering = Type::Enum(0);
        let optional_ordering = Type::Option(0);
        for (name, method, result) in [
            ("PartialOrd", "partial_compare", optional_ordering),
            ("Ord", "compare", ordering),
        ] {
            let mut definition = traits["PartialEq"].clone();
            let mut signature = definition.methods.remove("equals").unwrap();
            signature.return_type = result;
            definition.methods.insert(method.into(), signature);
            traits.insert(name.into(), definition);
        }
        let mut checker = Self {
            scopes: vec![HashMap::new()],
            compile_time_bindings: HashSet::new(),
            local_bindings: HashMap::new(),
            types: HashMap::new(),
            annotation_types: HashMap::new(),
            loop_break_types: Vec::new(),
            return_types: Vec::new(),
            functions: HashMap::new(),
            type_functions: HashMap::new(),
            intrinsic_type_arities: HashMap::new(),
            intrinsic_parameter_names: HashMap::new(),
            intrinsic_methods: HashMap::new(),
            intrinsic_effect_methods: HashMap::new(),
            constants: HashMap::new(),
            constant_definitions: HashMap::new(),
            constant_states: HashMap::new(),
            constant_references: HashMap::new(),
            imported_constants: HashMap::new(),
            imported_templates: HashMap::new(),
            generic_requests: Vec::new(),
            type_bindings: HashMap::new(),
            definition_module: None,
            standard_modules: HashSet::new(),
            type_query_depth: 0,
            resolved_method_calls: HashMap::new(),
            method_symbols: HashMap::new(),
            imported_method_abis: Vec::new(),
            c_pointers: Vec::new(),
            c_arrays: Vec::new(),
            next_import_node: 0,
            current_constant: None,
            current_module_prefix: None,
            import_aliases: HashMap::new(),
            dependency_types: Default::default(),
            imported_types: HashMap::new(),
            dependency_exports: HashMap::new(),
            external_symbols: HashMap::new(),
            external_signatures: HashMap::new(),
            foreign_functions: std::collections::HashSet::new(),
            function_types: Vec::new(),
            dynamic_types: Vec::new(),
            current_type_parameters: Vec::new(),
            current_type_parameter_bounds: Vec::new(),
            current_associated_bounds: Vec::new(),
            for_into_cursor: HashMap::new(),
            generic_calls: HashMap::new(),
            generic_call_spans: HashMap::new(),
            type_value_calls: HashMap::new(),
            tuple_types: Vec::new(),
            option_types: vec![ordering],
            result_types: Vec::new(),
            list_types: Vec::new(),
            maps: Vec::new(),
            cowns: Vec::new(),
            associated_types: Vec::new(),
            current_trait_context: None,
            structs: HashMap::new(),
            generated_structs: HashMap::new(),
            generated_enums: HashMap::new(),
            classes: HashMap::new(),
            enums: HashMap::from([(
                "Ordering".into(),
                EnumInfo {
                    id: 0,
                    variants: ["Less", "Equal", "Greater"]
                        .into_iter()
                        .map(|name| EnumVariantInfo {
                            name: name.into(),
                            fields: vec![],
                        })
                        .collect(),
                },
            )]),
            show_impls: HashSet::new(),
            traits,
            trait_impls: HashMap::new(),
            trait_associated_impls: HashMap::new(),
            effects: super::effects::EffectRegistry::default(),
            effect_aliases: HashMap::new(),
            current_effects: super::effects::EffectSet::new(),
            current_effect_groups: Vec::new(),
            handler_operations: HashMap::new(),
            effect_operations: HashMap::new(),
            closure_captures: HashMap::new(),
            closure_capture_bindings: HashMap::new(),
            borrowed_mutable_captures: HashSet::new(),
            when_depth: 0,
            expr_depth: 0,
            handler_abort_types: Vec::new(),
        };
        let parsed = Type::Result(intern_result(
            &mut checker.result_types,
            Type::SelfType,
            Type::String,
        ));
        checker.traits.insert(
            "FromString".into(),
            TraitInfo {
                associated_types: HashSet::new(),
                methods: HashMap::from([(
                    "from_string".into(),
                    TraitMethodInfo {
                        effect_names: vec![],
                        receiver_mode: crate::syntax::ReceiverMode::Static,
                        parameter_borrows: vec![true],
                        parameters: vec![("value".into(), Type::String)],
                        return_type: parsed,
                    },
                )]),
            },
        );
        let item = intern_associated_type(&mut checker.associated_types, None, "Cursor", "Item");
        let pair = Type::Tuple(intern_tuple(
            &mut checker.tuple_types,
            vec![item, Type::SelfType],
        ));
        let result = Type::Option(intern_option(&mut checker.option_types, pair));
        checker.traits.insert(
            "Cursor".into(),
            TraitInfo {
                associated_types: HashSet::from(["Item".into()]),
                methods: HashMap::from([(
                    "advance".into(),
                    TraitMethodInfo {
                        effect_names: vec![],
                        receiver_mode: crate::syntax::ReceiverMode::Owned,
                        parameter_borrows: vec![],
                        parameters: vec![],
                        return_type: result,
                    },
                )]),
            },
        );
        for declaration in super::intrinsic_constraints::builtin_intrinsic_declarations() {
            checker.register_intrinsic_type(declaration);
        }
        checker
    }
}

impl PatternTypes for Checker {
    fn enum_variant_count(&self, enum_id: usize) -> usize {
        if super::pattern::option_id_from_pattern_id(enum_id).is_some() {
            return 2;
        }
        if super::pattern::result_id_from_pattern_id(enum_id).is_some() {
            return 2;
        }
        self.enums
            .values()
            .find(|enumeration| enumeration.id == enum_id)
            .expect("checked enum")
            .variants
            .len()
    }

    fn enum_variant_fields(&self, enum_id: usize, variant_index: usize) -> Vec<Type> {
        if let Some(option_id) = super::pattern::option_id_from_pattern_id(enum_id) {
            return (variant_index == 0)
                .then_some(self.option_types[option_id])
                .into_iter()
                .collect();
        }
        if let Some(result_id) = super::pattern::result_id_from_pattern_id(enum_id) {
            let (ok, err) = self.result_types[result_id];
            return vec![if variant_index == 0 { ok } else { err }];
        }
        self.enums
            .values()
            .find(|enumeration| enumeration.id == enum_id)
            .expect("checked enum")
            .variants[variant_index]
            .fields
            .iter()
            .map(|(_, field_type)| *field_type)
            .collect()
    }

    fn tuple_elements(&self, tuple_id: usize) -> Vec<Type> {
        self.tuple_types[tuple_id].clone()
    }
}

pub(super) fn intern_map(maps: &mut Vec<super::types::MapInfo>, key: Type, value: Type) -> usize {
    maps.iter()
        .position(|info| info.key == key && info.value == value)
        .unwrap_or_else(|| {
            let id = maps.len();
            maps.push(super::types::MapInfo { key, value });
            id
        })
}

pub(super) fn intern_tuple(tuples: &mut Vec<Vec<Type>>, elements: Vec<Type>) -> usize {
    if let Some(id) = tuples.iter().position(|candidate| candidate == &elements) {
        id
    } else {
        let id = tuples.len();
        tuples.push(elements);
        id
    }
}

pub(super) fn intern_option(options: &mut Vec<Type>, value: Type) -> usize {
    if let Some(id) = options.iter().position(|candidate| *candidate == value) {
        id
    } else {
        let id = options.len();
        options.push(value);
        id
    }
}

pub(super) fn intern_result(results: &mut Vec<(Type, Type)>, ok: Type, err: Type) -> usize {
    if let Some(id) = results.iter().position(|candidate| *candidate == (ok, err)) {
        id
    } else {
        let id = results.len();
        results.push((ok, err));
        id
    }
}

pub(super) fn intern_function(
    functions: &mut Vec<super::symbol_table::FunctionTypeInfo>,
    parameter_names: Vec<String>,
    parameters: Vec<Type>,
    return_type: Type,
) -> usize {
    functions
        .iter()
        .position(|info| {
            info.parameter_names == parameter_names
                && info.parameters == parameters
                && info.return_type == return_type
        })
        .unwrap_or_else(|| {
            let id = functions.len();
            functions.push(super::symbol_table::FunctionTypeInfo {
                parameter_names,
                parameters,
                return_type,
                effects: super::effects::EffectSet::new(),
                suspends: false,
            });
            id
        })
}

pub(super) fn intern_list(lists: &mut Vec<Type>, element: Type) -> usize {
    if let Some(id) = lists.iter().position(|candidate| *candidate == element) {
        id
    } else {
        let id = lists.len();
        lists.push(element);
        id
    }
}

pub(super) fn intern_cown(cowns: &mut Vec<Type>, payload: Type) -> usize {
    if let Some(id) = cowns.iter().position(|candidate| *candidate == payload) {
        id
    } else {
        let id = cowns.len();
        cowns.push(payload);
        id
    }
}

pub(super) fn intern_associated_type(
    associated_types: &mut Vec<AssociatedTypeInfo>,
    parameter: Option<usize>,
    trait_name: &str,
    name: &str,
) -> Type {
    if let Some(id) = associated_types.iter().position(|candidate| {
        candidate.parameter == parameter
            && candidate.trait_name == trait_name
            && candidate.name == name
    }) {
        return Type::Associated(id);
    }
    let id = associated_types.len();
    associated_types.push(AssociatedTypeInfo {
        parameter,
        trait_name: trait_name.to_owned(),
        name: name.to_owned(),
    });
    Type::Associated(id)
}
