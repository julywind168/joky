//! Validate automatic user destruction after specialization and linking, too.
use std::collections::{HashMap, HashSet};

use crate::mir::*;
use crate::sema::{EffectMode, EffectSet, Type};
use crate::Diagnostic;

fn failure(message: impl Into<String>) -> Diagnostic {
    Diagnostic::codegen(format!("Drop: {}", message.into()))
}

pub(super) fn verify(program: &MirProgram) -> Result<(), Diagnostic> {
    let types = program.types();
    let functions: HashMap<_, _> = program.functions().iter().map(|f| (f.id, f)).collect();
    let destructors: HashMap<_, _> = program
        .functions()
        .iter()
        .filter_map(|f| {
            let Some(Type::Class(id)) = f.receiver else {
                return None;
            };
            (f.name == crate::sema::DROP_METHOD && types.class_drop_effects(id).is_some())
                .then_some((id, f.id))
        })
        .collect();
    for (&class, &function) in &destructors {
        let f = functions[&function];
        if f.foreign.is_some()
            || f.is_suspending
            || f.return_type != Type::Unit
            || !f.parameters.is_empty()
            || f.receiver_local
                .is_none_or(|local| f.locals[local.0].ownership != MirOwnership::Borrowed)
        {
            return Err(failure(
                "destructor requires a synchronous fn drop(&self) -> Unit",
            ));
        }
        for op in types.class_drop_effects(class).unwrap().iter() {
            let info = types
                .effects()
                .operation_info(op)
                .ok_or_else(|| failure("unknown cleanup effect"))?;
            if info.suspends || info.mode != EffectMode::Normal {
                return Err(failure(
                    "cleanup effects must be synchronous and non-abortive",
                ));
            }
        }
        verify_call_tree(function, &functions, &destructors, &mut HashSet::new())?;
    }

    let mut requirements = HashMap::new();
    for f in program.functions() {
        if f.external_symbol.is_some() {
            continue;
        }
        let mut effects = EffectSet::new();
        for local in &f.locals {
            if local.ownership == MirOwnership::Owned {
                effects.extend(&types.drop_contract(local.ty).1);
            }
        }
        for (ty, ownership) in f.value_types.iter().zip(&f.value_ownership) {
            if *ownership == MirOwnership::Owned {
                effects.extend(&types.drop_contract(*ty).1);
            }
        }
        requirements.insert(f.id, effects);
        for statement in f.blocks.iter().flat_map(|b| &b.statements) {
            match statement {
                MirStatement::Call { function, .. }
                | MirStatement::MethodCall {
                    method: function, ..
                } if destructors.values().any(|id| id == function)
                    && !(f.name.starts_with("@method/")
                        && f.name.ends_with(&format!("/{}", crate::sema::DROP_METHOD))
                        && f.parameters.len() == 1
                        && f.parameters[0].ownership == MirOwnership::Borrowed
                        && functions[function].receiver == Some(f.parameters[0].ty)) =>
                {
                    return Err(failure(format!(
                        "destructor cannot be called directly: {} -> {:?} ({})",
                        f.name, function, functions[function].name
                    )));
                }
                MirStatement::Project {
                    base, destination, ..
                } if matches!(f.value_types[base.0], Type::Class(id) if types.class_drop_effects(id).is_some())
                    && f.value_ownership[destination.0] == MirOwnership::Owned =>
                {
                    return Err(failure("cannot move owned fields out of a class with Drop"));
                }
                MirStatement::FunctionValue { captures, .. }
                    if captures
                        .iter()
                        .any(|v| types.drop_contract(f.value_types[v.0]).0) =>
                {
                    return Err(failure(
                        "values with user Drop cannot enter shared closure environments",
                    ));
                }
                MirStatement::RuntimeCall {
                    intrinsic: RuntimeIntrinsic::CownNew(payload),
                    ..
                } if types.drop_contract(*payload).0 => {
                    return Err(failure("Cown cannot own values with user Drop"));
                }
                _ => {}
            }
        }
    }
    // Implicit cleanup cannot be discharged by a lexical handler: cancellation
    // may run after that handler has been removed. Propagate static call edges.
    loop {
        let previous = requirements.clone();
        let mut changed = false;
        for f in program.functions() {
            let Some(effects) = requirements.get_mut(&f.id) else {
                continue;
            };
            for statement in f.blocks.iter().flat_map(|b| &b.statements) {
                let callee = match statement {
                    MirStatement::Call { function, .. }
                    | MirStatement::TaskCreate { function, .. } => Some(function),
                    MirStatement::MethodCall { method, .. } => Some(method),
                    _ => None,
                };
                if let Some(required) = callee.and_then(|id| previous.get(id)) {
                    effects.extend(required);
                }
            }
            changed |= *effects != previous[&f.id];
        }
        if !changed {
            break;
        }
    }
    for f in program.functions() {
        if let Some(required) = requirements.get(&f.id) {
            for op in required.iter() {
                if !f.declared_effects.contains(op.effect) {
                    return Err(failure(format!(
                        "function '{}' must declare cleanup effect '{}'",
                        f.name,
                        types.effects().effect(op.effect).unwrap().name
                    )));
                }
            }
        }
    }
    Ok(())
}

fn verify_call_tree(
    id: MirFunctionId,
    functions: &HashMap<MirFunctionId, &MirFunction>,
    destructors: &HashMap<usize, MirFunctionId>,
    seen: &mut HashSet<MirFunctionId>,
) -> Result<(), Diagnostic> {
    if !seen.insert(id) {
        return Ok(());
    }
    let f = functions[&id];
    // Import stubs are checked against their concrete implementation after link.
    if f.external_symbol.is_some() || f.foreign.is_some() {
        return Ok(());
    }
    if f.is_suspending || f.is_task {
        return Err(failure(format!("'{}' can suspend or start a task", f.name)));
    }
    for statement in f.blocks.iter().flat_map(|b| &b.statements) {
        match statement {
            MirStatement::Call { function, .. }
            | MirStatement::MethodCall {
                method: function, ..
            } => {
                verify_call_tree(*function, functions, destructors, seen)?;
            }
            MirStatement::CallIndirect { .. }
            | MirStatement::FunctionValue { .. }
            | MirStatement::HandlerEnter { .. }
            | MirStatement::HandlerExit
            | MirStatement::HandlerRequest { .. }
            | MirStatement::ResumableRequest { .. }
            | MirStatement::Suspend { .. }
            | MirStatement::TaskCreate { .. }
            | MirStatement::TaskAbort { .. }
            | MirStatement::TaskFailureRethrow { .. }
            | MirStatement::RuntimeCall {
                intrinsic: RuntimeIntrinsic::Panic,
                ..
            } => {
                return Err(failure(format!("'{}' uses panic, suspension, tasks, handlers or indirect calls during destruction", f.name)));
            }
            _ => {}
        }
    }
    // A helper may drop a local/temporary with its own user destructor.
    for ty in f
        .locals
        .iter()
        .map(|l| l.ty)
        .chain(f.value_types.iter().copied())
    {
        if let Type::Class(class) = ty {
            if let Some(destructor) = destructors.get(&class) {
                verify_call_tree(*destructor, functions, destructors, seen)?;
            }
        }
    }
    Ok(())
}
