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

    /// True when `call_async` may start this signature with `arity` Int32 arguments.
    pub fn accepts_async_call(&self, arity: usize) -> bool {
        matches!(self.ret, Type::Int32 | Type::Unit) && self.accepts_int32_args(arity)
    }

    pub(crate) fn accepts_int32_args(&self, arity: usize) -> bool {
        self.params.len() == arity && self.params.iter().all(|ty| *ty == Type::Int32)
    }
}
