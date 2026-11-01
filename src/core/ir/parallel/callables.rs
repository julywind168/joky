//! Closed callable targets from one immutable, compiler-authored module.
use super::*;
use crate::core::types::FunctionType;

pub(super) struct Targets<'a> {
    functions: Vec<(&'a FunctionType, &'a str)>,
    dynamic: Vec<(&'a crate::core::types::DynMethodSlot, &'a str)>,
}
impl<'a> Targets<'a> {
    pub(super) fn new(plan: &'a ModulePlan) -> Self {
        let mut result = Self {
            functions: Vec::new(),
            dynamic: Vec::new(),
        };
        for op in plan.bodies.values().flat_map(|body| &body.ops) {
            match op {
                Op::FunctionRef(slot, name) => {
                    if let Type::Function(function) = &slot.ty {
                        result.functions.push((function, name));
                    }
                }
                Op::DynTable { table, .. } => {
                    for (method, name) in table.interface.methods.iter().zip(&table.methods) {
                        result.dynamic.push((method, name));
                    }
                }
                _ => {}
            }
        }
        result
    }

    pub(super) fn matching<'b>(
        &'b self,
        function: &'b FunctionType,
    ) -> impl Iterator<Item = &'a str> + 'b {
        self.functions
            .iter()
            .filter(move |(candidate, _)| {
                candidate.signature == function.signature && candidate.suspends == function.suspends
            })
            .map(|(_, name)| *name)
            .chain(self.dynamic_matching(function))
    }

    pub(super) fn dynamic_matching<'b>(
        &'b self,
        function: &'b FunctionType,
    ) -> impl Iterator<Item = &'a str> + 'b {
        self.dynamic
            .iter()
            .filter(move |(method, _)| {
                method.suspends == function.suspends && method.signature == function.signature
            })
            .map(|(_, name)| *name)
    }
}
