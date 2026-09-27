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
        self.accepts_integer_async(arity, &Type::Int32)
    }

    pub(crate) fn accepts_integer_async(&self, arity: usize, integer: &Type) -> bool {
        matches!(integer, Type::Int32 | Type::Int64)
            && (self.ret == *integer || self.ret == Type::Unit)
            && self.params.len() == arity
            && self.params.iter().all(|ty| ty == integer)
    }

    pub(crate) fn accepts_int32_args(&self, arity: usize) -> bool {
        self.params.len() == arity && self.params.iter().all(|ty| *ty == Type::Int32)
    }
}
