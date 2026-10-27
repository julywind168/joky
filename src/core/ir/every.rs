//! Periodic declarations lower to ordinary named timer operations.
use super::*;
use crate::parser::ast::Param;
use std::collections::HashSet;

pub(super) fn entry(name: &str) -> String {
    format!("$every/{}:{name}", name.len())
}

pub(super) fn expand(module: &mut AstModule) -> Result<(), Error> {
    let states: HashSet<_> = module
        .records
        .iter()
        .filter(|r| r.attributes.iter().any(|a| a.path == "state"))
        .map(|r| r.name.as_str())
        .collect();
    let methods: HashSet<_> = module
        .implementations
        .iter()
        .filter(|i| i.trait_name.is_none() && states.contains(i.target_name.as_str()))
        .flat_map(|i| {
            i.methods
                .iter()
                .map(move |m| format!("{}.{}", i.target_name, m.name))
        })
        .collect();
    let mut entries = Vec::new();
    for item in &mut module.items {
        let Item::FnDef(function) = item else {
            continue;
        };
        if function.every.is_none() {
            continue;
        }
        if !methods.contains(&function.name)
            || !function.type_params.is_empty()
            || function.suspends
            || function.ret_ty != Type::Unit
            || function.params.len() != 1
            || function.params[0].name != "self"
            || !function.params[0]
                .ty
                .reference_type()
                .is_some_and(|r| r.mutable)
        {
            return Err(type_error(
                &function.span,
                ErrorCode::InvalidCall,
                "#every requires a nongeneric synchronous fn(&mut self) -> Unit on a #state type",
            ));
        }
        // Only the hidden adapter carries scheduling metadata. The ordinary
        // method remains directly callable without registering another timer.
        let mut adapter = function.clone();
        function.every = None;
        adapter.name = entry(&function.name);
        adapter.public = false;
        adapter.params.push(Param {
            mutable: false,
            name: "$every.input".into(),
            ty: Type::Unit,
            span: function.span,
        });
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
            || info.signature.params.len() != 2
            || !info.signature.params[0]
                .reference_type()
                .is_some_and(|r| r.mutable)
            || info.signature.params[1] != Type::Unit
        {
            return Err(type_error(
                &info.span,
                ErrorCode::InvalidCall,
                "#every requires a proved synchronous, host-free state method",
            ));
        }
    }
    Ok(())
}
