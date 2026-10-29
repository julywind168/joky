//! Conservative call-graph evidence for scoped read-only task inputs.
use super::*;
use crate::core::ir::FunctionPlan;
use std::sync::Arc;

/// Recover finite nominal layouts for private task input evidence. Language
/// signatures remain shallow; only this compiler-owned descriptor is expanded.
pub(crate) fn signature(plan: &ModulePlan, name: &str) -> crate::FunctionSignature {
    fn expand(body: &FunctionPlan, ty: &Type, depth: usize, work: &mut usize) -> Option<Type> {
        *work += 1;
        if depth > 32 || *work > 65_536 {
            return None;
        }
        let mut ty = body.pointee(ty, &body.type_registry).ok()?;
        if body.has_custom_drop(&ty) {
            return None;
        }
        match &mut ty {
            Type::Record(r) => {
                let r = Arc::make_mut(r);
                for field in &mut r.fields {
                    field.ty = expand(body, &field.ty, depth + 1, work)?;
                }
                for argument in &mut r.arguments {
                    *argument = argument
                        .try_map_type(|ty| expand(body, ty, depth + 1, work).ok_or(()))
                        .ok()?;
                }
            }
            Type::Tuple(t) => {
                ty = Type::tuple(
                    t.elements
                        .iter()
                        .map(|element| expand(body, element, depth + 1, work))
                        .collect::<Option<Vec<_>>>()?,
                );
            }
            Type::Buffer(element) | Type::BufferSlice(element) => {
                *element = Arc::new(expand(body, element, depth + 1, work)?);
            }
            _ => {}
        }
        Some(ty)
    }
    let signature = &plan.functions[name].signature;
    let params = signature
        .params
        .iter()
        .map(|ty| {
            let Some(reference) = ty.reference_type() else {
                return ty.clone();
            };
            let Some(body) = plan.bodies.get(name) else {
                return ty.clone();
            };
            let Some(target) = expand(body, &reference.target, 0, &mut 0) else {
                // Private evidence must remain inadmissible even when the
                // original signature already contains a fully expanded owner.
                return Type::reference(Type::Ptr, reference.mutable);
            };
            let resolved = Type::reference(target, reference.mutable);
            if crate::runtime::parallel::scoped::Shape::of(&resolved).is_some() {
                resolved
            } else {
                ty.clone()
            }
        })
        .collect();
    crate::FunctionSignature::new(params, signature.ret.clone())
}

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
    let targets = callables::Targets::new(plan);
    let mut rejected = HashMap::new();
    let mut edges = HashMap::<String, BTreeSet<String>>::new();
    for (name, info) in &plan.functions {
        let reason = if let Some(body) = plan.bodies.get(name) {
            let dependencies = edges.entry(name.clone()).or_default();
            let mut reason = None;
            for op in &body.ops {
                dependencies.extend(op.dependencies().cloned());
                let failure = match op {
                    Op::Call {
                        indirect: Some(slot),
                        ..
                    } => match &slot.ty {
                        Type::Function(function) => {
                            let candidates: BTreeSet<_> = targets.matching(function).collect();
                            if candidates.is_empty() {
                                Some("indirect call has no frozen language targets")
                            } else if targets.dynamic_matching(function).next().is_some() {
                                Some("indirect call may use dynamic dispatch outside the scoped callback protocol")
                            } else if candidates
                                .iter()
                                .any(|name| !plan.bodies.contains_key(*name))
                            {
                                Some("indirect call may target a host ABI; scoped callbacks require language code")
                            } else {
                                dependencies.extend(candidates.into_iter().map(str::to_owned));
                                None
                            }
                        }
                        _ => Some("indirect call lacks a checked function type"),
                    },
                    Op::FunctionRef(slot, _) if !matches!(slot.ty, Type::Function(_)) => {
                        Some("raw function address is outside the scoped callback protocol")
                    }
                    Op::FunctionRef(_, target) if !plan.bodies.contains_key(target) => Some(
                        "function value targets a host ABI; scoped callbacks require language code",
                    ),
                    Op::Concurrent { branches, .. }
                        if branches.iter().any(|b| b.indirect.is_some()) =>
                    {
                        Some("call graph contains an indirect concurrent target")
                    }
                    _ => None,
                };
                if reason.is_none() {
                    reason = failure;
                }
            }
            reason
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
            if let Some(dependency) = edges[*name]
                .iter()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_callbacks_require_known_language_targets() {
        let source = r#"
            fn missing(callback: fn(Int64) -> Int64) -> Int64 { callback(1) }
            fn suspended(value: Int64) @suspends -> Int64 { value }
            fn expose() -> fn(Int64) @suspends -> Int64 { suspended }
        "#;
        let types = crate::TypeRegistry::default().with_prelude(false);
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap();
        let prepared =
            crate::core::prepared::PreparedModule::new(&ast, &types, &Default::default()).unwrap();
        let plan = prepared.plan(&Default::default()).unwrap();
        let rejected = rejections(&plan);
        assert!(rejected["missing"].contains("no frozen language targets"));
        assert!(!rejected.contains_key("suspended"));
    }
}
