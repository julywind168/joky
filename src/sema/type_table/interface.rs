//! Export/import metadata in a module's local type-index space.
use super::*;
use std::ops::{Deref, DerefMut};

/// A struct default is either a relocatable constant or the deferred diagnostic
/// for an expression which is legal locally but cannot cross a module boundary.
pub(crate) type StructDefault = Option<Result<crate::module::constants::ConstantValue, String>>;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub(crate) struct ModuleInterface {
    pub(in crate::sema) trait_definitions: HashMap<String, crate::sema::symbol_table::TraitInfo>,
    pub(crate) pending_functions: HashMap<String, bool>,
    pub(crate) public_abis: HashMap<String, ExternalFunction>,
    pub(crate) public_constants: HashMap<String, crate::module::constants::ConstantValue>,
    pub(crate) public_templates: HashMap<String, crate::module::generics::GenericTemplate>,
    pub(crate) generic_requests: HashMap<String, crate::module::generics::GenericRequest>,
    pub(crate) generic_symbols: HashMap<String, SymbolId>,
    pub(crate) intrinsic_types: Vec<crate::syntax::IntrinsicType>,
    pub(crate) type_program: Option<Box<crate::syntax::Program>>,
    pub(crate) module_imports: HashMap<String, crate::module::StableId>,
    pub(crate) module_exports: HashMap<crate::module::StableId, HashMap<String, String>>,
    pub(crate) standard_modules: HashSet<crate::module::StableId>,
    pub(crate) module_identity: Option<crate::module::StableId>,
    pub(crate) methods: Vec<crate::module::generics::MethodAbi>,
    pub(crate) method_symbols: HashMap<(Type, String), SymbolId>,
    pub(crate) struct_defaults: Vec<Vec<StructDefault>>,
}

/// Read-only import snapshots and checked source modules combine their layouts
/// with the interface. MIR and codegen use only the layout `TypeTable`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub(crate) struct ModuleTypes {
    pub(crate) types: TypeTable,
    pub(crate) interface: ModuleInterface,
}

impl Deref for ModuleTypes {
    type Target = TypeTable;
    fn deref(&self) -> &TypeTable {
        &self.types
    }
}

impl DerefMut for ModuleTypes {
    fn deref_mut(&mut self) -> &mut TypeTable {
        &mut self.types
    }
}

impl ModuleTypes {
    pub(in crate::sema) fn snapshot(checker: &crate::sema::checker::Checker) -> Self {
        Self {
            types: TypeTable {
                dynamic_types: checker.dynamic_types.clone(),
                tuples: checker.tuple_types.clone(),
                c_pointers: checker.c_pointers.clone(),
                c_arrays: checker.c_arrays.clone(),
                structs: checker.struct_layouts(),
                classes: checker.class_layouts(),
                enums: checker.enum_layouts(),
                options: checker.option_types.clone(),
                results: checker.result_types.clone(),
                lists: checker.list_types.clone(),
                maps: checker.maps.clone(),
                cowns: checker.cowns.clone(),
                function_types: checker.function_types.clone(),
                effects: checker.effects.clone(),
                associated_types: checker.associated_types.clone(),
                trait_associated_impls: checker.trait_associated_impls.clone(),
                trait_implementations: checker.trait_impls.clone(),
                show_impls: checker.show_impls.clone(),
                explicit_hash_impls: TypeTable::explicit_hash_impls(checker),
            },
            interface: ModuleInterface {
                trait_definitions: checker.traits.clone(),
                methods: checker.imported_method_abis.clone(),
                ..Default::default()
            },
        }
    }

    pub(crate) fn is_static_trait_method(&self, trait_name: &str, method: &str) -> bool {
        self.interface
            .trait_definitions
            .get(trait_name)
            .and_then(|info| info.methods.get(method))
            .is_some_and(|info| info.receiver_mode == crate::syntax::ReceiverMode::Static)
    }

    pub(crate) fn instance_name(&self, function: &str, arguments: &[Type]) -> String {
        let keys = arguments
            .iter()
            .map(|argument| self.stable_type_key(*argument, crate::module::StableId(0)))
            .collect::<Vec<_>>();
        let suffix = if keys.len() == 1 {
            keys[0].clone()
        } else {
            format!("{keys:?}")
        };
        format!("{function}$${suffix}")
    }

    pub(crate) fn closure_state_type(&mut self, captures: &[(String, Type)]) -> Type {
        let module = self
            .interface
            .module_identity
            .unwrap_or(crate::module::StableId(0));
        let key = captures
            .iter()
            .map(|(_, ty)| hex::encode(self.stable_type_key(*ty, module).as_bytes()))
            .collect::<Vec<_>>()
            .join("/");
        let name = format!("@closure/state/{key}");
        if let Some(ty) = self.class_type(&name) {
            return ty;
        }
        let ty = Type::Class(self.classes.len());
        self.classes.push(ClassLayout {
            name,
            fields: captures
                .iter()
                .enumerate()
                .map(|(index, (_, ty))| (format!("capture{index}"), *ty))
                .collect(),
            drop_effects: None,
        });
        ty
    }

    pub(crate) fn sort_state_type(&mut self, list: usize) -> Type {
        // Helpers use ordinary class fields for loop-carried mutable state.
        // Generic artifacts can reuse a local list index for different elements.
        let key = self.stable_type_key(
            Type::List(list),
            self.interface
                .module_identity
                .unwrap_or(crate::module::StableId(0)),
        );
        let name = format!("@sort/state/{}", hex::encode(key.as_bytes()));
        if let Some(ty) = self.class_type(&name) {
            return ty;
        }
        let mut fields = [
            "n", "i", "width", "start", "mid", "end", "left", "right", "out",
        ]
        .into_iter()
        .map(|name| (name.into(), Type::U64))
        .collect::<Vec<_>>();
        fields.extend([
            ("cursor".into(), Type::List(list)),
            ("result".into(), Type::List(list)),
        ]);
        let ty = Type::Class(self.classes.len());
        self.classes.push(ClassLayout {
            name,
            fields,
            drop_effects: None,
        });
        ty
    }

    pub(crate) fn struct_import_defaults(&self, id: usize) -> Option<&[StructDefault]> {
        self.interface.struct_defaults.get(id).map(Vec::as_slice)
    }
}
