use super::*;

pub(super) fn verify_phi_statement(
    function: &MirFunction,
    available: &[IndexSet<MirValueId>],
    predecessors: &[Vec<(MirBlockId, Vec<MirValueId>)>],
    block_id: MirBlockId,
    statement: &MirStatement,
    saw_non_phi: bool,
    phi_count: &mut usize,
) -> Result<(), Diagnostic> {
    let MirStatement::Phi {
        destination,
        incoming,
    } = statement
    else {
        return Err(Diagnostic::codegen(
            "non-Phi statement passed to Phi verifier",
        ));
    };
    if saw_non_phi {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' places Phi after a non-Phi statement",
            function.name
        )));
    }
    *phi_count += 1;
    let destination_type = check_value_exists(function, *destination)?;
    if incoming.is_empty() {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' has an empty Phi",
            function.name
        )));
    }
    let mut incoming_blocks = HashSet::new();
    for (incoming_block, value) in incoming {
        if !incoming_blocks.insert(*incoming_block)
            || !predecessors[block_id.0]
                .iter()
                .any(|(predecessor, _)| predecessor == incoming_block)
        {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has an invalid Phi predecessor bb{} -> bb{}",
                function.name, incoming_block.0, block_id.0
            )));
        }
        check_definition(&available[incoming_block.0], *value, function)?;
        check_value_type(function, *value, destination_type)?;
    }
    Ok(())
}

pub(super) fn verify_phi_edges(
    function: &MirFunction,
    block: &MirBlock,
    block_id: MirBlockId,
    phi_count: usize,
    predecessors: &[Vec<(MirBlockId, Vec<MirValueId>)>],
) -> Result<(), Diagnostic> {
    if phi_count > 0 {
        if predecessors[block_id.0].is_empty()
            || predecessors[block_id.0]
                .iter()
                .any(|(_, arguments)| arguments.len() != phi_count)
            || block.statements[..phi_count].iter().any(|statement| {
                matches!(statement, MirStatement::Phi { incoming, .. } if incoming.len() != predecessors[block_id.0].len())
            })
        {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has mismatched block arguments for bb{}",
                function.name, block_id.0
            )));
        }
        for (phi_index, statement) in block.statements.iter().take(phi_count).enumerate() {
            let MirStatement::Phi {
                destination,
                incoming,
            } = statement
            else {
                unreachable!("the first MIR statements are Phi nodes");
            };
            let destination_type = check_value_exists(function, *destination)?;
            for (incoming_block, incoming_value) in incoming {
                let arguments = predecessors[block_id.0]
                    .iter()
                    .find_map(|(predecessor, arguments)| {
                        (predecessor == incoming_block).then_some(arguments)
                    })
                    .expect("Phi predecessors were checked above");
                if arguments[phi_index] != *incoming_value {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has a Phi edge/value mismatch in bb{}",
                        function.name, block_id.0
                    )));
                }
                check_value_type(function, arguments[phi_index], destination_type)?;
            }
        }
    } else if predecessors[block_id.0]
        .iter()
        .any(|(_, arguments)| !arguments.is_empty())
    {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' passes arguments to a block without Phi parameters",
            function.name
        )));
    }
    Ok(())
}
