//! Language signatures used by compiled exports and host registrations.
use crate::core::types::Type;

/// Physical protocol of an exported entry, separate from its language signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryAbi {
    Scalar,
    OwnedTextV2,
}

#[derive(Clone)]
pub struct FunctionSignature {
    pub params: Vec<Type>,
    pub ret: Type,
}

impl PartialEq for FunctionSignature {
    fn eq(&self, other: &Self) -> bool {
        self.params.len() == other.params.len()
            && crate::core::types::equal_types(
                self.params
                    .iter()
                    .zip(&other.params)
                    .chain(std::iter::once((&self.ret, &other.ret))),
            )
    }
}
impl Eq for FunctionSignature {}
impl std::fmt::Debug for FunctionSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            Type::function(self.params.clone(), self.ret.clone())
        )
    }
}

impl FunctionSignature {
    pub(crate) fn has_text(&self) -> bool {
        self.params
            .iter()
            .chain(std::iter::once(&self.ret))
            .any(|ty| {
                *ty == Type::String
                    || ty
                        .reference_type()
                        .is_some_and(|r| r.target == Type::String)
            })
    }
    /// Bound shared signature DAGs without expanding duplicate branches.
    pub(crate) fn type_depth(&self) -> usize {
        let mut pending: Vec<_> = self
            .params
            .iter()
            .chain(std::iter::once(&self.ret))
            .map(|t| (t, 1usize))
            .collect();
        let mut seen = std::collections::HashMap::new();
        let mut maximum = 1;
        while let Some((ty, depth)) = pending.pop() {
            if depth > 256 {
                return depth;
            }
            match ty {
                Type::Function(signature) => {
                    let previous = seen.entry(std::sync::Arc::as_ptr(signature)).or_insert(0);
                    if *previous >= depth {
                        continue;
                    }
                    *previous = depth;
                    pending.extend(
                        signature
                            .signature
                            .params
                            .iter()
                            .chain(std::iter::once(&signature.signature.ret))
                            .map(|t| (t, depth + 1)),
                    );
                }
                Type::Reference(r) => pending.push((&r.target, depth + 1)),
                _ => maximum = maximum.max(depth + ty.layout_depth()),
            }
        }
        maximum
    }

    pub fn new(params: Vec<Type>, ret: Type) -> Self {
        Self { params, ret }
    }

    /// True when `call_async` may start this signature with `arity` Int32 arguments.
    pub fn accepts_async_call(&self, arity: usize) -> bool {
        self.accepts_integer_async(arity, &Type::Int32)
    }

    pub(crate) fn accepts_integer_async(&self, arity: usize, integer: &Type) -> bool {
        integer.is_numeric()
            && (self.ret == *integer || self.ret == Type::Unit)
            && self.params.len() == arity
            && self.params.iter().all(|ty| ty == integer)
    }
}
