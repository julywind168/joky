//! Symbol information for declarations, types, and method signatures

use std::collections::{HashMap, HashSet};

use crate::syntax::Expr;

use super::effects::{EffectGroupSet, EffectSet};
use super::types::Type;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct TraitInfo {
    pub(super) associated_types: HashSet<String>,
    pub(super) methods: HashMap<String, TraitMethodInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AssociatedTypeInfo {
    pub(super) parameter: Option<usize>,
    pub(super) trait_name: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct TraitMethodInfo {
    pub(super) effect_names: Vec<String>,
    pub(super) receiver_mode: crate::syntax::ReceiverMode,
    pub(super) parameter_borrows: Vec<bool>,
    pub(super) parameters: Vec<(String, Type)>,
    pub(super) return_type: Type,
}

#[derive(Clone)]
pub(super) struct FunctionSignature {
    pub(super) receiver_mode: crate::syntax::ReceiverMode,
    pub(super) parameter_borrows: Vec<bool>,
    pub(super) type_parameters: Vec<String>,
    pub(super) type_parameter_bounds: Vec<Vec<String>>,
    pub(super) associated_bounds: Vec<(Type, Vec<String>)>,
    pub(super) parameters: Vec<(String, Type)>,
    pub(super) return_type: Type,
    pub(super) declared_effects: EffectGroupSet,
    pub(super) used_effects: EffectSet,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct FunctionTypeInfo {
    pub(crate) parameter_names: Vec<String>,
    pub(crate) parameters: Vec<Type>,
    pub(crate) return_type: Type,
    pub(crate) effects: EffectSet,
    /// Whether values of this function type may return Pending.
    pub(crate) suspends: bool,
}

#[derive(Clone)]
pub(super) struct StructInfo {
    pub(super) id: usize,
    pub(super) repr_c: bool,
    pub(super) fields: Vec<(String, Type)>,
    pub(super) defaults: Vec<Option<Expr>>,
    pub(super) methods: HashMap<String, FunctionSignature>,
}

#[derive(Clone)]
pub(super) struct ClassFieldInfo {
    pub(super) ty: Type,
    pub(super) mutable: bool,
}

#[derive(Clone)]
pub(super) struct ClassInfo {
    pub(super) id: usize,
    pub(super) fields: Vec<(String, ClassFieldInfo, Option<Expr>)>,
    pub(super) methods: HashMap<String, FunctionSignature>,
}

#[derive(Clone)]
pub(super) struct EnumVariantInfo {
    pub(super) name: String,
    pub(super) fields: Vec<(String, Type)>,
}

#[derive(Clone)]
pub(super) struct EnumInfo {
    pub(super) id: usize,
    pub(super) variants: Vec<EnumVariantInfo>,
}
