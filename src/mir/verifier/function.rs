//! Structural and type verification of the typed MIR.
//!
//! The verifier checks each reachable block of every function for
//! definition-before-use ordering, dominance of used values, type-correct
//! statements and terminators, and well-formed phi nodes. Call and method
//! call sites are validated against a signature map built from every function
//! in the program.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::diagnostic::Diagnostic;
use crate::mir::*;
use crate::sema::{type_name, Type, TypeTable};
use crate::syntax::UnaryOp;

#[path = "aggregates.rs"]
mod aggregates;
#[path = "borrows.rs"]
mod borrows;
#[path = "continuation_shapes.rs"]
mod continuation_shapes;
#[path = "continuations.rs"]
mod continuations;
#[path = "destruction.rs"]
mod destruction;
#[path = "effects.rs"]
mod effects;
#[path = "flow.rs"]
mod flow;
#[path = "handlers.rs"]
mod handlers;
#[path = "locals.rs"]
mod locals;
#[path = "ownership.rs"]
mod ownership;
#[path = "ownership_statements.rs"]
mod ownership_statements;
#[path = "phi.rs"]
mod phi;
#[path = "runtime.rs"]
mod runtime;
#[path = "sets.rs"]
mod sets;
use sets::IndexSet;

#[path = "shape.rs"]
mod shape;
#[path = "statements.rs"]
mod statement_checks;
#[path = "tasks.rs"]
mod tasks;
#[path = "terminators.rs"]
mod terminators;

#[path = "constants.rs"]
mod constants;
#[path = "helpers.rs"]
mod helpers;

use aggregates::{
    verify_construct_statement, verify_enum_construct_statement, verify_tuple_statement,
};
use continuation_shapes::{verify_abort_block_shapes, verify_suspending_continuation_shapes};
use continuations::verify_continuations;
use effects::{
    verify_handler_request, verify_resumable_request, verify_resume_statement,
    verify_suspend_statement,
};
use flow::prepare_flow;
use handlers::{verify_handler_capture_lifetimes, verify_handler_enter_statement};
use helpers::{
    check_definition, check_destination_type, check_owned_argument, check_value_exists,
    check_value_type, compute_dominators, consumed_statement_values, local_use_after_move,
    ownership_use_error, reachable_blocks, statement_destination, terminator_edges,
    terminator_targets, validate_call_arguments, verify_effect_argument_ownership,
};
pub(crate) use helpers::{statement_operands, MirSignature};
use locals::{verify_borrow_local_statement, verify_read_statement, verify_take_local_statement};
use ownership::{
    verify_aggregate_move_protocol, verify_cown_lease_lifetimes, verify_local_ownership_flow,
    verify_ownership_flow,
};
use ownership_statements::verify_ownership_statement;
use phi::{verify_phi_edges, verify_phi_statement};
use runtime::verify_runtime_call;
use shape::verify_function_shape;
use statement_checks::{
    verify_aggregate_and_store_statement, verify_basic_statement,
    verify_call_and_projection_statement, verify_effect_and_task_statement,
};
use tasks::{verify_task_result_reads, verify_task_scope_lifetimes, verify_task_structure};
use terminators::verify_terminator;

/// Verify an entire `MirProgram`. Returns the first error found, if any.
use constants::verify_constant_statement;
pub(crate) fn verify(program: &MirProgram) -> Result<(), Diagnostic> {
    crate::mir::suspending_analysis::verify_mutable_capture_suspension(program)?;
    let mut signatures: HashMap<MirFunctionId, MirSignature> = HashMap::new();
    let mut foreign_signatures = HashMap::new();
    for function in program.functions() {
        if let Some(foreign) = &function.foreign {
            let signature = (
                function.parameters.iter().map(|p| p.ty).collect::<Vec<_>>(),
                function.return_type,
            );
            if let Some(previous) =
                foreign_signatures.insert((&foreign.library, &foreign.symbol), signature.clone())
            {
                if previous != signature {
                    return Err(Diagnostic::codegen(format!(
                        "conflicting C declarations for '{}' in '{}'",
                        foreign.symbol, foreign.library
                    )));
                }
            }
        }
        let signature = MirSignature {
            receiver_ownership: function
                .receiver_local
                .map(|local| function.locals[local.0].ownership),
            parameter_ownership: function
                .parameters
                .iter()
                .map(|parameter| parameter.ownership)
                .collect(),
            parameter_types: function
                .parameters
                .iter()
                .map(|parameter| parameter.ty)
                .collect(),
            return_type: function.return_type,
            foreign: function.foreign.is_some(),
        };
        signatures.insert(function.id, signature);
    }
    for function in program.functions() {
        if matches!(
            function.name.as_str(),
            crate::sema::DEBUG_METHOD
                | crate::sema::HASH_METHOD
                | crate::sema::PARTIAL_EQ_METHOD
                | crate::sema::PARTIAL_ORD_METHOD
                | crate::sema::ORD_METHOD
        ) && function.receiver.is_some_and(|receiver| {
            program
                .types()
                .trait_implementations
                .get(match function.name.as_str() {
                    crate::sema::DEBUG_METHOD => "Debug",
                    crate::sema::HASH_METHOD => "Hash",
                    crate::sema::PARTIAL_EQ_METHOD => "PartialEq",
                    crate::sema::PARTIAL_ORD_METHOD => "PartialOrd",
                    _ => "Ord",
                })
                .is_some_and(|implementations| implementations.contains(&receiver))
        }) && function.is_suspending
        {
            return Err(Diagnostic::codegen(format!(
                "{} must be synchronous and cannot suspend",
                match function.name.as_str() {
                    crate::sema::DEBUG_METHOD => "Debug.debug",
                    crate::sema::HASH_METHOD => "Hash.hash",
                    crate::sema::PARTIAL_EQ_METHOD => "PartialEq.equals",
                    crate::sema::PARTIAL_ORD_METHOD => "PartialOrd.partial_compare",
                    _ => "Ord.compare",
                }
            )));
        }
    }
    for function in program.functions() {
        verify_function(function, program.types(), &signatures)?;
    }
    destruction::verify(program)?;
    crate::mir::regions::verify(program)?;
    Ok(())
}

pub(crate) fn verify_function(
    function: &MirFunction,
    types: &TypeTable,
    signatures: &HashMap<MirFunctionId, MirSignature>,
) -> Result<(), Diagnostic> {
    // Import stubs are link-time placeholders; they are never executed and are
    // stripped once external calls are rewritten to concrete definitions.
    if function.external_symbol.is_some() {
        return Ok(());
    }
    verify_function_shape(function, types)?;
    if let Some(foreign) = &function.foreign {
        if foreign.library.is_empty()
            || foreign.library.contains('\0')
            || foreign.symbol.is_empty()
            || foreign.symbol.contains('\0')
            || function.receiver.is_some()
            || function.is_suspending
            || function.is_task
            || function.parameters.iter().any(|p| {
                // A callback parameter carries the C-callable trampoline
                // address as a single opaque pointer word.
                let flat = matches!(p.ty, Type::Function(_));
                (!flat && !p.ty.is_c_abi_compatible()) || p.ownership != MirOwnership::Copy
            })
            || !(function.return_type.is_c_abi_compatible() || function.return_type == Type::Unit)
            || function.blocks.len() != 1
            || !function.blocks[0].statements.is_empty()
            || !matches!(
                function.blocks[0].terminator,
                Some(MirTerminator::Unreachable)
            )
        {
            return Err(Diagnostic::codegen(
                "invalid synchronous C FFI declaration in MIR",
            ));
        }
        // The adapter has no Joky task scope or body; only its declaration
        // participates in MIR call verification.
        return Ok(());
    }

    let flow = prepare_flow(function)?;
    let reachable = &flow.reachable;
    let available = &flow.available;
    let predecessors = &flow.predecessors;
    let doms = &flow.dominators;
    verify_task_structure(function, reachable, signatures)?;
    verify_task_scope_lifetimes(function, reachable)?;
    verify_task_result_reads(function, reachable, predecessors)?;
    let handler_enters = reachable
        .iter()
        .flat_map(|block_id| function.blocks[block_id.0].statements.iter())
        .filter(|statement| matches!(statement, MirStatement::HandlerEnter { .. }))
        .count();
    let handler_exits = reachable
        .iter()
        .flat_map(|block_id| function.blocks[block_id.0].statements.iter())
        .filter(|statement| matches!(statement, MirStatement::HandlerExit))
        .count();
    if handler_enters != handler_exits {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' has unbalanced HandlerEnter/HandlerExit",
            function.name
        )));
    }
    let mut handler_joins = HashSet::new();
    let mut handler_enter_blocks = HashSet::new();
    let mut handler_exit_blocks = HashSet::new();
    // Abort transfers control out of the current task. Keep this check in the
    // general function verifier so it also applies to functions without a
    // continuation table.
    for block in &function.blocks {
        for (index, statement) in block.statements.iter().enumerate() {
            if matches!(statement, MirStatement::TaskFailureClaim { .. }) {
                let next_is_scope_exit = block
                    .statements
                    .get(index + 1)
                    .is_some_and(|next| matches!(next, MirStatement::ScopeExit { .. }));
                if !next_is_scope_exit {
                    return Err(Diagnostic::codegen(format!(
                        "MIR block b{} must release its claimed task failure before continuing",
                        block.id.0
                    )));
                }
            }
        }
        if let Some(index) = block
            .statements
            .iter()
            .position(|statement| statement.control_protocol() == Some(MirControlProtocol::Abort))
        {
            let trailing_cleanup = block.statements[index + 1..].iter().all(|statement| {
                matches!(
                    statement,
                    MirStatement::Drop { .. }
                        | MirStatement::DropLocal { .. }
                        | MirStatement::Deinit { .. }
                        | MirStatement::ScopeExit { .. }
                        | MirStatement::HandlerExit
                )
            });
            if !trailing_cleanup || !matches!(block.terminator, Some(MirTerminator::Return(_))) {
                return Err(Diagnostic::codegen(format!(
                    "MIR block b{} must terminate immediately after an abort",
                    block.id.0
                )));
            }
        }
    }
    verify_continuations(function, types, &flow.dominators)?;
    verify_abort_block_shapes(function)?;
    for continuation in &function.continuations {
        verify_suspending_continuation_shapes(function, continuation, types)?;
    }

    for block_id in reachable {
        let block = &function.blocks[block_id.0];
        let mut saw_non_phi = false;
        let mut phi_count = 0;
        for statement in &block.statements {
            if verify_basic_statement(function, types, &flow.available[block_id.0], statement)? {
                saw_non_phi = true;
                continue;
            }
            if verify_call_and_projection_statement(
                function,
                types,
                &flow.available[block_id.0],
                signatures,
                statement,
            )? {
                saw_non_phi = true;
                continue;
            }
            if verify_effect_and_task_statement(
                function,
                types,
                &flow.available[block_id.0],
                *block_id,
                signatures,
                &mut handler_joins,
                &mut handler_enter_blocks,
                &mut handler_exit_blocks,
                statement,
            )? {
                saw_non_phi = true;
                continue;
            }
            if verify_aggregate_and_store_statement(
                function,
                types,
                &flow.available[block_id.0],
                signatures,
                statement,
            )? {
                saw_non_phi = true;
                continue;
            }
            match statement {
                MirStatement::Const { .. }
                | MirStatement::Unit { .. }
                | MirStatement::Read { .. }
                | MirStatement::BorrowLocal { .. }
                | MirStatement::TakeLocal { .. }
                | MirStatement::Unary { .. }
                | MirStatement::Binary { .. }
                | MirStatement::Numeric { .. } => {
                    unreachable!("basic MIR statement was handled above");
                }
                MirStatement::Call { .. }
                | MirStatement::FunctionValue { .. }
                | MirStatement::DynamicValue { .. }
                | MirStatement::DynamicUpcast { .. }
                | MirStatement::CallIndirect { .. }
                | MirStatement::Project { .. }
                | MirStatement::EnumTag { .. }
                | MirStatement::EnumProject { .. } => {
                    unreachable!("call/projection statement was handled above");
                }
                MirStatement::Tuple { .. }
                | MirStatement::EnumConstruct { .. }
                | MirStatement::Construct { .. }
                | MirStatement::Store { .. }
                | MirStatement::MethodCall { .. } => {
                    unreachable!("aggregate/store statement was handled above");
                }
                MirStatement::HandlerRequest { .. }
                | MirStatement::ResumableRequest { .. }
                | MirStatement::Suspend { .. }
                | MirStatement::TaskWait { .. }
                | MirStatement::CownAcquire { .. }
                | MirStatement::Resume { .. }
                | MirStatement::TaskPoll { .. }
                | MirStatement::TaskCancelled { .. }
                | MirStatement::TaskAbort { .. }
                | MirStatement::TaskFailureOperation { .. }
                | MirStatement::TaskFailurePayload { .. }
                | MirStatement::TaskFailureClaim { .. }
                | MirStatement::TaskFailureRethrow { .. }
                | MirStatement::HandlerEnter { .. }
                | MirStatement::HandlerExit
                | MirStatement::RuntimeCall { .. } => {
                    unreachable!("effect/task statement was handled above");
                }
                MirStatement::Dup { .. }
                | MirStatement::Move { .. }
                | MirStatement::Drop { .. }
                | MirStatement::Deinit { .. }
                | MirStatement::DropLocal { .. }
                | MirStatement::Bind { .. } => {
                    saw_non_phi = true;
                    verify_ownership_statement(function, types, &available[block_id.0], statement)?;
                }
                MirStatement::Phi { .. } => {
                    verify_phi_statement(
                        function,
                        available,
                        predecessors,
                        *block_id,
                        statement,
                        saw_non_phi,
                        &mut phi_count,
                    )?;
                }
                MirStatement::ScopeEnter { .. }
                | MirStatement::ScopeExit { .. }
                | MirStatement::TaskCreate { .. }
                | MirStatement::TaskJoin { .. }
                | MirStatement::TaskClaimResult { .. }
                | MirStatement::TaskCancel { .. }
                | MirStatement::RaceStart { .. }
                | MirStatement::RaceSelect { .. } => {
                    saw_non_phi = true;
                }
            }
        }
        verify_phi_edges(function, block, *block_id, phi_count, predecessors)?;
        verify_terminator(
            function,
            types,
            &available[block_id.0],
            block
                .terminator
                .as_ref()
                .expect("reachable block is terminated"),
        )?;
    }
    if handler_exit_blocks.iter().any(|block_id| {
        !handler_joins.contains(block_id)
            || !handler_enter_blocks
                .iter()
                .any(|enter| doms[block_id.0].contains(enter))
    }) {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' has a HandlerExit outside a handler join",
            function.name
        )));
    }
    verify_handler_capture_lifetimes(function, reachable, doms, predecessors)?;
    verify_cown_lease_lifetimes(function, reachable)?;
    verify_aggregate_move_protocol(function, types, reachable)?;
    verify_ownership_flow(function, &flow.reverse_postorder, predecessors)?;
    verify_local_ownership_flow(function, types, &flow.reverse_postorder, predecessors)?;
    crate::mir::local_ssa::verify_initialization(function)?;
    borrows::verify_borrow_lifetimes(function, &flow.reverse_postorder)?;
    Ok(())
}
