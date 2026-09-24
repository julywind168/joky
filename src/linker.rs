//! Merge independently compiled module artifacts into one linkable MIR program.

use std::collections::HashMap;

use crate::mir::{MirFunction, MirFunctionId, MirProgram, MirStatement, MirTerminator};
use crate::module::{ExportedSymbol, ModuleArtifact, ResolvedSymbol, StableId, SymbolId};

pub struct Linker {
    artifacts: Vec<ModuleArtifact>,
}

impl Linker {
    pub fn new(artifacts: Vec<ModuleArtifact>) -> Self {
        Self { artifacts }
    }

    pub fn link(mut self) -> Result<LinkedProgram, String> {
        self.validate_abi()?;
        let mir = self.merge_mir()?;
        Ok(LinkedProgram {
            artifacts: self.artifacts,
            mir,
        })
    }

    fn validate_abi(&self) -> Result<(), String> {
        if self.artifacts.is_empty() {
            return Err("cannot link an empty artifact set".into());
        }
        let abi_map: HashMap<StableId, u64> = self
            .artifacts
            .iter()
            .map(|artifact| (artifact.metadata.stable_id, artifact.metadata.abi_hash))
            .collect();
        if abi_map.len() != self.artifacts.len() {
            return Err("duplicate module identity in artifact set".into());
        }

        for artifact in &self.artifacts {
            for (dep_id, expected_hash) in &artifact.metadata.dependencies {
                match abi_map.get(dep_id) {
                    None => {
                        return Err(format!(
                            "missing dependency {:016x} required by module {:016x}",
                            dep_id.0, artifact.metadata.stable_id.0
                        ))
                    }
                    Some(actual_hash) if *actual_hash != *expected_hash => {
                        return Err(format!(
                            "ABI hash mismatch for dependency {:016x}: \
                             expected {:016x}, got {:016x}",
                            dep_id.0, expected_hash, actual_hash
                        ))
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    fn merge_mir(&mut self) -> Result<MirProgram, String> {
        let mut global_exports: HashMap<SymbolId, ResolvedSymbol> = HashMap::new();
        let mut merged_functions = Vec::new();
        let mut types = crate::sema::TypeTable::default();
        let mut relocations = Vec::new();
        let mut next_id = 0;

        for artifact in &mut self.artifacts {
            let type_map = types.merge_module(&artifact.mir.types, artifact.metadata.stable_id)?;
            let mut local_map = HashMap::<MirFunctionId, MirFunctionId>::new();
            let mut stub_map = HashMap::<MirFunctionId, SymbolId>::new();
            let functions = &artifact.mir.functions;

            for function in functions {
                if let Some(symbol) = &function.external_symbol {
                    stub_map.insert(function.id, symbol.clone());
                }
            }

            // Allocate every local ID before rewriting any body. Function
            // declarations are order-independent, so a function may call a
            // later declaration in the same artifact.
            for function in functions {
                if function.external_symbol.is_none() {
                    let new_id = MirFunctionId(next_id);
                    next_id += 1;
                    local_map.insert(function.id, new_id);
                }
            }

            for (name, exported) in &artifact.exports {
                if let ExportedSymbol::Function { mir_index, .. } = exported {
                    let symbol = SymbolId {
                        module: artifact.metadata.stable_id,
                        name: name.clone(),
                    };
                    let old_id = MirFunctionId(*mir_index);
                    let global_id = *local_map.get(&old_id).ok_or_else(|| {
                        format!(
                            "export '{name}' in module {:016x} does not resolve to a local MIR function",
                            artifact.metadata.stable_id.0
                        )
                    })?;
                    global_exports.insert(
                        symbol,
                        ResolvedSymbol {
                            mir_index: global_id.0,
                        },
                    );
                }
            }
            relocations.push((type_map, local_map, stub_map));
        }

        let mut expected_signatures = Vec::new();
        for (artifact, (type_map, local_map, stub_map)) in
            self.artifacts.iter_mut().zip(relocations)
        {
            for mut function in std::mem::take(&mut artifact.mir.functions) {
                crate::linker_types::remap(&mut function, &type_map);
                if let Some(symbol) = &function.external_symbol {
                    let id = global_exports
                        .get(symbol)
                        .ok_or_else(|| format!("unresolved external symbol {}", symbol.name))?
                        .mir_index;
                    expected_signatures.push((id, function));
                    continue;
                }
                function.id = local_map[&function.id];
                remap_function(&mut function, &local_map, &stub_map, &global_exports)?;
                merged_functions.push(function);
            }
        }
        for (id, expected) in expected_signatures {
            let actual = &merged_functions[id];
            if expected.return_type != actual.return_type
                || expected
                    .parameters
                    .iter()
                    .map(|p| (p.ty, p.ownership))
                    .ne(actual.parameters.iter().map(|p| (p.ty, p.ownership)))
                || expected.is_suspending != actual.is_suspending
            {
                return Err(format!("import ABI mismatch for '{}'", expected.name));
            }
        }

        for artifact in &self.artifacts {
            for import in &artifact.imports {
                if !global_exports.contains_key(import) {
                    return Err(format!(
                        "unresolved import {:016x}/{}",
                        import.module.0, import.name
                    ));
                }
            }
        }

        // A function type's Pending ABI is a whole-program fact: a closure with
        // an internal wait in one module changes every indirect call of that
        // type, including call sites compiled in modules that never saw it.
        let suspending =
            crate::mir::suspending_analysis::compute_suspending_functions(&merged_functions);
        crate::mir::suspending_analysis::propagate_suspending_property(
            &mut merged_functions,
            &suspending,
        );
        crate::mir::suspending_analysis::materialize_direct_call_continuations(
            &mut merged_functions,
            &suspending,
        );

        let program = MirProgram {
            functions: merged_functions,
            types,
        };
        program
            .verify()
            .map_err(|error| format!("invalid linked MIR: {error}"))?;
        Ok(program)
    }
}

pub struct LinkedProgram {
    pub artifacts: Vec<ModuleArtifact>,
    pub mir: MirProgram,
}

fn remap_function(
    function: &mut MirFunction,
    local_map: &HashMap<MirFunctionId, MirFunctionId>,
    stub_map: &HashMap<MirFunctionId, SymbolId>,
    global_exports: &HashMap<SymbolId, ResolvedSymbol>,
) -> Result<(), String> {
    for continuation in &mut function.continuations {
        if let Some(callee) = continuation.callee {
            continuation.callee = Some(remap_function_id(
                callee,
                local_map,
                stub_map,
                global_exports,
            )?);
        }
    }

    for block in &mut function.blocks {
        for statement in &mut block.statements {
            remap_statement(statement, local_map, stub_map, global_exports)?;
        }
        if let Some(terminator) = &mut block.terminator {
            remap_terminator(terminator, local_map, stub_map, global_exports)?;
        }
    }
    Ok(())
}

fn remap_statement(
    statement: &mut MirStatement,
    local_map: &HashMap<MirFunctionId, MirFunctionId>,
    stub_map: &HashMap<MirFunctionId, SymbolId>,
    global_exports: &HashMap<SymbolId, ResolvedSymbol>,
) -> Result<(), String> {
    match statement {
        MirStatement::DynamicValue { methods, .. } => {
            for method in methods {
                *method = remap_function_id(*method, local_map, stub_map, global_exports)?;
            }
        }
        MirStatement::Call { function, .. }
        | MirStatement::FunctionValue { function, .. }
        | MirStatement::MethodCall {
            method: function, ..
        }
        | MirStatement::TaskCreate { function, .. } => {
            *function = remap_function_id(*function, local_map, stub_map, global_exports)?;
        }
        MirStatement::HandlerEnter { handlers } => {
            for handler in handlers {
                if let Some(function) = handler.resumable_function {
                    handler.resumable_function = Some(remap_function_id(
                        function,
                        local_map,
                        stub_map,
                        global_exports,
                    )?);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn remap_terminator(
    _terminator: &mut MirTerminator,
    _local_map: &HashMap<MirFunctionId, MirFunctionId>,
    _stub_map: &HashMap<MirFunctionId, SymbolId>,
    _global_exports: &HashMap<SymbolId, ResolvedSymbol>,
) -> Result<(), String> {
    Ok(())
}

fn remap_function_id(
    id: MirFunctionId,
    local_map: &HashMap<MirFunctionId, MirFunctionId>,
    stub_map: &HashMap<MirFunctionId, SymbolId>,
    global_exports: &HashMap<SymbolId, ResolvedSymbol>,
) -> Result<MirFunctionId, String> {
    if let Some(symbol) = stub_map.get(&id) {
        let resolved = global_exports.get(symbol).ok_or_else(|| {
            format!(
                "unresolved external symbol {:016x}/{}",
                symbol.module.0, symbol.name
            )
        })?;
        return Ok(MirFunctionId(resolved.mir_index));
    }
    local_map
        .get(&id)
        .copied()
        .ok_or_else(|| format!("MIR function id {} was not remapped during linking", id.0))
}
