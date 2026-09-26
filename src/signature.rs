//! Language signatures used by compiled exports and host registrations.
use crate::parser::ast::Type;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    pub params: Vec<Type>,
    pub ret: Type,
}

impl FunctionSignature {
    pub fn new(params: Vec<Type>, ret: Type) -> Self {
        Self { params, ret }
    }
}
