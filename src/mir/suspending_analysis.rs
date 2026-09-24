//! Suspending function analysis (Phase 2)
//!
//! Computes which functions may suspend, either directly (contain Suspend
//! statements) or transitively (call other suspending functions).
//!
//! ## Purpose
//!
//! The `is_suspending` flag marks functions that may suspend during execution.
//! This serves multiple purposes:
//!
//! 1. **Documentation**: Makes it explicit which functions can suspend
//! 2. **Static Analysis**: Enables verification that suspending functions are
//!    only called from appropriate structured contexts (effect handlers,
//!    task branches and race arms)
//! 3. **Future Optimizations**: Provides information for potential optimizations
//!    (e.g., skipping continuation checks in non-suspending functions)
//!
//! ## Implementation
//!
//! The analysis uses a fixed-point algorithm to propagate the suspending property
//! through the call graph:
//!
//! 1. **Direct marking**: Functions containing `Suspend` statements are marked
//!    during MIR lowering (these call `@suspends` effect operations)
//!
//! 2. **Transitive propagation**: If function A calls suspending function B,
//!    then A is also suspending. This propagates until no new functions are
//!    marked.
//!
//! The analysis also drives function-call continuation materialization. That
//! transformation is kept separate until Pending codegen can consume it.

use crate::mir::*;
use std::collections::{HashMap, HashSet};

/// Compute the `is_suspending` property for all functions using fixed-point
/// iteration.
///
/// A function is suspending if:
/// - It contains any `Suspend` statements (already marked during lowering), OR
/// - It calls another suspending function (direct call or method call)
///
/// ## Algorithm
///
/// 1. Initialize: collect the initial `is_suspending` values from all functions
///    (set during MIR lowering for functions containing `Suspend` statements)
///
/// 2. Iterate: for each function, scan all `Call` and `MethodCall` statements.
///    If any callee is suspending, mark the caller as suspending.
///
/// 3. Repeat step 2 until no new functions are marked (fixed point reached)
///
/// ## Complexity
///
/// - Time: O(F * S * I) where F = number of functions, S = average statements
///   per function, I = number of iterations (typically small, proportional to
///   the longest call chain depth)
/// - Space: O(F) for the suspending map
///
/// Returns a map from function ID to whether it is suspending.
pub(crate) fn compute_suspending_functions(
    functions: &[MirFunction],
) -> HashMap<MirFunctionId, bool> {
    let mut suspending: HashMap<MirFunctionId, bool> =
        functions.iter().map(|f| (f.id, f.is_suspending)).collect();

    // Fixed-point iteration: propagate suspending property through call graph
    let mut changed = true;
    while changed {
        changed = false;
        let pending_types = pending_closure_types(functions, &suspending);
        // Function values sharing a source signature must share a machine ABI,
        // including pure closures passed alongside closures with internal waits.
        for function in functions {
            for statement in function.blocks.iter().flat_map(|block| &block.statements) {
                if let MirStatement::FunctionValue {
                    destination,
                    function: target,
                    ..
                } = statement
                {
                    if pending_types.contains(&function.value_types[destination.0])
                        && !suspending[target]
                    {
                        suspending.insert(*target, true);
                        changed = true;
                    }
                }
            }
        }
        for function in functions {
            if suspending[&function.id] {
                continue; // Already marked as suspending
            }

            // Check if this function calls any suspending function
            for block in &function.blocks {
                for statement in &block.statements {
                    let calls_suspending = match statement {
                        MirStatement::Call {
                            function: callee, ..
                        } => suspending.get(callee).copied().unwrap_or(false),
                        MirStatement::MethodCall { method, .. } => {
                            suspending.get(method).copied().unwrap_or(false)
                        }
                        MirStatement::CallIndirect {
                            callee,
                            may_suspend,
                            ..
                        } => {
                            *may_suspend || pending_types.contains(&function.value_types[callee.0])
                        }
                        _ => false,
                    };

                    if calls_suspending {
                        suspending.insert(function.id, true);
                        changed = true;
                        break;
                    }
                }
                if suspending[&function.id] {
                    break;
                }
            }
        }
    }

    suspending
}

/// Track possible suspension separately from the shared Pending calling convention.
/// A pure closure can need that convention solely because another closure has
/// the same function type; borrowing its mutable environment is still safe.
pub(crate) fn compute_actual_suspending_functions(
    functions: &[MirFunction],
    types: &TypeTable,
) -> HashMap<MirFunctionId, bool> {
    let mut suspending = functions
        .iter()
        .map(|function| {
            let direct = (function.external_symbol.is_some() && function.is_suspending)
                || function.continuations.iter().any(|continuation| {
                    matches!(
                        continuation.kind,
                        MirContinuationKind::TaskWait | MirContinuationKind::CownAcquire
                    ) || (continuation.kind == MirContinuationKind::Suspending
                        && continuation.operation.is_some())
                })
                || function
                    .blocks
                    .iter()
                    .flat_map(|block| &block.statements)
                    .any(|statement| match statement {
                        MirStatement::Suspend { .. }
                        | MirStatement::TaskWait { .. }
                        | MirStatement::CownAcquire { .. } => true,
                        MirStatement::HandlerRequest { operation, .. }
                        | MirStatement::ResumableRequest { operation, .. } => types
                            .effects()
                            .operation_info(*operation)
                            .is_some_and(|operation| operation.suspends),
                        _ => false,
                    });
            (function.id, direct)
        })
        .collect::<HashMap<_, _>>();
    loop {
        let mut changed = false;
        let mut suspending_types = pending_closure_types(functions, &suspending);
        for function in functions {
            for statement in function.blocks.iter().flat_map(|block| &block.statements) {
                let MirStatement::DynamicValue {
                    destination,
                    methods,
                    ..
                } = statement
                else {
                    continue;
                };
                let Some(Type::Dyn(id)) = function.value_types.get(destination.0) else {
                    continue;
                };
                let Some(dynamic) = types.dynamic_types.get(*id) else {
                    continue;
                };
                for (target, method) in methods.iter().zip(&dynamic.methods) {
                    if suspending.get(target).copied().unwrap_or(false) {
                        suspending_types.insert(Type::Function(method.signature));
                    }
                }
            }
        }
        for function in functions {
            if suspending[&function.id] {
                continue;
            }
            let calls_suspending = function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .any(|statement| match statement {
                    MirStatement::Call {
                        function: callee, ..
                    }
                    | MirStatement::MethodCall { method: callee, .. } => {
                        suspending.get(callee).copied().unwrap_or(false)
                    }
                    MirStatement::HandlerEnter { handlers } => handlers.iter().any(|handler| {
                        handler
                            .resumable_function
                            .is_some_and(|callee| suspending.get(&callee).copied().unwrap_or(false))
                    }),
                    MirStatement::CallIndirect {
                        callee,
                        may_suspend,
                        ..
                    } => {
                        *may_suspend
                            || function
                                .value_types
                                .get(callee.0)
                                .is_some_and(|ty| suspending_types.contains(ty))
                    }
                    _ => false,
                });
            if calls_suspending {
                suspending.insert(function.id, true);
                changed = true;
            }
        }
        if !changed {
            return suspending;
        }
    }
}

pub(crate) fn verify_mutable_capture_suspension(program: &MirProgram) -> Result<(), Diagnostic> {
    if !program
        .functions
        .iter()
        .any(|function| function.source.mutable_capture.is_some())
    {
        return Ok(());
    }
    let suspending = compute_actual_suspending_functions(&program.functions, program.types());
    for function in &program.functions {
        if let Some(span) = function.source.mutable_capture {
            if suspending[&function.id] {
                return Err(Diagnostic::semantic(
                    "closures with mutable captures cannot suspend, including internal task waits",
                    span,
                ));
            }
        }
    }
    Ok(())
}

// Internal waits are introduced after source effect checking. They do not
// change a closure's public effects, but all values of its function type need
// the same calling convention at an indirect call site.
fn pending_closure_types(
    functions: &[MirFunction],
    suspending: &HashMap<MirFunctionId, bool>,
) -> HashSet<crate::sema::Type> {
    functions
        .iter()
        .flat_map(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .filter_map(|statement| {
                    if let MirStatement::FunctionValue {
                        destination,
                        function: target,
                        ..
                    } = statement
                    {
                        if suspending.get(target).copied().unwrap_or(false) {
                            function.value_types.get(destination.0).copied()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
        })
        .collect()
}

/// Materialize internal wait boundaries before consuming structured tasks.
/// Parallel waits once for the whole group, keeping joined values out of spills
/// until all TaskClaimResult statements have transferred their ownership.
pub(crate) fn materialize_task_waits(functions: &mut [MirFunction]) {
    for function in functions {
        let mut scopes = std::collections::HashSet::new();
        let mut consumers = std::collections::HashSet::new();
        let mut failure_checks = std::collections::HashSet::new();
        let mut materialized = std::collections::HashSet::new();
        for block in &function.blocks {
            for statement in &block.statements {
                match statement {
                    MirStatement::TaskWait { scope, .. } => {
                        materialized.insert(*scope);
                    }
                    MirStatement::TaskFailureOperation { scope, .. } => {
                        failure_checks.insert(*scope);
                    }
                    MirStatement::TaskCreate { scope, .. } => {
                        scopes.insert(*scope);
                    }
                    MirStatement::TaskJoin { scope, .. }
                    | MirStatement::RaceSelect { scope, .. } => {
                        consumers.insert(*scope);
                    }
                    _ => {}
                }
            }
        }
        let mut joins = std::collections::HashSet::new();
        let mut sites = Vec::new();
        for block in &function.blocks {
            for (index, statement) in block.statements.iter().enumerate() {
                let wait = match statement {
                    MirStatement::TaskJoin { scope, .. } if joins.insert(*scope) => {
                        Some((*scope, false))
                    }
                    MirStatement::TaskFailureOperation { scope, .. }
                        if !consumers.contains(scope) =>
                    {
                        Some((*scope, false))
                    }
                    MirStatement::RaceSelect { scope, .. } => Some((*scope, true)),
                    MirStatement::ScopeExit { scope }
                        if !block.statements.iter().any(|s| {
                            s.control_protocol() == Some(super::MirControlProtocol::Abort)
                        }) && scopes.contains(scope)
                            && !consumers.contains(scope)
                            && !failure_checks.contains(scope) =>
                    {
                        Some((*scope, false))
                    }
                    _ => None,
                };
                if let Some((scope, race)) = wait {
                    if !materialized.contains(&scope) {
                        sites.push((block.id, index, scope, race));
                    }
                }
            }
        }
        for (block_id, index, scope, race) in sites.into_iter().rev() {
            function.is_suspending = true;
            let id = MirContinuationId(function.continuations.len());
            let destination = MirValueId(function.value_types.len());
            function.value_types.push(crate::sema::Type::Unit);
            function.value_ownership.push(MirOwnership::Copy);
            let resume_block = MirBlockId(function.blocks.len());
            let block = &mut function.blocks[block_id.0];
            let mut tail = block.statements.split_off(index);
            tail.insert(0, MirStatement::Resume { continuation: id });
            block.statements.push(MirStatement::TaskWait {
                destination,
                scope,
                race,
                continuation: id,
            });
            let resume = MirBlock {
                id: resume_block,
                scoped: block.scoped,
                scope_depth: block.scope_depth,
                statements: tail,
                terminator: block.terminator.take(),
            };
            block.terminator = Some(MirTerminator::Goto {
                target: resume_block,
                arguments: Vec::new(),
            });
            function.blocks.push(resume);
            for block in &mut function.blocks {
                for statement in &mut block.statements {
                    if let MirStatement::Phi { incoming, .. } = statement {
                        for (predecessor, _) in incoming {
                            if *predecessor == block_id {
                                *predecessor = resume_block;
                            }
                        }
                    }
                }
            }
            for continuation in &mut function.continuations {
                if continuation.suspend_block == block_id {
                    continuation.suspend_block = resume_block;
                }
                // A pre-existing resume marker only moves when it belongs to the tail.
                if continuation.resume_block == block_id && function.blocks[resume_block.0].statements.iter().any(|s| matches!(s, MirStatement::Resume { continuation: other } if *other == continuation.id)) {
                    continuation.resume_block = resume_block;
                }
            }
            function.continuations.push(MirContinuation {
                id,
                kind: MirContinuationKind::TaskWait,
                operation: None,
                callee: None,
                suspend_block: block_id,
                resume_block,
                resume_destination: Some(destination),
                generation: 0,
                locals_before_suspend: function.locals.len(),
                spill_values: Vec::new(),
                spill_slots: Vec::new(),
                frame_slots: Vec::new(),
            });
        }
        crate::mir::lower::finalize_function_continuation_spills(function);
    }
}

/// Update all functions' `is_suspending` field based on the computed result.
pub(crate) fn propagate_suspending_property(
    functions: &mut [MirFunction],
    suspending_map: &HashMap<MirFunctionId, bool>,
) {
    let pending_types = pending_closure_types(functions, suspending_map);
    for function in functions {
        for statement in function
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.statements)
        {
            if let MirStatement::CallIndirect {
                callee,
                may_suspend,
                ..
            } = statement
            {
                *may_suspend |= pending_types.contains(&function.value_types[callee.0]);
            }
        }
        if let Some(&is_suspending) = suspending_map.get(&function.id) {
            function.is_suspending = is_suspending;
        }
    }
}

/// Materialize a continuation boundary at each statically known suspending
/// direct call, including task bodies and functions reachable from tasks.
/// Pending codegen must be available before enabling this in the pipeline.
pub(crate) fn materialize_direct_call_continuations(
    functions: &mut [MirFunction],
    suspending: &HashMap<MirFunctionId, bool>,
) {
    // Recursive calls intentionally use the same per-invocation continuation
    // allocation as every other suspending direct call. The MIR continuation
    // id is a template; runtime handles, frames, and spills are allocated for
    // each invocation, so recursive suspension does not reuse a caller frame.
    for function in functions {
        let mut block_index = 0;
        // Splitting appends the tail; visiting appended blocks also handles
        // several suspending calls in the same original block.
        while block_index < function.blocks.len() {
            let call = function.blocks[block_index]
                .statements
                .iter()
                .enumerate()
                .find_map(|(index, statement)| {
                    let (callee, destination, continuation, may_suspend) = match statement {
                        MirStatement::Call {
                            function: callee,
                            continuation,
                            destination,
                            ..
                        }
                        | MirStatement::MethodCall {
                            method: callee,
                            continuation,
                            destination,
                            ..
                        } => (Some(*callee), *destination, continuation, false),
                        MirStatement::CallIndirect {
                            destination,
                            may_suspend,
                            continuation,
                            ..
                        } => (None, *destination, continuation, *may_suspend),
                        _ => return None,
                    };
                    (continuation.is_none()
                        && (may_suspend
                            || callee
                                .and_then(|id| suspending.get(&id).copied())
                                .unwrap_or(false)))
                    .then_some((index, callee, destination))
                });
            if let Some((statement_index, callee, destination)) = call {
                let id = MirContinuationId(function.continuations.len());
                let resume_block = MirBlockId(function.blocks.len());
                let block = &mut function.blocks[block_index];
                match &mut block.statements[statement_index] {
                    MirStatement::Call { continuation, .. }
                    | MirStatement::MethodCall { continuation, .. }
                    | MirStatement::CallIndirect { continuation, .. } => *continuation = Some(id),
                    _ => unreachable!(),
                }
                let mut tail = block.statements.split_off(statement_index + 1);
                tail.insert(0, MirStatement::Resume { continuation: id });
                let resume = MirBlock {
                    id: resume_block,
                    scoped: block.scoped,
                    scope_depth: block.scope_depth,
                    statements: tail,
                    terminator: block.terminator.take(),
                };
                block.terminator = Some(MirTerminator::Goto {
                    target: resume_block,
                    arguments: Vec::new(),
                });
                function.blocks.push(resume);
                // The old outgoing edges now originate at the appended tail.
                // Phi inputs are keyed by predecessor, including loop edges.
                for block in &mut function.blocks {
                    for statement in &mut block.statements {
                        if let MirStatement::Phi { incoming, .. } = statement {
                            for (predecessor, _) in incoming {
                                if *predecessor == MirBlockId(block_index) {
                                    *predecessor = resume_block;
                                }
                            }
                        }
                    }
                }
                // Effect requests already lowered in the tail move with it.
                for statement in &function.blocks[resume_block.0].statements {
                    match statement {
                        MirStatement::Suspend { continuation, .. }
                        | MirStatement::TaskWait { continuation, .. }
                        | MirStatement::CownAcquire { continuation, .. }
                        | MirStatement::ResumableRequest { continuation, .. }
                        | MirStatement::HandlerRequest { continuation, .. } => {
                            function.continuations[continuation.0].suspend_block = resume_block;
                        }
                        MirStatement::Resume { continuation } if *continuation != id => {
                            function.continuations[continuation.0].resume_block = resume_block;
                        }
                        _ => {}
                    }
                }
                function.continuations.push(MirContinuation {
                    id,
                    kind: MirContinuationKind::Suspending,
                    operation: None,
                    callee,
                    suspend_block: MirBlockId(block_index),
                    resume_block,
                    resume_destination: Some(destination),
                    generation: 0,
                    locals_before_suspend: function.locals.len(),
                    spill_values: Vec::new(),
                    spill_slots: Vec::new(),
                    frame_slots: Vec::new(),
                });
            }
            block_index += 1;
        }
        crate::mir::lower::finalize_function_continuation_spills(function);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hir::CoreProgram, sema, syntax};

    fn lower(source: &str) -> MirProgram {
        let program = syntax::parse_program(source).unwrap();
        let types = sema::check_program(&program).unwrap();
        let core = CoreProgram::lower(program, types).unwrap();
        MirProgram::lower(&core).unwrap()
    }

    #[test]
    fn direct_suspend_marks_function_as_suspending() {
        let source = "eff Timer { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn wait() -> Unit effects { Timer } { Timer.sleep(1ms) }\n\
             fn main() effects { Timer } { wait() }";
        let mir = lower(source);
        let wait_fn = mir
            .functions()
            .iter()
            .find(|f| f.name == "wait")
            .expect("wait function");
        assert!(
            wait_fn.is_suspending,
            "Function with Suspend statement should be marked as suspending"
        );
    }

    #[test]
    fn calling_suspending_function_propagates() {
        let source = "eff Timer { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn wait() -> Unit effects { Timer } { Timer.sleep(1ms) }\n\
             fn call_wait() -> Unit effects { Timer } { wait() }\n\
             fn main() effects { Timer } { call_wait() }";
        let mir = lower(source);

        let wait_fn = mir
            .functions()
            .iter()
            .find(|f| f.name == "wait")
            .expect("wait function");
        assert!(wait_fn.is_suspending, "wait() should be suspending");

        let call_wait_fn = mir
            .functions()
            .iter()
            .find(|f| f.name == "call_wait")
            .expect("call_wait function");
        assert!(
            call_wait_fn.is_suspending,
            "call_wait() should be suspending because it calls wait()"
        );
    }

    #[test]
    fn non_suspending_function_not_marked() {
        let source = "fn add(a: Int32, b: Int32) -> Int32 { a + b }\n\
             fn main() { let x = add(1, 2); println(x) }";
        let mir = lower(source);
        let add_fn = mir
            .functions()
            .iter()
            .find(|f| f.name == "add")
            .expect("add function");
        assert!(
            !add_fn.is_suspending,
            "Pure function should not be marked as suspending"
        );
    }

    fn materialize(source: &str) -> MirProgram {
        let mut mir = lower(source);
        let suspending = compute_suspending_functions(&mir.functions);
        materialize_direct_call_continuations(&mut mir.functions, &suspending);
        mir.verify()
            .unwrap_or_else(|error| panic!("{error:?}\n{}", mir.dump()));
        mir
    }

    const TIMER: &str = "eff Timer { @suspends fn sleep(duration: Duration) -> Unit }\n\
        fn wait() -> Int32 effects { Timer } { Timer.sleep(1ms); 20 }\n";

    #[test]
    fn call_boundaries_split_multiple_calls_and_preserve_scalar_spills() {
        let mut mir = materialize(&format!(
            "{TIMER}\n\
            fn main() effects {{ Timer }} {{ println(wait() + wait()) }}"
        ));
        let main = mir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(main.continuations.len(), 2);
        let first = &main.continuations[0];
        let second = &main.continuations[1];
        assert_ne!(first.suspend_block, first.resume_block);
        assert_ne!(second.suspend_block, second.resume_block);
        assert!(second
            .spill_values
            .contains(&first.resume_destination.unwrap()));
        let before = mir.dump();
        let suspending = compute_suspending_functions(&mir.functions);
        materialize_direct_call_continuations(&mut mir.functions, &suspending);
        assert_eq!(mir.dump(), before, "materialization must be idempotent");
    }

    #[test]
    fn call_boundaries_update_effect_requests_and_phi_predecessors() {
        let mir = materialize(&format!(
            "{TIMER}\n\
            fn choose(flag: Bool) -> Int32 effects {{ Timer }} {{\n\
                let answer = if flag {{ wait() }} else {{ wait() + 2 }};\n\
                Timer.sleep(1ms); answer\n\
            }}\n\
            fn main() effects {{ Timer }} {{ println(choose(true)) }}"
        ));
        let choose = mir
            .functions
            .iter()
            .find(|function| function.name == "choose")
            .unwrap();
        assert_eq!(choose.continuations.len(), 3);
        assert_eq!(
            choose
                .continuations
                .iter()
                .filter(|item| item.callee.is_some())
                .count(),
            2
        );
    }

    #[test]
    fn call_boundaries_preserve_local_frames_and_loops() {
        let mir = materialize(&format!(
            "{TIMER}\n\
            fn repeat(n: Int32) -> String effects {{ Timer }} {{\n\
                let label = \"done\";\n\
                while n > 0 {{ println(wait() + n); break }}; label\n\
            }}\n\
            fn main() effects {{ Timer }} {{ println(repeat(2)) }}"
        ));
        let repeat = mir
            .functions
            .iter()
            .find(|function| function.name == "repeat")
            .unwrap();
        let call = repeat
            .continuations
            .iter()
            .find(|item| item.callee.is_some())
            .unwrap();
        assert!(call
            .frame_slots
            .iter()
            .any(|slot| repeat.locals[slot.local.0].name == "label"));
    }

    #[test]
    fn call_boundaries_include_task_reachable_functions() {
        let mir = materialize(&format!(
            "{TIMER}\n\
            fn nested() -> Int32 effects {{ Timer }} {{ wait() + 1 }}\n\
            fn main() effects {{ Timer }} {{\n\
                let result = parallel {{ | nested() }}; println(result.0)\n\
            }}"
        ));
        let nested = mir
            .functions
            .iter()
            .find(|function| function.name == "nested")
            .unwrap();
        assert!(nested
            .continuations
            .iter()
            .any(|item| item.callee.is_some()));
    }

    #[test]
    fn verifier_rejects_inline_call_boundaries_and_missing_spill_slots() {
        let mut mir = materialize(&format!(
            "{TIMER}\n\
            fn main() effects {{ Timer }} {{ println(wait() + wait()) }}"
        ));
        let main = mir
            .functions
            .iter_mut()
            .find(|function| function.name == "main")
            .unwrap();
        main.continuations[1].spill_slots.clear();
        assert!(mir.verify().is_err());
        let main = mir
            .functions
            .iter_mut()
            .find(|function| function.name == "main")
            .unwrap();
        crate::mir::lower::finalize_function_continuation_spills(main);
        main.continuations[0].resume_block = main.continuations[0].suspend_block;
        assert!(mir.verify().is_err());
    }

    #[test]
    fn method_call_boundaries_preserve_receiver_and_result() {
        let mir = materialize(&format!(
            "{TIMER}\n\
            struct Counter {{ let value: Int32 = 22;\n\
                fn answer() -> Int32 effects {{ Timer }} {{ wait() + self.value }}\n\
            }}\n\
            fn main() effects {{ Timer }} {{ println(Counter().answer()) }}"
        ));
        assert!(mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.statements)
            .any(|statement| matches!(
                statement,
                MirStatement::MethodCall {
                    continuation: Some(_),
                    ..
                }
            )));
    }

    #[test]
    fn normal_effect_requests_do_not_color_functions_as_suspending() {
        let mir = lower(
            "eff Value { fn get() -> Int32 }\n\
            fn value() -> Int32 effects { Value } { Value.get() }\n\
            fn main() effects { Value } { println(value()) }",
        );
        assert!(mir.functions.iter().all(|function| !function.is_suspending));
    }

    #[test]
    fn mutable_capture_check_distinguishes_pending_abi_from_actual_waits() {
        let mut mir = lower(
            r#"
            fn invoke(callback: fn() -> Int32) -> Int32 { callback() }
            fn main() {
                let pure = fn() -> Int32 { 42 }
                let waiting = fn() -> Int32 {
                    let values = parallel {
                        | 1
                        | 2
                    }
                    values.0
                }
                println(invoke(pure) + invoke(waiting))
            }
            "#,
        );
        let closures = mir
            .functions
            .iter()
            .filter(|function| function.name.starts_with("__closure_"))
            .collect::<Vec<_>>();
        assert_eq!(closures.len(), 2);
        assert!(closures.iter().all(|function| function.is_suspending));
        let pure = closures
            .iter()
            .find(|function| {
                !function.blocks.iter().any(|block| {
                    block
                        .statements
                        .iter()
                        .any(|statement| matches!(statement, MirStatement::TaskWait { .. }))
                })
            })
            .unwrap()
            .id;
        let waiting = closures
            .iter()
            .find(|function| function.id != pure)
            .unwrap()
            .id;
        let invoke = mir
            .functions
            .iter()
            .find(|function| function.name == "invoke")
            .unwrap()
            .id;

        // Target analysis must also work before the Pending pass colors calls.
        for function in &mut mir.functions {
            for statement in function
                .blocks
                .iter_mut()
                .flat_map(|block| &mut block.statements)
            {
                if let MirStatement::CallIndirect { may_suspend, .. } = statement {
                    *may_suspend = false;
                }
            }
        }
        let actual = compute_actual_suspending_functions(&mir.functions, mir.types());
        assert!(!actual[&pure]);
        assert!(actual[&waiting]);
        assert!(actual[&invoke]);

        let span = crate::Span::new(1, 2);
        mir.functions
            .iter_mut()
            .find(|function| function.id == pure)
            .unwrap()
            .source
            .mutable_capture = Some(span);
        verify_mutable_capture_suspension(&mir).unwrap();
        let pending = compute_suspending_functions(&mir.functions);
        propagate_suspending_property(&mut mir.functions, &pending);
        mir.verify().unwrap();
        mir.functions
            .iter_mut()
            .find(|function| function.id == waiting)
            .unwrap()
            .source
            .mutable_capture = Some(span);
        let error = mir.verify().unwrap_err();
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic);
        assert_eq!(error.span(), Some(span));
        assert!(error.to_string().contains("internal task waits"));
    }

    #[test]
    fn actual_suspension_propagates_through_methods_and_direct_calls() {
        let mut mir = lower(
            r#"
            eff Timer { @suspends fn sleep(duration: Duration) -> Unit }
            struct Worker {
                fn work() -> Int32 effects { Timer } {
                    Timer.sleep(1ms)
                    7
                }
            }
            fn relay() -> Int32 effects { Timer } { Worker().work() }
            fn main() effects { Timer } { println(relay()) }
            "#,
        );
        let actual = compute_actual_suspending_functions(&mir.functions, mir.types());
        assert!(mir.functions.iter().all(|function| actual[&function.id]));
        let span = crate::Span::new(3, 5);
        mir.functions
            .iter_mut()
            .find(|function| function.name == "relay")
            .unwrap()
            .source
            .mutable_capture = Some(span);
        let error = mir.verify().unwrap_err();
        assert_eq!(error.span(), Some(span));
        assert!(error
            .to_string()
            .contains("mutable captures cannot suspend"));
    }

    #[test]
    fn actual_suspension_ignores_pure_direct_call_continuations() {
        let mut mir = lower(
            r#"
            fn invoke(callback: fn() -> Int32) -> Int32 { callback() }
            fn direct() -> Int32 {
                let callback = fn() -> Int32 { 42 }
                callback()
            }
            fn main() {
                let waiting = fn() -> Int32 {
                    let values = parallel { | 1 }
                    values.0
                }
                println(direct() + invoke(waiting))
            }
            "#,
        );
        let direct = mir
            .functions
            .iter_mut()
            .find(|function| function.name == "direct")
            .unwrap();
        assert!(direct.is_suspending);
        assert!(direct
            .continuations
            .iter()
            .any(MirContinuation::is_function_call));
        direct.source.mutable_capture = Some(crate::Span::new(1, 2));
        let id = direct.id;
        let actual = compute_actual_suspending_functions(&mir.functions, mir.types());
        assert!(!actual[&id]);
        mir.verify().unwrap();
    }

    #[test]
    fn actual_suspension_uses_pending_fact_only_for_external_stubs() {
        let mut mir = lower("fn external() -> Int32 { 1 } fn main() { println(external()) }");
        let external = mir
            .functions
            .iter_mut()
            .find(|function| function.name == "external")
            .unwrap();
        external.is_suspending = true;
        let id = external.id;
        let actual = compute_actual_suspending_functions(&mir.functions, mir.types());
        assert!(!actual[&id]);

        mir.functions
            .iter_mut()
            .find(|function| function.id == id)
            .unwrap()
            .external_symbol = Some(crate::module::SymbolId {
            module: crate::module::StableId(1),
            name: "external".into(),
        });
        let actual = compute_actual_suspending_functions(&mir.functions, mir.types());
        assert!(mir.functions.iter().all(|function| actual[&function.id]));
    }
}
