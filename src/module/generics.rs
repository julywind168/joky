//! Generic templates are checked in their defining module and instantiated on demand.
use super::SymbolId;
use crate::sema::{ExternalFunction, Type};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct GenericTemplate {
    pub parameters: Vec<String>,
    pub bounds: Vec<Vec<String>>,
    pub abi: ExternalFunction,
    pub effects: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct GenericRequest {
    pub definition: SymbolId,
    pub arguments: Vec<Type>,
    pub abi: ExternalFunction,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct MethodAbi {
    pub receiver: Type,
    pub receiver_mode: crate::syntax::ReceiverMode,
    pub name: String,
    pub symbol: SymbolId,
    pub abi: ExternalFunction,
    pub effects: Vec<String>,
    pub declared_effects: Vec<String>,
}
