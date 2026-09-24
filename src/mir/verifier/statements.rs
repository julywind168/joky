use std::collections::{HashMap, HashSet};

use super::*;

/// Shared Abort-protocol check: every abortive statement must reference an
/// operation declared with the `Aborts` effect mode. Returns the validated
/// operation info so callers do not re-query the table.
pub(super) fn verify_abort_operation<'a>(
    types: &'a TypeTable,
    operation: crate::sema::EffectOperationId,
    label: &str,
) -> Result<&'a crate::sema::EffectOperation, Diagnostic> {
    let info = types
        .effects()
        .operation_info(operation)
        .ok_or_else(|| Diagnostic::codegen(format!("unknown MIR {label} operation")))?;
    if info.mode != crate::sema::EffectMode::Aborts {
        return Err(Diagnostic::codegen(format!(
            "MIR {label} requires an Aborts effect operation"
        )));
    }
    Ok(info)
}

pub(super) fn verify_basic_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    statement: &MirStatement,
) -> Result<bool, Diagnostic> {
    match statement {
        MirStatement::Const { destination, value } => {
            verify_constant_statement(function, *destination, value)?;
        }
        MirStatement::Unit { destination } => {
            check_destination_type(function, *destination, Type::Unit)?;
        }
        MirStatement::Read { destination, local } => {
            verify_read_statement(function, *destination, *local)?;
        }
        MirStatement::BorrowLocal { destination, local } => {
            verify_borrow_local_statement(function, types, *destination, *local)?;
        }
        MirStatement::TakeLocal { destination, local } => {
            verify_take_local_statement(function, *destination, *local)?;
        }
        MirStatement::Unary {
            destination,
            op,
            operand,
        } => {
            let destination_type = check_value_exists(function, *destination)?;
            check_definition(available, *operand, function)?;
            let operand_type = check_value_exists(function, *operand)?;
            let valid = match op {
                UnaryOp::Negate => operand_type.is_numeric() && destination_type == operand_type,
                UnaryOp::Not => operand_type == Type::Bool && destination_type == Type::Bool,
                UnaryOp::BitNot => operand_type.is_integer() && destination_type == operand_type,
            };
            if !valid {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has invalid unary operand types",
                    function.name
                )));
            }
        }
        MirStatement::Binary {
            destination,
            op,
            left,
            right,
        } => {
            let destination_type = check_value_exists(function, *destination)?;
            check_definition(available, *left, function)?;
            check_definition(available, *right, function)?;
            let left_type = check_value_exists(function, *left)?;
            let right_type = check_value_exists(function, *right)?;
            let valid = if op.is_comparison() {
                destination_type == Type::Bool
                    && left_type == right_type
                    && (left_type.is_numeric() || left_type == Type::Bool)
            } else if op.is_bitwise() {
                destination_type == left_type && left_type == right_type && left_type.is_integer()
            } else {
                destination_type == left_type && left_type == right_type && left_type.is_numeric()
            };
            if !valid {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has invalid binary operand types",
                    function.name
                )));
            }
        }
        MirStatement::Numeric {
            destination,
            method,
            arguments,
        } => {
            let destination_type = check_value_exists(function, *destination)?;
            for argument in arguments {
                check_definition(available, *argument, function)?;
            }
            let valid = match method {
                crate::mir::NumericMethod::Abs => {
                    arguments.len() == 1
                        && check_value_exists(function, arguments[0])?.is_numeric()
                        && destination_type == check_value_exists(function, arguments[0])?
                }
                crate::mir::NumericMethod::Min | crate::mir::NumericMethod::Max => {
                    arguments.len() == 2
                        && check_value_exists(function, arguments[0])?.is_numeric()
                        && check_value_exists(function, arguments[0])?
                            == check_value_exists(function, arguments[1])?
                        && destination_type == check_value_exists(function, arguments[0])?
                }
                crate::mir::NumericMethod::To => {
                    arguments.len() == 1
                        && check_value_exists(function, arguments[0])?.is_numeric()
                        && destination_type.is_numeric()
                }
            };
            if !valid {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has invalid numeric method types",
                    function.name
                )));
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub(super) fn verify_call_and_projection_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    signatures: &HashMap<MirFunctionId, MirSignature>,
    statement: &MirStatement,
) -> Result<bool, Diagnostic> {
    match statement {
        MirStatement::Call {
            destination,
            function: function_name,
            arguments,
            continuation: _,
        } => {
            let signature = signatures.get(function_name).ok_or_else(|| {
                Diagnostic::codegen(format!(
                    "MIR function '{}' calls unknown function '{}'",
                    function.name, function_name.0
                ))
            })?;
            validate_call_arguments(
                function,
                &format!("function#{}", function_name.0),
                arguments,
                signature,
                available,
                types,
            )?;
            check_destination_type(function, *destination, signature.return_type)?;
        }
        MirStatement::DynamicUpcast { destination, value } => {
            check_definition(available, *value, function)?;
            let (Type::Dyn(source), Type::Dyn(target)) = (
                check_value_exists(function, *value)?,
                check_value_exists(function, *destination)?,
            ) else {
                return Err(Diagnostic::codegen("dynamic upcast requires dynamic types"));
            };
            if function.value_ownership[value.0] != MirOwnership::Owned
                || function.value_ownership[destination.0] != MirOwnership::Owned
            {
                return Err(Diagnostic::codegen(
                    "cannot upcast a non-owned dynamic value",
                ));
            }
            types.dynamic_types[source]
                .upcast_slots(&types.dynamic_types[target], |id| types.function_type(id))
                .map_err(Diagnostic::codegen)?;
        }
        MirStatement::DynamicValue {
            destination,
            value,
            methods,
        } => {
            check_definition(available, *value, function)?;
            let Type::Dyn(id) = check_value_exists(function, *destination)? else {
                return Err(Diagnostic::codegen("dynamic value has non-dynamic type"));
            };
            let info = &types.dynamic_types[id];
            if info.methods.len() != methods.len() || methods.is_empty() {
                return Err(Diagnostic::codegen("invalid dynamic vtable"));
            }
            if function.value_ownership[value.0] == MirOwnership::Borrowed {
                return Err(Diagnostic::codegen("cannot move a borrowed value into Dyn"));
            }
            for (method, target) in info.methods.iter().zip(methods) {
                let target = signatures
                    .get(target)
                    .ok_or_else(|| Diagnostic::codegen("unknown dynamic method wrapper"))?;
                let signature = types.function_type(method.signature);
                let expected_receiver = if method.receiver == crate::syntax::ReceiverMode::Borrowed
                {
                    MirOwnership::Borrowed
                } else {
                    ownership_for_type(function.value_types[value.0], types)
                };
                if target.parameter_types.first() != Some(&function.value_types[value.0])
                    || target.parameter_types[1..] != signature.parameters
                    || target.return_type != signature.return_type
                    || target.parameter_ownership.first() != Some(&expected_receiver)
                {
                    return Err(Diagnostic::codegen("invalid dynamic method signature"));
                }
            }
        }
        MirStatement::FunctionValue {
            destination,
            function: target,
            captures,
        } => {
            let Type::Function(signature_id) = check_value_exists(function, *destination)? else {
                return Err(Diagnostic::codegen(
                    "MIR function value has non-function type",
                ));
            };
            let target_signature = signatures.get(target).ok_or_else(|| {
                Diagnostic::codegen("MIR function value targets an unknown function")
            })?;
            let closure_signature = types.function_type(signature_id);
            if target_signature.parameter_types.len() < captures.len()
                || target_signature.parameter_types[captures.len()..]
                    != closure_signature.parameters
                || target_signature.return_type != closure_signature.return_type
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' has an incompatible closure target",
                    function.name
                )));
            }
            for (index, capture) in captures.iter().enumerate() {
                check_definition(available, *capture, function)?;
                if function.value_ownership[capture.0] == MirOwnership::Borrowed {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' captures a borrowed value in an escaping closure",
                        function.name
                    )));
                }
                let capture_type = check_value_exists(function, *capture)?;
                if capture_type != target_signature.parameter_types[index] {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has a closure capture type mismatch",
                        function.name
                    )));
                }
            }
        }
        MirStatement::CallIndirect {
            destination,
            callee,
            arguments,
            ..
        } => {
            check_definition(available, *callee, function)?;
            let Type::Function(signature_id) = check_value_exists(function, *callee)? else {
                return Err(Diagnostic::codegen("MIR indirect callee is not a function"));
            };
            let info = types.function_type(signature_id);
            let parameter_ownership = function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .find_map(|statement| {
                    let MirStatement::Project {
                        destination,
                        base,
                        access: MirFieldAccess::Index(slot),
                    } = statement
                    else {
                        return None;
                    };
                    if destination != callee {
                        return None;
                    }
                    let Type::Dyn(id) = function.value_types[base.0] else {
                        return None;
                    };
                    Some(
                        types.dynamic_types[id].methods[*slot]
                            .parameter_borrows
                            .iter()
                            .zip(&info.parameters)
                            .map(|(borrowed, ty)| {
                                if *borrowed && types.is_owned(*ty) {
                                    MirOwnership::Borrowed
                                } else {
                                    ownership_for_type(*ty, types)
                                }
                            })
                            .collect(),
                    )
                })
                .unwrap_or_default();
            let signature = MirSignature {
                foreign: false,
                receiver_ownership: None,
                parameter_ownership,
                parameter_types: info.parameters.clone(),
                return_type: info.return_type,
            };
            validate_call_arguments(
                function,
                "indirect function",
                arguments,
                &signature,
                available,
                types,
            )?;
            check_destination_type(function, *destination, signature.return_type)?;
        }
        MirStatement::Project {
            destination,
            base,
            access,
        } => {
            check_value_exists(function, *destination)?;
            check_definition(available, *base, function)?;
            let base_type = check_value_exists(function, *base)?;
            let field_type = match (base_type, access) {
                (Type::Dyn(id), MirFieldAccess::Index(slot)) => {
                    let method = types.dynamic_types[id]
                        .methods
                        .get(*slot)
                        .ok_or_else(|| Diagnostic::codegen("dynamic method slot out of bounds"))?;
                    if function.value_ownership[destination.0] != MirOwnership::Borrowed {
                        return Err(Diagnostic::codegen(
                            "dynamic method projection must borrow its receiver",
                        ));
                    }
                    Type::Function(method.signature)
                }
                (Type::Tuple(id), MirFieldAccess::Index(index)) => {
                    *types.tuple_elements(id).get(*index).ok_or_else(|| {
                        Diagnostic::codegen(format!(
                            "MIR function '{}' projects tuple index {} out of bounds",
                            function.name, index
                        ))
                    })?
                }
                (Type::Struct(id), MirFieldAccess::Index(index)) => types
                    .struct_fields(id)
                    .get(*index)
                    .map(|(_, ty)| *ty)
                    .ok_or_else(|| Diagnostic::codegen("MIR struct field index out of bounds"))?,
                (Type::Class(id), MirFieldAccess::Index(index)) => types
                    .class_fields(id)
                    .get(*index)
                    .map(|(_, ty)| *ty)
                    .ok_or_else(|| Diagnostic::codegen("MIR class field index out of bounds"))?,
                _ => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' has an invalid field projection on type '{}'",
                        function.name,
                        type_name(base_type)
                    )))
                }
            };
            check_destination_type(function, *destination, field_type)?;
            if types.is_owned(field_type) {
                let base_ownership = function.value_ownership[base.0];
                let destination_ownership = function.value_ownership[destination.0];
                let valid = match base_ownership {
                    MirOwnership::Borrowed => destination_ownership == MirOwnership::Borrowed,
                    MirOwnership::Owned => {
                        matches!(
                            destination_ownership,
                            MirOwnership::Owned | MirOwnership::Borrowed
                        )
                    }
                    MirOwnership::Copy | MirOwnership::Shared => false,
                };
                if !valid {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' projects an owned field with invalid ownership",
                        function.name
                    )));
                }
            }
        }
        MirStatement::EnumTag { destination, value } => {
            check_destination_type(function, *destination, Type::I32)?;
            check_definition(available, *value, function)?;
            if !matches!(
                check_value_exists(function, *value)?,
                Type::Enum(_) | Type::Option(_) | Type::Result(_)
            ) {
                return Err(Diagnostic::codegen(format!(
                    "MIR enum tag reads a non-enum value in function '{}'",
                    function.name
                )));
            }
        }
        MirStatement::EnumProject {
            destination,
            value,
            variant,
            field,
        } => {
            check_value_exists(function, *destination)?;
            check_definition(available, *value, function)?;
            let value_type = check_value_exists(function, *value)?;
            let fields = match value_type {
                Type::Enum(enum_id) => types
                    .enum_variants(enum_id)
                    .get(*variant)
                    .map(|variant| variant.fields.as_slice()),
                Type::Option(option_id) if *variant == 0 => {
                    Some(&[("value".to_owned(), types.option_type(option_id))][..])
                }
                Type::Option(_) => Some(&[] as &[(String, Type)]),
                Type::Result(result_id) => {
                    let (ok, err) = types.result_types(result_id);
                    match *variant {
                        0 => Some(&[("value".to_owned(), ok)][..]),
                        1 => Some(&[("value".to_owned(), err)][..]),
                        _ => None,
                    }
                }
                _ => None,
            };
            let Some(fields) = fields else {
                return Err(Diagnostic::codegen(format!(
                    "MIR enum projection reads a non-enum value in function '{}'",
                    function.name
                )));
            };
            let field_type = fields
                .get(*field)
                .map(|(_, field_type)| *field_type)
                .ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "MIR function '{}' projects enum field index {} out of bounds in variant {}",
                        function.name, field, variant
                    ))
                })?;
            check_destination_type(function, *destination, field_type)?;
            if types.is_owned(field_type) {
                let source_ownership = function.value_ownership[value.0];
                let destination_ownership = function.value_ownership[destination.0];
                let valid = match source_ownership {
                    MirOwnership::Borrowed => destination_ownership == MirOwnership::Borrowed,
                    MirOwnership::Owned => {
                        matches!(
                            destination_ownership,
                            MirOwnership::Owned | MirOwnership::Borrowed
                        )
                    }
                    MirOwnership::Copy | MirOwnership::Shared => false,
                };
                if !valid {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' projects an owned enum field with invalid ownership",
                        function.name
                    )));
                }
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Effect/task verification maintains separate handler join, entry and exit sets alongside statement context."
)]
pub(super) fn verify_effect_and_task_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    block_id: MirBlockId,
    signatures: &HashMap<MirFunctionId, MirSignature>,
    handler_joins: &mut HashSet<MirBlockId>,
    handler_enter_blocks: &mut HashSet<MirBlockId>,
    handler_exit_blocks: &mut HashSet<MirBlockId>,
    statement: &MirStatement,
) -> Result<bool, Diagnostic> {
    match statement {
        MirStatement::HandlerRequest {
            destination,
            operation,
            continuation,
            arguments,
        } => {
            debug_assert_eq!(
                statement.request_continuation_kind(),
                Some(MirContinuationKind::Normal)
            );
            verify_handler_request(
                function,
                types,
                available,
                *destination,
                *operation,
                *continuation,
                arguments,
            )?;
        }
        MirStatement::ResumableRequest {
            destination,
            operation,
            continuation,
            arguments,
        } => {
            debug_assert_eq!(
                statement.request_continuation_kind(),
                Some(MirContinuationKind::Resumable)
            );
            verify_resumable_request(
                function,
                types,
                available,
                *destination,
                *operation,
                *continuation,
                arguments,
            )?;
        }
        MirStatement::CownAcquire {
            destination,
            arguments,
            ..
        } => {
            check_destination_type(function, *destination, Type::Unit)?;
            if arguments.is_empty()
                || arguments.iter().any(|arg| {
                    !matches!(function.value_types.get(arg.value.0), Some(Type::Cown(_)))
                })
            {
                return Err(Diagnostic::codegen(
                    "Cown acquisition requires Cown handles",
                ));
            }
        }
        MirStatement::TaskWait { destination, .. } => {
            check_destination_type(function, *destination, Type::Unit)?;
        }
        MirStatement::Suspend {
            destination,
            operation,
            continuation,
            arguments,
        } => {
            debug_assert_eq!(
                statement.request_continuation_kind(),
                Some(MirContinuationKind::Suspending)
            );
            verify_suspend_statement(
                function,
                types,
                available,
                block_id,
                *destination,
                *operation,
                *continuation,
                arguments,
            )?;
        }
        MirStatement::Resume { continuation } => {
            debug_assert_eq!(
                statement.control_protocol(),
                Some(MirControlProtocol::Resume)
            );
            verify_resume_statement(function, block_id, *continuation)?;
        }
        MirStatement::TaskPoll { destination } => {
            check_destination_type(function, *destination, Type::Bool)?;
        }
        MirStatement::TaskCancelled { destination } => {
            check_destination_type(function, *destination, function.return_type)?;
        }
        MirStatement::TaskAbort {
            destination,
            operation,
            arguments,
        } => {
            debug_assert_eq!(
                statement.control_protocol(),
                Some(MirControlProtocol::Abort)
            );
            let info = verify_abort_operation(types, *operation, "TaskAbort")?;
            let signature = MirSignature {
                foreign: false,
                receiver_ownership: None,
                parameter_ownership: Vec::new(),
                parameter_types: info.parameters.clone(),
                return_type: info.return_type,
            };
            let call_arguments = arguments
                .iter()
                .enumerate()
                .map(|(parameter, value)| MirCallArgument {
                    parameter,
                    value: *value,
                })
                .collect::<Vec<_>>();
            validate_call_arguments(
                function,
                "non-resumable effect",
                &call_arguments,
                &signature,
                available,
                types,
            )?;
            check_destination_type(function, *destination, function.return_type)?;
        }
        MirStatement::TaskFailureOperation { destination, scope } => {
            check_destination_type(function, *destination, Type::U64)?;
            let _ = scope;
        }
        MirStatement::TaskFailurePayload {
            destination,
            operation,
            parameter,
            ..
        } => {
            let operation = verify_abort_operation(types, *operation, "TaskFailurePayload")?;
            let parameter_type = operation
                .parameters
                .get(*parameter)
                .copied()
                .ok_or_else(|| Diagnostic::codegen("invalid task failure payload parameter"))?;
            check_destination_type(function, *destination, parameter_type)?;
        }
        MirStatement::TaskFailureClaim { .. } => {}
        MirStatement::TaskFailureRethrow { destination, .. } => {
            check_destination_type(function, *destination, function.return_type)?;
        }
        MirStatement::HandlerEnter { handlers } => {
            handler_enter_blocks.insert(block_id);
            verify_handler_enter_statement(function, types, signatures, handlers, handler_joins)?;
        }
        MirStatement::HandlerExit => {
            handler_exit_blocks.insert(block_id);
        }
        MirStatement::RuntimeCall {
            destination,
            arguments,
            intrinsic,
        } => {
            verify_runtime_call(
                function,
                types,
                available,
                *destination,
                arguments,
                intrinsic,
            )?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub(super) fn verify_aggregate_and_store_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    signatures: &HashMap<MirFunctionId, MirSignature>,
    statement: &MirStatement,
) -> Result<bool, Diagnostic> {
    match statement {
        MirStatement::Tuple {
            destination,
            elements,
        } => {
            verify_tuple_statement(function, types, available, *destination, elements)?;
        }
        MirStatement::EnumConstruct {
            destination,
            variant,
            enum_id,
            arguments,
        } => {
            verify_enum_construct_statement(
                function,
                types,
                available,
                *destination,
                *variant,
                *enum_id,
                arguments,
            )?;
        }
        MirStatement::Construct {
            destination,
            fields,
            type_id,
        } => {
            verify_construct_statement(function, types, available, *destination, fields, *type_id)?;
        }
        MirStatement::Store {
            destination,
            receiver,
            access,
            value,
        } => {
            check_destination_type(function, *destination, Type::Unit)?;
            check_definition(available, *receiver, function)?;
            check_definition(available, *value, function)?;
            if !matches!(check_value_exists(function, *receiver)?, Type::Class(_))
                || !matches!(access, MirFieldAccess::Index(_))
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR store target is not a class field in function '{}'",
                    function.name
                )));
            }
            if function.value_ownership[receiver.0] != MirOwnership::Borrowed {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' stores through a consuming class reference",
                    function.name
                )));
            }
            let Type::Class(class_id) = check_value_exists(function, *receiver)? else {
                unreachable!("class receiver checked above");
            };
            let MirFieldAccess::Index(index) = access;
            if *index >= types.class_fields(class_id).len() {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' stores to an out-of-bounds class field",
                    function.name
                )));
            }
            let field_type = types.class_fields(class_id)[*index].1;
            check_value_type(function, *value, field_type)?;
            if types.is_owned(field_type)
                && function.value_ownership[value.0] != MirOwnership::Owned
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' stores a borrowed value into an owning field",
                    function.name
                )));
            }
        }
        MirStatement::MethodCall {
            destination,
            receiver,
            arguments,
            receiver_type,
            method,
            continuation: _,
        } => {
            check_value_exists(function, *destination)?;
            check_definition(available, *receiver, function)?;
            if !matches!(receiver_type, Type::Class(_) | Type::Struct(_))
                || check_value_exists(function, *receiver)? != *receiver_type
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR method receiver type is invalid in function '{}'",
                    function.name
                )));
            }
            let expected_receiver_ownership = signatures
                .get(method)
                .and_then(|signature| signature.receiver_ownership)
                .ok_or_else(|| Diagnostic::codegen("MIR method has no receiver signature"))?;
            if function.value_ownership[receiver.0] != expected_receiver_ownership {
                return Err(Diagnostic::codegen(format!(
                    "MIR method receiver has invalid ownership in function '{}'",
                    function.name
                )));
            }
            let signature = signatures.get(method).ok_or_else(|| {
                Diagnostic::codegen(format!(
                    "MIR function '{}' calls unknown method id {} on '{}'",
                    function.name,
                    method.0,
                    type_name(*receiver_type)
                ))
            })?;
            validate_call_arguments(
                function,
                &format!("method#{}", method.0),
                arguments,
                signature,
                available,
                types,
            )?;
            check_destination_type(function, *destination, signature.return_type)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}
