use super::constants::verify_resumable_constant;
use super::*;

pub(super) fn verify_handler_enter_statement(
    function: &MirFunction,
    types: &TypeTable,
    signatures: &HashMap<MirFunctionId, MirSignature>,
    handlers: &[MirHandlerArm],
    handler_joins: &mut HashSet<MirBlockId>,
) -> Result<(), Diagnostic> {
    let mut operations = HashSet::new();
    for handler in handlers {
        if !operations.insert(handler.operation) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' enters a duplicate handler operation",
                function.name
            )));
        }
        let operation = types
            .effects()
            .operation_info(handler.operation)
            .ok_or_else(|| Diagnostic::codegen("unknown MIR handler operation"))?;
        if !matches!(
            operation.mode,
            crate::sema::EffectMode::Normal
                | crate::sema::EffectMode::Aborts
                | crate::sema::EffectMode::Resumable
        ) {
            return Err(Diagnostic::codegen(
                "unsupported effect operation mode in MIR handler",
            ));
        }
        // `result_type` is the type of the enclosing `do` expression, not the
        // operation payload type. A Normal handler receives
        // `operation.return_type` as its parameter/result input, then its arm
        // value is allowed to become the surrounding `do` result.
        if handler.target.0 >= function.blocks.len()
            || handler.join.0 >= function.blocks.len()
            || handler.target == handler.join
        {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has an invalid handler target or join",
                function.name
            )));
        }
        if handler.parameter_locals.len() != handler.parameter_indices.len()
            || handler.parameter_locals.len() > operation.parameters.len()
        {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has mismatched handler parameter locals",
                function.name
            )));
        }
        let mut parameter_indices = HashSet::new();
        for (index, local) in handler
            .parameter_indices
            .iter()
            .zip(&handler.parameter_locals)
        {
            if *index >= operation.parameters.len() || !parameter_indices.insert(*index) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has invalid handler parameter index",
                    function.name
                )));
            }
            if local.0 >= function.locals.len() {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has an invalid handler parameter local",
                    function.name
                )));
            }
            let local_type = function.locals[local.0].ty;
            if local_type != operation.parameters[*index] {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has a handler parameter local type mismatch",
                    function.name
                )));
            }
        }
        if operation.mode == crate::sema::EffectMode::Resumable {
            let resumable_forms = usize::from(handler.resumable_value.is_some())
                + usize::from(handler.resumable_parameter.is_some())
                + usize::from(handler.resumable_transform.is_some())
                + usize::from(handler.resumable_function.is_some());
            if resumable_forms != 1
                || !handler.parameter_indices.is_empty()
                || !handler.parameter_locals.is_empty()
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has an invalid resumable handler arm",
                    function.name
                )));
            }
            if handler.result_type != operation.return_type {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has a resumable handler result type mismatch",
                    function.name
                )));
            }
            if let Some(parameter) = handler.resumable_parameter {
                let parameter_type = operation.parameters.get(parameter).ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "MIR function '{}' has an invalid resumable forwarding parameter",
                        function.name
                    ))
                })?;
                if ownership_for_type(*parameter_type, types) != MirOwnership::Copy {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' forwards a non-copyable resumable parameter",
                        function.name
                    )));
                }
                if *parameter_type != operation.return_type {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' forwards a parameter with the wrong result type",
                        function.name
                    )));
                }
            } else if let Some(transform) = handler.resumable_transform {
                let parameter_type =
                    operation
                        .parameters
                        .get(transform.parameter)
                        .ok_or_else(|| {
                            Diagnostic::codegen(format!(
                                "MIR function '{}' has an invalid resumable transform parameter",
                                function.name
                            ))
                        })?;
                if ownership_for_type(*parameter_type, types) != MirOwnership::Copy
                    || !parameter_type.is_integer()
                    || *parameter_type != operation.return_type
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has an invalid resumable scalar transform",
                        function.name
                    )));
                }
                if !matches!(
                    transform.operator,
                    crate::syntax::BinaryOp::Add
                        | crate::syntax::BinaryOp::Subtract
                        | crate::syntax::BinaryOp::Multiply
                ) {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has an unsupported resumable transform operator",
                        function.name
                    )));
                }
            } else if let Some(handler_function) = handler.resumable_function {
                for capture in &handler.resumable_captures {
                    let capture_type = function.value_types.get(capture.0).ok_or_else(|| {
                        Diagnostic::codegen(format!(
                            "MIR function '{}' has an invalid synthetic handler capture",
                            function.name
                        ))
                    })?;
                    if !matches!(
                        capture_type,
                        Type::I32 | Type::I64 | Type::F64 | Type::Bool | Type::Class(_)
                    ) && !types.is_shared(*capture_type)
                        && !types.is_owned(*capture_type)
                    {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' has an unsupported synthetic handler capture",
                            function.name
                        )));
                    }
                }
                let signature = signatures.get(&handler_function).ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "MIR function '{}' references an unknown synthetic handler function",
                        function.name
                    ))
                })?;
                let mut expected_parameters = operation.parameters.clone();
                expected_parameters.extend(
                    handler
                        .resumable_captures
                        .iter()
                        .map(|capture| function.value_types.get(capture.0).copied())
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            Diagnostic::codegen(format!(
                                "MIR function '{}' has an invalid normal handler capture",
                                function.name
                            ))
                        })?,
                );
                let valid_parameters = signature.parameter_types == expected_parameters;
                if !valid_parameters || signature.return_type != operation.return_type {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has a synthetic handler signature mismatch",
                        function.name
                    )));
                }
            } else {
                verify_resumable_constant(
                    handler
                        .resumable_value
                        .as_ref()
                        .expect("resumable value checked above"),
                    handler.result_type,
                    types,
                )?;
            }
            handler_joins.insert(handler.join);
            continue;
        }
        // A normal arm with a runtime payload is serviced by HandlerRequest
        // rather than by the legacy CFG Phi target. Its target/join remain
        // reserved metadata so the frame has stable identities.
        if handler.resumable_value.is_some()
            || handler.resumable_parameter.is_some()
            || handler.resumable_transform.is_some()
            || handler.resumable_function.is_some()
        {
            if !handler.parameter_indices.is_empty() || !handler.parameter_locals.is_empty() {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has static parameter bindings on a runtime handler arm",
                    function.name
                )));
            }
            if handler.result_type != operation.return_type {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has a normal handler result type mismatch",
                    function.name
                )));
            }
            if let Some(payload) = handler.resumable_value.as_ref() {
                verify_resumable_constant(payload, handler.result_type, types)?;
            } else if let Some(handler_function) = handler.resumable_function {
                let signature = signatures.get(&handler_function).ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "MIR function '{}' references an unknown normal handler function",
                        function.name
                    ))
                })?;
                let mut expected_parameters = operation.parameters.clone();
                expected_parameters.extend(
                    handler
                        .resumable_captures
                        .iter()
                        .map(|capture| function.value_types[capture.0]),
                );
                if signature.parameter_types != expected_parameters
                    || signature.return_type != operation.return_type
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has a normal handler signature mismatch",
                        function.name
                    )));
                }
            } else {
                return Err(Diagnostic::codegen(
                    "normal handler arm has no runtime payload or thunk",
                ));
            }
            handler_joins.insert(handler.join);
            continue;
        }
        if operation.mode == crate::sema::EffectMode::Normal {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has a legacy static Normal handler arm",
                function.name
            )));
        }
        let phi_parameters = function.blocks[handler.target.0]
            .statements
            .iter()
            .take_while(|statement| matches!(statement, MirStatement::Phi { .. }))
            .collect::<Vec<_>>();
        if phi_parameters.len() != operation.parameters.len() {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has mismatched handler parameter Phi count",
                function.name
            )));
        }
        for (index, statement) in phi_parameters.iter().enumerate() {
            let MirStatement::Phi { destination, .. } = statement else {
                unreachable!();
            };
            check_value_type(function, *destination, operation.parameters[index])?;
            if function.value_ownership[destination.0]
                != ownership_for_type(operation.parameters[index], types)
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has invalid handler parameter Phi ownership",
                    function.name
                )));
            }
        }
        for (index, local) in handler
            .parameter_indices
            .iter()
            .zip(&handler.parameter_locals)
        {
            let phi_destination = match phi_parameters[*index] {
                MirStatement::Phi { destination, .. } => destination,
                _ => unreachable!(),
            };
            let bound = function.blocks[handler.target.0]
                .statements
                .iter()
                .skip(phi_parameters.len())
                .any(|statement| {
                    matches!(
                        statement,
                        MirStatement::Bind {
                            local: bound_local,
                            value: Some(value),
                            ..
                        } if bound_local == local && *value == *phi_destination
                    )
                });
            if !bound {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' does not bind handler parameter local",
                    function.name
                )));
            }
        }
        handler_joins.insert(handler.join);
        let join_has_result = function.blocks[handler.join.0]
            .statements
            .iter()
            .any(|statement| {
                matches!(
                    statement,
                    MirStatement::Phi {
                        destination,
                        ..
                    } if function.value_types.get(destination.0) == Some(&handler.result_type)
                )
            });
        if !join_has_result {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has no handler result Phi at join bb{}",
                function.name, handler.join.0
            )));
        }
    }
    Ok(())
}

/// Check the lifetime contract of values captured by a generated handler
/// environment. Class captures are borrowed views of their lexical owner:
/// the owner must dominate the handler entry and remain initialized until the
/// matching HandlerExit. They must also never be forwarded as task payloads,
/// which would let a borrowed pointer outlive the lexical handler frame.
pub(super) fn verify_handler_capture_lifetimes(
    function: &MirFunction,
    reachable: &HashSet<MirBlockId>,
    dominators: &[IndexSet<MirBlockId>],
    predecessors: &[Vec<(MirBlockId, Vec<MirValueId>)>],
) -> Result<(), Diagnostic> {
    for enter_id in reachable {
        let Some(handlers) = function.blocks[enter_id.0]
            .statements
            .iter()
            .find_map(|statement| match statement {
                MirStatement::HandlerEnter { handlers } => Some(handlers),
                _ => None,
            })
        else {
            continue;
        };

        for handler in handlers {
            for capture in &handler.resumable_captures {
                let capture_type = *function.value_types.get(capture.0).ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "MIR function '{}' has an out-of-bounds handler capture",
                        function.name
                    ))
                })?;
                if function.value_ownership[capture.0] != MirOwnership::Borrowed {
                    continue;
                }
                if !matches!(capture_type, Type::Class(_)) {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' borrows a non-class handler capture",
                        function.name
                    )));
                }

                // Recover the lexical owner from the defining local read. A
                // borrowed capture must not be synthesized from an unrelated
                // value or from a definition that is only conditionally live.
                let owner = function.blocks.iter().find_map(|block| {
                    block
                        .statements
                        .iter()
                        .find_map(|statement| match statement {
                            MirStatement::Read { destination, local }
                            | MirStatement::BorrowLocal { destination, local }
                                if destination == capture =>
                            {
                                Some(*local)
                            }
                            _ => None,
                        })
                });
                let Some(owner) = owner else {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has a borrowed handler capture without an owner",
                        function.name
                    )));
                };
                let owner_local = function.locals.get(owner.0).ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "MIR function '{}' has an invalid handler capture owner",
                        function.name
                    ))
                })?;
                if !matches!(
                    owner_local.ownership,
                    MirOwnership::Owned | MirOwnership::Borrowed
                ) || !matches!(owner_local.ty, Type::Class(_))
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has an invalid borrowed class handler owner",
                        function.name
                    )));
                }

                let capture_definition = function
                    .blocks
                    .iter()
                    .find_map(|block| {
                        block
                            .statements
                            .iter()
                            .any(|statement| statement_destination(statement) == Some(*capture))
                            .then_some(block.id)
                    })
                    .ok_or_else(|| {
                        Diagnostic::codegen(format!(
                            "MIR function '{}' has an undefined handler capture",
                            function.name
                        ))
                    })?;
                if !dominators[enter_id.0].contains(&capture_definition) {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' handler capture does not dominate HandlerEnter",
                        function.name
                    )));
                }

                // Find every block that is both inside this handler's lexical
                // region and on a path to its join. Drops in this set would
                // invalidate the borrowed pointer before HandlerExit.
                let mut can_reach_join = HashSet::from([handler.join]);
                let mut pending = vec![handler.join];
                while let Some(block) = pending.pop() {
                    for (predecessor, _) in &predecessors[block.0] {
                        if reachable.contains(predecessor) && can_reach_join.insert(*predecessor) {
                            pending.push(*predecessor);
                        }
                    }
                }
                for block in function.blocks.iter().filter(|block| {
                    reachable.contains(&block.id)
                        && dominators[block.id.0].contains(enter_id)
                        && can_reach_join.contains(&block.id)
                }) {
                    let limit = if block.id == *enter_id {
                        block
                            .statements
                            .iter()
                            .position(|statement| {
                                matches!(statement, MirStatement::HandlerEnter { .. })
                            })
                            .map_or(0, |index| index + 1)
                    } else if block.id == handler.join {
                        block
                            .statements
                            .iter()
                            .position(|statement| matches!(statement, MirStatement::HandlerExit))
                            .unwrap_or(block.statements.len())
                    } else {
                        block.statements.len()
                    };
                    for statement in block.statements.iter().take(limit) {
                        if matches!(statement, MirStatement::TakeLocal { local, .. }
                            | MirStatement::DropLocal { local, .. } if *local == owner)
                        {
                            return Err(Diagnostic::codegen(format!(
                                "MIR function '{}' drops borrowed class owner before HandlerExit",
                                function.name
                            )));
                        }
                        if let MirStatement::TaskCreate { arguments, .. } = statement {
                            if arguments.iter().any(|argument| argument.value == *capture) {
                                return Err(Diagnostic::codegen(format!(
                                    "MIR function '{}' forwards borrowed handler capture to a task",
                                    function.name
                                )));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
