//! Conservative call-graph evidence for scoped read-only task inputs.
use super::*;

pub(crate) fn callees(plan: &ModulePlan) -> HashSet<String> {
    let mut safe = HashSet::new();
    for (name, info) in &plan.functions {
        let allowed = if let Some(body) = plan.bodies.get(name) {
            !body.ops.iter().any(|op| {
                matches!(op, Op::Call { indirect: Some(_), .. } | Op::FunctionRef(..))
                    || matches!(op, Op::Concurrent { branches, .. } if branches.iter().any(|b| b.indirect.is_some()))
            })
        } else {
            // Host reference ABIs are not part of the private view protocol,
            // even when the binding is otherwise ParallelSafe.
            info.signature
                .params
                .iter()
                .chain(std::iter::once(&info.signature.ret))
                .all(|ty| !ty.contains_references())
        };
        if allowed {
            safe.insert(name.clone());
        }
    }
    loop {
        let rejected: Vec<_> = safe
            .iter()
            .filter(|name| {
                plan.bodies.get(*name).is_some_and(|body| {
                    body.ops
                        .iter()
                        .flat_map(Op::dependencies)
                        .any(|dependency| !safe.contains(dependency))
                })
            })
            .cloned()
            .collect();
        if rejected.is_empty() {
            break;
        }
        for name in rejected {
            safe.remove(&name);
        }
    }
    // A borrowed task always starts in generated language code, never a host ABI.
    safe.retain(|name| plan.bodies.contains_key(name));
    safe
}
