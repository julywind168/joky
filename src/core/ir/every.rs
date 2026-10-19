//! Periodic declarations lower to ordinary named timer operations.
use super::*;
use crate::parser::ast::Param;

pub(super) fn entry(name: &str) -> String {
    format!("$every/{}:{name}", name.len())
}

pub(super) fn expand(module: &mut AstModule) -> Result<(), Error> {
    let mut entries = Vec::new();
    for item in &module.items {
        let Item::FnDef(function) = item else {
            continue;
        };
        if function.every.is_none() {
            continue;
        }
        if !function.type_params.is_empty()
            || function.suspends
            || !(1..=2).contains(&function.params.len())
        {
            return Err(type_error(&function.span, ErrorCode::InvalidCall,
                "#every requires a nongeneric synchronous function with one state receiver and at most one pure-value input"));
        }
        // Keep a separate stable registration identity for both signatures.
        // Removing the declaration removes this entry even if the ordinary
        // function remains. Cloned bodies retain their defining import scope.
        let mut adapter = function.clone();
        adapter.name = entry(&function.name);
        adapter.public = false;
        adapter.every = None;
        if adapter.params.len() == 1 {
            adapter.params.push(Param {
                name: "$every.input".into(),
                ty: Type::Unit,
                span: function.span,
            });
        }
        entries.push(Item::FnDef(adapter));
    }
    module.items.extend(entries);
    Ok(())
}

pub(super) fn validate(functions: &HashMap<String, FunctionInfo>) -> Result<(), Error> {
    for info in functions.values().filter(|f| f.every.is_some()) {
        if !info.domain_entry
            || info.suspends
            || info.signature.ret != Type::Unit
            || !(1..=2).contains(&info.signature.params.len())
            || !info.signature.params[0]
                .reference_type()
                .is_some_and(|r| r.mutable)
            || !info.signature.params[1..]
                .iter()
                .all(|ty| crate::core::script::schema(ty).is_some())
        {
            return Err(type_error(&info.span, ErrorCode::InvalidCall,
                "#every requires a proved synchronous, host-free fn(&mut State[, Input]) -> Unit with bounded pure-value input"));
        }
    }
    Ok(())
}
