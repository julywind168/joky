//! Import stub generation and external call lowering.

use std::collections::{HashMap, HashSet};

use crate::module::{symbol_key, SymbolId};
use crate::sema::CheckedTypes;
use crate::syntax::Visibility;

use super::*;
use crate::hir::{CoreCallArgument, CoreExpr};

pub(super) fn collect_external_symbols(types: &CheckedTypes) -> Vec<SymbolId> {
    let mut symbols = types
        .external_symbols()
        .values()
        .chain(types.interface.generic_symbols.values())
        .chain(types.interface.method_symbols.values())
        .cloned()
        .collect::<Vec<_>>();
    symbols.sort();
    symbols.dedup();
    symbols
}

pub(super) fn register_external_function_ids(
    symbols: &[SymbolId],
    function_ids: &mut HashMap<String, MirFunctionId>,
    starting_id: usize,
) -> usize {
    let mut next_id = starting_id;
    for symbol in symbols {
        let key = symbol_key(symbol);
        if function_ids.contains_key(&key) {
            continue;
        }
        function_ids.insert(key.clone(), MirFunctionId(next_id));
        next_id += 1;
    }
    next_id
}

pub(super) fn build_external_stub(
    id: MirFunctionId,
    symbol: &SymbolId,
    signature: &crate::sema::ExternalFunction,
    types: &CheckedTypes,
) -> Result<MirFunction, Diagnostic> {
    let parameters = &signature.parameters;
    let return_type = signature.return_type;
    let mut locals = Vec::new();
    let mir_parameters = parameters
        .iter()
        .enumerate()
        .map(|(index, (name, ty))| {
            let ownership = if signature.parameter_borrows[index] && types.is_owned(*ty) {
                MirOwnership::Borrowed
            } else {
                ownership_for_type(*ty, types)
            };
            let local_id = MirLocalId(index);
            locals.push(MirLocal {
                id: local_id,
                name: name.clone(),
                ty: *ty,
                ownership,
                scope_depth: 0,
            });
            MirParameter {
                local: local_id,
                name: name.clone(),
                ty: *ty,
                ownership,
            }
        })
        .collect::<Vec<_>>();
    let entry = MirBlockId(0);
    Ok(MirFunction {
        source: MirFunctionSource::default(),
        declared_effects: Default::default(),
        foreign: None,
        id,
        module: MirModuleId(0),
        name: symbol_key(symbol),
        external_symbol: Some(symbol.clone()),
        imported_region_contract: signature.region_contract.clone(),
        visibility: Visibility::Private,
        receiver: None,
        parameters: mir_parameters,
        receiver_local: None,
        locals,
        return_type,
        is_task: false,
        is_suspending: signature.suspends,
        entry,
        blocks: vec![MirBlock {
            id: entry,
            scoped: false,
            scope_depth: 0,
            statements: Vec::new(),
            terminator: Some(MirTerminator::Unreachable),
        }],
        value_types: Vec::new(),
        value_ownership: Vec::new(),
        continuations: Vec::new(),
    })
}

impl Lowerer<'_> {
    pub(super) fn lower_external_call(
        &mut self,
        expression: &CoreExpr,
        symbol: &SymbolId,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let function = *self
            .function_ids
            .get(&symbol_key(symbol))
            .ok_or_else(|| Diagnostic::codegen("external import stub was not registered"))?;
        let parameter_names = self
            .function_parameters
            .get(&function)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut used_parameters = vec![false; parameter_names.len()];
        let mut next_positional = 0;
        let mut mir_arguments = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let parameter = resolve_argument_index(
                argument.label.as_deref(),
                parameter_names,
                &mut used_parameters,
                &mut next_positional,
            )?;
            let borrowed = types
                .external_signature(symbol)
                .is_some_and(|signature| signature.parameter_borrows[parameter]);
            let value = if borrowed {
                self.lower_borrowed_value(&argument.value, function_names, types)?
            } else {
                self.lower_value(&argument.value, function_names, types)?
            };
            let Some(value) = value else {
                return Ok(None);
            };
            mir_arguments.push(MirCallArgument { parameter, value });
        }
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::Call {
            destination,
            function,
            arguments: mir_arguments,
            continuation: None,
        });
        let destination = self.lower_task_poll_result(destination)?;
        Ok(Some(destination))
    }
}
