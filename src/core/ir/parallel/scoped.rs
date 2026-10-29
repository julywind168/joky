//! Conservative call-graph evidence for scoped read-only task inputs.
use super::*;

pub(crate) fn callees(plan: &ModulePlan) -> HashSet<String> {
    let rejected = rejections(plan);
    plan.bodies
        .keys()
        .filter(|name| !rejected.contains_key(*name))
        .cloned()
        .collect()
}

/// Keep the first deterministic witness as well as the admission decision.
pub(crate) fn rejections(plan: &ModulePlan) -> HashMap<String, String> {
    let mut rejected = HashMap::new();
    for (name, info) in &plan.functions {
        let reason = if let Some(body) = plan.bodies.get(name) {
            body.ops.iter().find_map(|op| match op {
                Op::Call {
                    indirect: Some(_), ..
                } => Some("call graph contains an indirect call"),
                Op::FunctionRef(..) => Some("call graph contains a function value"),
                Op::Concurrent { branches, .. }
                    if branches.iter().any(|b| b.indirect.is_some()) =>
                {
                    Some("call graph contains an indirect concurrent target")
                }
                _ => None,
            })
        } else if info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
            .any(Type::contains_references)
        {
            Some("host reference ABI is outside the scoped view protocol")
        } else {
            None
        };
        if let Some(reason) = reason {
            rejected.insert(name.clone(), reason.to_owned());
        }
    }
    let mut names: Vec<_> = plan.bodies.keys().collect();
    names.sort();
    loop {
        let mut changed = false;
        for name in &names {
            if rejected.contains_key(*name) {
                continue;
            }
            let mut dependencies: Vec<_> = plan.bodies[*name]
                .ops
                .iter()
                .flat_map(Op::dependencies)
                .collect();
            dependencies.sort();
            if let Some(dependency) = dependencies
                .into_iter()
                .find(|dep| rejected.contains_key(*dep) || !plan.functions.contains_key(*dep))
            {
                let label = display_name(dependency);
                let reason = format!(
                    "callee or cleanup `{label}`: {}",
                    rejected
                        .get(dependency)
                        .map_or("unknown target", String::as_str)
                );
                rejected.insert((*name).clone(), reason);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    rejected
}
