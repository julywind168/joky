use super::*;

#[test]
fn verifier_rejects_machine_entry_unrepresentable_return_types() {
    // D2: the machine-entry ABI cannot represent type-parameter, Self,
    // or associated return types, so a suspending continuation with such a
    // return type must be rejected with an actionable diagnostic.
    for return_type in [Type::SelfType, Type::Param(usize::MAX)] {
        let mut mir = lower(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn capable() -> Int32 effects { time } { time.sleep(1ms); 42 }\n\
             fn main() -> Int32 effects { time } { capable() }",
        );
        let function = mir
            .functions
            .iter_mut()
            .find(|function| function.name == "capable")
            .expect("capable function");
        function.return_type = return_type;
        let error = mir
            .verify()
            .expect_err("an unrepresentable return type must block the machine entry");
        assert!(
            error
                .to_string()
                .contains("cannot be represented by machine-entry ABI"),
            "unexpected diagnostic for {return_type:?}: {error}"
        );
    }
}

#[test]
fn lowers_time_sleep_to_suspend() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Unit effects { time } { time.sleep(1ms) }\n\
         fn main() effects { time } { wait() }";
    let mir = lower(source);
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let (continuation, suspend_block) = function
        .blocks
        .iter()
        .find_map(|block| {
            block
                .statements
                .iter()
                .find_map(|statement| match statement {
                    MirStatement::Suspend { continuation, .. } => Some((*continuation, block.id)),
                    _ => None,
                })
        })
        .expect("time.sleep suspend");
    let metadata = function
        .continuations
        .iter()
        .find(|item| item.id == continuation)
        .expect("continuation metadata");
    assert_eq!(metadata.suspend_block, suspend_block);
    assert!(function.blocks[metadata.resume_block.0]
        .statements
        .iter()
        .any(|statement| matches!(statement, MirStatement::Resume { continuation: id } if *id == continuation)));
    mir.verify().expect("time.sleep MIR should verify");
}

#[test]
fn lowers_typed_suspending_operation_to_typed_destination() {
    let source = "eff Input { @suspends fn read() -> Int32 }\n\
         fn wait() -> Int32 effects { Input } { Input.read() }\n\
         fn main() -> Int32 effects { Input } { wait() }";
    let mir = lower(source);
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let (destination, continuation) = function
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::Suspend {
                destination,
                continuation,
                ..
            } => Some((*destination, *continuation)),
            _ => None,
        })
        .expect("typed suspend");
    assert_eq!(function.value_types[destination.0], Type::I32);
    assert_eq!(
        function.continuations[continuation.0].resume_destination,
        Some(destination)
    );
    mir.verify().expect("typed suspend MIR should verify");
}

#[test]
fn verifier_rejects_continuation_protocol_mismatch() {
    let mut mir = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() effects { time } { time.sleep(1ms) }
         fn main() effects { time } { wait() }",
    );
    let continuation = &mut mir.functions[0].continuations[0];
    continuation.kind = crate::mir::MirContinuationKind::Resumable;
    let error = mir
        .verify()
        .expect_err("continuation kind must match operation");
    assert!(error
        .to_string()
        .contains("kind does not match operation mode"));
}

#[test]
fn verifier_rejects_resumable_request_on_suspending_continuation() {
    let mut mir = lower(
        "eff Input { @suspends fn read() -> Int32 }\n\
         fn wait() -> Int32 effects { Input } { Input.read() }\n\
         fn main() -> Int32 effects { Input } { wait() }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let statement = function
        .blocks
        .iter_mut()
        .flat_map(|block| block.statements.iter_mut())
        .find(|statement| matches!(statement, MirStatement::Suspend { .. }))
        .expect("suspend statement");
    let replacement = match std::mem::replace(statement, MirStatement::HandlerExit) {
        MirStatement::Suspend {
            destination,
            operation,
            continuation,
            arguments,
        } => MirStatement::ResumableRequest {
            destination,
            operation,
            continuation,
            arguments,
        },
        _ => unreachable!(),
    };
    *statement = replacement;
    mir.verify()
        .expect_err("resumable request must use a resumable continuation");
}

#[test]
fn verifier_rejects_handler_request_on_resumable_continuation() {
    let mut mir = lower(
        "eff Input { fn read() -> Int32 }\n\
         fn wait() -> Int32 effects { Input } { Input.read() }\n\
         fn main() -> Int32 effects { Input } { wait() }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = function
        .continuations
        .first()
        .expect("normal continuation")
        .id;
    function.continuations[continuation.0].kind = MirContinuationKind::Resumable;
    let error = mir
        .verify()
        .expect_err("normal handler request must use a normal continuation");
    assert!(error.to_string().contains("kind does not match operation"));
}

#[test]
fn verifier_rejects_resumable_request_on_suspending_kind() {
    let mut mir = lower(
        "eff Input { @suspends @resumable fn read() -> Int32 }\n\
         fn wait() -> Int32 effects { Input } { Input.read() }\n\
         fn main() -> Int32 effects { Input } { wait() }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = function
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::ResumableRequest { continuation, .. } => Some(*continuation),
            _ => None,
        })
        .expect("resumable request");
    function.continuations[continuation.0].kind = MirContinuationKind::Suspending;
    let error = mir
        .verify()
        .expect_err("resumable request must not use a suspending continuation");
    assert!(error.to_string().contains("kind does not match operation"));
}

#[test]
fn verifier_rejects_statements_after_task_abort() {
    let mut mir = lower(
        "eff Failure { @aborts fn stop() -> Unit }\n\
         fn worker() effects { Failure } { Failure.stop() }\n\
         fn main() effects { Failure } { worker() }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "worker")
        .expect("worker function");
    let block = function
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::TaskAbort { .. }))
        })
        .expect("abort block");
    block.statements.push(MirStatement::HandlerExit);
    mir.verify()
        .expect_err("abort must terminate its block immediately");
}

#[test]
fn verifier_rejects_statements_after_suspend() {
    let mut mir = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Unit effects { time } { time.sleep(1ms) }\n\
         fn main() effects { time } { wait() }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let suspend_block = function.continuations[0].suspend_block;
    let block = &mut function.blocks[suspend_block.0];
    // Insert a statement after Suspend
    block.statements.push(MirStatement::Unit {
        destination: MirValueId(999),
    });
    let error = mir
        .verify()
        .expect_err("Suspend must be the last statement in its block");
    assert!(error
        .to_string()
        .contains("Suspend statement must be the last statement"));
}

#[test]
fn verifier_rejects_continuation_without_resume_marker() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Unit effects { time } { time.sleep(1ms) }\n\
         fn main() effects { time } { wait() }";
    let mut mir = lower(source);
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    for block in &mut function.blocks {
        block
            .statements
            .retain(|statement| !matches!(statement, MirStatement::Resume { .. }));
    }
    assert!(mir.verify().is_err());
}

#[test]
fn verifier_rejects_continuation_resume_marker_in_wrong_block() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Unit effects { time } { time.sleep(1ms) }\n\
         fn main() effects { time } { wait() }";
    let mut mir = lower(source);
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = function
        .continuations
        .first()
        .expect("continuation metadata")
        .id;
    let resume_block = function
        .blocks
        .iter()
        .position(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::Resume { continuation: id } if *id == continuation))
        })
        .expect("resume marker block");
    let other_block = (0..function.blocks.len())
        .find(|block| *block != resume_block)
        .expect("another block");
    let marker = function.blocks[resume_block]
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::Resume { continuation: id } if *id == continuation))
        .expect("resume marker");
    let statement = function.blocks[resume_block].statements.remove(marker);
    function.blocks[other_block].statements.push(statement);
    let error = mir
        .verify()
        .expect_err("resume marker must stay in its continuation block");
    assert!(error.to_string().contains("resume marker is outside"));
}

#[test]
fn verifier_rejects_normal_edge_into_resume_block() {
    let mut mir = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait(flag: Bool) effects { time } { if flag { time.sleep(1ms) } else { () } }\n\
         fn main() effects { time } { wait(true) }",
    );
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let resume_block = function
        .continuations
        .first()
        .expect("continuation")
        .resume_block;
    let branch = function
        .blocks
        .iter_mut()
        .find_map(|block| match block.terminator.as_mut() {
            Some(MirTerminator::Branch { then_block, .. }) if block.id != resume_block => {
                Some(then_block)
            }
            _ => None,
        })
        .expect("branch block");
    *branch = resume_block;
    mir.verify()
        .expect_err("normal CFG must not enter a resume block");
}

#[test]
fn verifier_rejects_duplicate_task_result_claim() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn second() -> Int32 { 2 }\n\
         fn main() { parallel {\n| first()\n| second()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    let claim = main
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::TaskClaimResult { scope, task } => Some((*scope, *task)),
            _ => None,
        })
        .expect("branch result claim");
    let (exit_block, insert_at) = main
        .blocks
        .iter()
        .find_map(|block| {
            block
                .statements
                .iter()
                .position(|statement| {
                    matches!(statement, MirStatement::ScopeExit { scope } if *scope == claim.0)
                })
                .map(|index| (block.id, index))
        })
        .expect("scope exit");
    main.blocks[exit_block.0].statements.insert(
        insert_at,
        MirStatement::TaskClaimResult {
            scope: claim.0,
            task: claim.1,
        },
    );
    let error = mir
        .verify()
        .expect_err("a task result can only be claimed once");
    assert!(
        error.to_string().contains("claims task t") && error.to_string().contains("more than once")
    );
}

#[test]
fn verifier_rejects_task_result_read_before_claim() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn main() { parallel {\n| first()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    let block = main
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::TaskClaimResult { .. }))
        })
        .expect("task claim block");
    let claim_index = block
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::TaskClaimResult { .. }))
        .expect("task claim");
    let tuple_index = block
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::Tuple { .. }))
        .expect("parallel result tuple");
    let tuple = block.statements.remove(tuple_index);
    // The tuple reads the join destinations, so consuming the values before
    // the claim reads scope-owned result storage: the value-level escape.
    block.statements.insert(claim_index, tuple);
    let error = mir
        .verify()
        .expect_err("task result values must not be read before the scope claims them");
    assert!(error.to_string().contains("before the scope claims it"));
}

#[test]
fn verifier_rejects_task_use_after_scope_exit() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn main() { parallel {\n| first()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    let block = main
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::TaskJoin { .. }))
        })
        .expect("task join block");
    let join_index = block
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::TaskJoin { .. }))
        .expect("task join");
    let scope = match block.statements[join_index] {
        MirStatement::TaskJoin { scope, .. } => scope,
        _ => unreachable!(),
    };
    block
        .statements
        .insert(join_index, MirStatement::ScopeExit { scope });
    let error = mir
        .verify()
        .expect_err("task handles must not be used after scope exit");
    assert!(error.to_string().contains("outside its active lifetime"));
}

#[test]
fn verifier_rejects_race_without_result_selection() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn second() -> Int32 { 2 }\n\
         fn main() { race {\n| first()\n| second()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    let block = main
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::RaceSelect { .. }))
        })
        .expect("race block");
    block
        .statements
        .retain(|statement| !matches!(statement, MirStatement::RaceSelect { .. }));
    let error = mir
        .verify()
        .expect_err("a race must select a winner result");
    assert!(error.to_string().contains("without selecting"));
}

#[test]
fn verifier_rejects_race_selection_without_start() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn second() -> Int32 { 2 }\n\
         fn main() { race {\n| first()\n| second()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    let block = main
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::RaceStart { .. }))
        })
        .expect("race block");
    block
        .statements
        .retain(|statement| !matches!(statement, MirStatement::RaceStart { .. }));
    let error = mir
        .verify()
        .expect_err("race selection requires a started race");
    assert!(error.to_string().contains("was not started"));
}

#[test]
fn verifier_rejects_ordinary_consumption_of_race_task() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn second() -> Int32 { 2 }\n\
         fn main() { race {\n| first()\n| second()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    let (block, index, scope, task, result_type) = main
        .blocks
        .iter()
        .enumerate()
        .find_map(|(block_index, block)| {
            block
                .statements
                .iter()
                .enumerate()
                .find_map(|(index, statement)| match statement {
                    MirStatement::RaceStart { scope, tasks } => Some((
                        block_index,
                        index + 1,
                        *scope,
                        *tasks.first().expect("race task"),
                        main.value_types[tasks.first().expect("race task").0],
                    )),
                    _ => None,
                })
        })
        .expect("race start");
    let destination = MirValueId(main.value_types.len());
    main.value_types.push(result_type);
    main.value_ownership.push(MirOwnership::Copy);
    main.blocks[block].statements.insert(
        index,
        MirStatement::TaskJoin {
            destination,
            scope,
            task,
        },
    );
    let error = mir
        .verify()
        .expect_err("race tasks must only be consumed by RaceSelect");
    assert!(error.to_string().contains("joins race task"));
}

#[test]
fn verifier_rejects_task_in_multiple_races() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn second() -> Int32 { 2 }\n\
         fn main() { race {\n| first()\n| second()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    let race = main
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter())
        .find_map(|statement| match statement {
            MirStatement::RaceStart { scope, tasks } => Some((*scope, tasks.clone())),
            _ => None,
        })
        .expect("race start");
    let block = main
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .statements
                .iter()
                .any(|statement| matches!(statement, MirStatement::RaceSelect { .. }))
        })
        .expect("race block");
    let index = block
        .statements
        .iter()
        .position(|statement| matches!(statement, MirStatement::RaceSelect { .. }))
        .expect("race select");
    block.statements.insert(
        index,
        MirStatement::RaceStart {
            tasks: race.1,
            scope: race.0,
        },
    );
    let error = mir
        .verify()
        .expect_err("a task cannot participate in multiple races");
    assert!(error.to_string().contains("more than one race"));
}

#[test]
fn verifier_requires_claim_after_task_join() {
    let mut mir = lower(
        "fn first() -> Int32 { 1 }\n\
         fn main() { parallel {\n| first()\n} }",
    );
    let main = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main function");
    main.blocks.iter_mut().for_each(|block| {
        block
            .statements
            .retain(|statement| !matches!(statement, MirStatement::TaskClaimResult { .. }))
    });
    let error = mir
        .verify()
        .expect_err("joined task results must be claimed");
    assert!(error.to_string().contains("without claiming"));
}

#[test]
fn continuation_records_owned_values_used_after_suspend() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn pair(message: String, marker: Unit) -> Unit { println(message) }\n\
         fn wait(message: String) -> Unit effects { time } { pair(message, time.sleep(1ms)) }\n\
         fn main() effects { time } { wait(\"hello\") }";
    let mir = lower(source);
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = function
        .continuations
        .first()
        .expect("continuation metadata");
    assert!(continuation
        .spill_values
        .iter()
        .any(|value| function.value_types[value.0] == Type::String));
    assert_eq!(
        continuation.spill_slots.len(),
        continuation.spill_values.len()
    );
    assert!(continuation
        .frame_slots
        .iter()
        .any(|slot| { slot.local == function.parameters[0].local && slot.value.is_none() }));
    assert!(continuation
        .spill_slots
        .iter()
        .enumerate()
        .all(|(slot, metadata)| slot == metadata.slot));
    mir.verify()
        .expect("continuation spill metadata should verify");
}

#[test]
fn verifier_rejects_undefined_continuation_spill() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait(message: String) -> Unit effects { time } {\n\
             time.sleep(1ms)\n\
             println(message)\n\
         }\n\
         fn main() effects { time } { wait(\"hello\") }";
    let mut mir = lower(source);
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = function
        .continuations
        .first_mut()
        .expect("continuation metadata");
    let value = MirValueId(function.value_types.len());
    function.value_types.push(Type::String);
    function.value_ownership.push(MirOwnership::Shared);
    let ownership = function.value_ownership[value.0];
    continuation.spill_values.push(value);
    continuation.spill_slots.push(MirSpillSlot {
        value,
        slot: continuation.spill_slots.len(),
        ownership,
    });
    let error = mir
        .verify()
        .expect_err("spill values must dominate the suspend point");
    assert!(error.to_string().contains("spill value has no definition"));
}

#[test]
fn continuation_frame_slot_records_explicit_local_binding() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Int32 effects { time } { let value = 40; time.sleep(1ms); value + 2 }\n\
         fn main() -> Int32 effects { time } { wait() }";
    let mir = lower(source);
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = function.continuations.first().expect("continuation");
    let slot = continuation
        .frame_slots
        .iter()
        .find(|slot| slot.value.is_some())
        .expect("explicit local frame slot");
    assert_eq!(slot.ty, Type::I32);
    assert_eq!(slot.ownership, MirOwnership::Copy);
    assert!(slot
        .value
        .is_some_and(|value| function.value_types[value.0] == Type::I32));
    mir.verify().expect("frame value metadata should verify");
}

#[test]
fn continuation_frame_omits_handler_parameter_bound_after_suspend() {
    let source = r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Console { fn read(prompt: String) -> Int32 }
        fn wait() -> Int32 effects { time } {
            do { time.sleep(1ms); Console.read(prompt: "age") }
            with { Console.read(prompt) => 42 }
        }
        fn main() -> Int32 effects { time } { wait() }
    "#;
    let mir = lower(source);
    let function = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait function");
    assert!(function.blocks.iter().flat_map(|block| &block.statements).any(|statement| {
        matches!(statement, MirStatement::HandlerEnter { handlers } if handlers.iter().all(|arm| arm.resumable_value.is_some()))
    }));
    assert!(function.locals.iter().all(|local| local.name != "prompt"));
    let task = mir
        .functions()
        .iter()
        .find(|function| function.is_task && function.name.starts_with("__task_wait_"))
        .expect("do body task function");
    let continuation = task.continuations.first().expect("continuation");
    assert!(task.locals.iter().all(|local| local.name != "prompt"));
    assert!(continuation
        .frame_slots
        .iter()
        .all(|slot| task.locals[slot.local.0].name != "prompt"));
    assert!(!function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|statement| matches!(statement, MirStatement::TaskCreate { .. })));
    mir.verify()
        .expect("handler parameter should not need a pre-suspend frame slot");
}

#[test]
fn verifier_rejects_out_of_bounds_continuation_frame_value() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Int32 effects { time } { let value = 40; time.sleep(1ms); value + 2 }\n\
         fn main() -> Int32 effects { time } { wait() }";
    let mut mir = lower(source);
    let function = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let slot = function
        .continuations
        .first_mut()
        .and_then(|continuation| {
            continuation
                .frame_slots
                .iter_mut()
                .find(|slot| slot.value.is_some())
        })
        .expect("explicit local frame slot");
    slot.value = Some(MirValueId(usize::MAX));
    assert!(mir.verify().is_err());
}

#[test]
fn reports_machine_entry_capability_per_continuation() {
    let mir = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn capable() -> Int32 effects { time } { time.sleep(1ms); 42 }\n\
         fn branching(mode: Bool) -> String effects { time } {\n\
             time.sleep(1ms)\n\
             if mode \"yes\" else \"no\"\n\
         }\n\
         fn resuspending() -> Unit effects { time } {\n\
             time.sleep(1ms)\n\
             let first = capable()\n\
         }\n\
         fn main() effects { time } { resuspending() }",
    );
    mir.verify().unwrap();
    let capable_continuations = mir
        .functions()
        .iter()
        .filter(|function| function.name == "capable")
        .flat_map(|function| &function.continuations)
        .count();
    assert_eq!(capable_continuations, 1);
    let dump = mir.dump();
    assert!(dump.contains("function 'capable' continuation c0: machine-entry capable"));
    assert!(dump.contains("function 'branching' continuation c0: machine-entry capable"));
    assert!(dump.contains("function 'resuspending' continuation c0: machine-entry capable"));
}

#[test]
fn machine_entry_accepts_normal_handler_requests_in_resume_tails() {
    let mir = lower(
        "eff Input { @suspends fn read() -> Int32 }\n\
         eff Log { fn write() -> Unit }\n\
         fn task() -> Int32 effects { Input, Log } {\n\
             let number = Input.read()\n\
             Log.write()\n\
             number\n\
         }\n\
         fn main() effects { Input, Log } {\n\
             let values = parallel { | task() }\n\
             println(values.0)\n\
         }",
    );
    mir.verify().unwrap();
    // A synchronous Normal request in the resume tail keeps the machine entry.
    assert!(mir
        .dump()
        .contains("function 'task' continuation c0: machine-entry capable"));
}

#[test]
fn machine_entry_accepts_abort_statements_in_resume_tails() {
    let mir = lower(
        "eff Input { @suspends fn read() -> Int32 }\n\
         eff Abort { @aborts fn stop() -> Unit }\n\
         fn task() -> Unit effects { Input, Abort } {\n\
             let ignored = Input.read()\n\
             Abort.stop()\n\
         }\n\
         fn main() effects { Input, Abort } {\n\
             let ignored = parallel { | task() }\n\
         }",
    );
    mir.verify().unwrap();
    // The abort block shape (TaskAbort, cleanup, Return) compiles inside a
    // machine entry; completion publishes the Aborted state.
    assert!(mir
        .dump()
        .contains("function 'task' continuation c0: machine-entry capable"));
}

#[test]
fn machine_entry_supports_resumable_request_in_resume_tail() {
    let mir = lower(
        "eff Input {\n\
         \x20   @suspends fn read() -> Int32\n\
         \x20   @resumable fn ask() -> Int32\n\
         }\n\
         fn task() -> Int32 effects { Input } {\n\
             let number = Input.read()\n\
             Input.ask()\n\
         }\n\
         fn main() effects { Input } {\n\
             let values = parallel { | task() }\n\
             println(values.0)\n\
         }",
    );
    mir.verify().unwrap();

    // Check the actual capability
    let function = mir
        .functions()
        .iter()
        .find(|f| f.name == "task")
        .expect("task function");
    let continuation = &function.continuations[0];
    let blocker = crate::mir::continuation_capability::machine_entry_blocker(
        function,
        continuation,
        mir.types(),
    );

    assert!(
        blocker.is_none(),
        "ResumableRequest in resume tail should not block machine entry, but got: {:?}",
        blocker
    );
}

#[test]
fn machine_entry_accepts_bytes_return_type() {
    let mir = lower(
        "eff Input { @suspends fn read() -> Bytes }\n\
         fn task() -> Bytes effects { Input } { Input.read() }\n\
         fn main() effects { Input } { let values = parallel { | task() } }",
    );
    mir.verify().unwrap();
    let function = mir
        .functions()
        .iter()
        .find(|f| f.name == "task")
        .expect("task function");
    let blocker = crate::mir::continuation_capability::machine_entry_blocker(
        function,
        &function.continuations[0],
        mir.types(),
    );
    assert!(blocker.is_none(), "blocker: {:?}", blocker);
}

#[test]
fn machine_entry_accepts_mut_bytes_return_type() {
    let mir = lower(
        "eff Input { @suspends fn read() -> MutBytes }\n\
         fn task() -> MutBytes effects { Input } { Input.read() }\n\
         fn main() effects { Input } { let values = parallel { | task() } }",
    );
    mir.verify().unwrap();
    let function = mir.functions().iter().find(|f| f.name == "task").unwrap();
    assert!(crate::mir::continuation_capability::machine_entry_blocker(
        function,
        &function.continuations[0],
        mir.types(),
    )
    .is_none());
}

#[test]
fn nested_suspending_calls_preserve_continuation_chain() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn deepest() -> Unit effects { time } { time.sleep(1ms) }\n\
         fn middle() -> Unit effects { time } { deepest() }\n\
         fn outer() -> Unit effects { time } { middle() }\n\
         fn main() effects { time } { outer() }";
    let mir = lower(source);

    // Each function in the chain should have is_suspending = true
    for name in ["deepest", "middle", "outer", "main"] {
        let function = mir
            .functions()
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{} function", name));
        assert!(
            function.is_suspending,
            "{} should be marked as suspending",
            name
        );
    }

    mir.verify()
        .expect("nested suspending call chain should verify");
}

#[test]
fn nested_suspending_calls_with_values_preserve_types() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn compute() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             42\n\
         }\n\
         fn add_ten() -> Int32 effects { time } {\n\
             compute() + 10\n\
         }\n\
         fn double() -> Int32 effects { time } {\n\
             add_ten() * 2\n\
         }\n\
         fn main() effects { time } { println(double()) }";
    let mir = lower(source);

    // Verify all functions are suspending
    for name in ["compute", "add_ten", "double"] {
        let function = mir
            .functions()
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{} function", name));
        assert!(
            function.is_suspending,
            "{} should be marked as suspending",
            name
        );
    }

    mir.verify()
        .expect("nested suspending calls with values should verify");
}

#[test]
fn mixed_suspending_and_non_suspending_calls() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn pure(x: Int32) -> Int32 { x + 1 }\n\
         fn suspending() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             pure(41)\n\
         }\n\
         fn main() effects { time } { println(suspending()) }";
    let mir = lower(source);

    let pure_fn = mir
        .functions()
        .iter()
        .find(|f| f.name == "pure")
        .expect("pure function");
    assert!(
        !pure_fn.is_suspending,
        "pure function should not be marked as suspending"
    );

    let suspending_fn = mir
        .functions()
        .iter()
        .find(|f| f.name == "suspending")
        .expect("suspending function");
    assert!(
        suspending_fn.is_suspending,
        "suspending function should be marked as suspending"
    );

    mir.verify()
        .expect("mixed suspending and non-suspending calls should verify");
}

#[test]
fn multiple_suspending_calls_in_sequence() {
    let source = "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn multi_wait() -> Unit effects { time } {\n\
             time.sleep(1ms)\n\
             time.sleep(2ms)\n\
             time.sleep(3ms)\n\
         }\n\
         fn main() effects { time } { multi_wait() }";
    let mir = lower(source);

    let function = mir
        .functions()
        .iter()
        .find(|f| f.name == "multi_wait")
        .expect("multi_wait function");

    assert!(function.is_suspending);

    // Count the number of suspend statements
    let suspend_count = function
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter(|stmt| matches!(stmt, MirStatement::Suspend { .. }))
        .count();

    assert_eq!(
        suspend_count, 3,
        "multi_wait should have 3 suspend statements"
    );

    mir.verify()
        .expect("multiple suspending calls in sequence should verify");
}

#[test]
fn verified_programs_have_no_machine_entry_blockers() {
    // After phase 3 there is no synchronous fallback: any suspending
    // continuation that survives verification must be machine-entry capable.
    // Pin that codegen/verifier agreement across the representative shapes.
    let sources = [
        // direct suspending calls in resume tails, repeated suspends
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn inner() -> Int32 effects { time } { time.sleep(1ms); 40 }\n\
         fn outer() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             inner() + 2\n\
         }\n\
         fn main() effects { time } { let v = parallel {\n| outer()\n}; println(v.0) }",
        // suspending closures (indirect calls) and recursion
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn countdown(n: Int32) -> Int32 effects { time } {\n\
             if n == 0 { 0 } else { time.sleep(1ms); countdown(n - 1) + 1 }\n\
         }\n\
         fn wait() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             let callback = fn () -> Int32 { time.sleep(1ms); 42 }\n\
             callback() + countdown(2)\n\
         }\n\
         fn main() effects { time } { let v = parallel {\n| wait()\n}; println(v.0) }",
        // aborts with typed payloads and normal handler requests
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         eff Boom { @aborts fn stop(message: String) -> Unit }\n\
         eff Log { fn write() -> Unit }\n\
         fn task() -> Int32 effects { time, Boom, Log } {\n\
             time.sleep(1ms)\n\
             Log.write()\n\
             Boom.stop(message: \"boom\")\n\
             0\n\
         }\n\
         fn main() -> Unit effects { time, Boom, Log } {\n\
             do { let ignored = parallel { | task() }; println(\"joined\") } with {\n\
                 Boom.stop(message) => println(message)\n\
             }\n\
         }",
        // task lifecycle, race, and cancellation in resume tails
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn slow() -> Int32 effects { time } { time.sleep(30ms); 1 }\n\
         fn quick() -> Int32 effects { time } { time.sleep(1ms); 2 }\n\
         fn wait() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             let winner = race {\n| slow()\n| quick()\n}\n\
             let later = parallel {\n| quick()\n}\n\
             winner + later.0\n\
         }\n\
         fn main() effects { time } { let v = parallel {\n| wait()\n}; println(v.0) }",
    ];
    for source in sources {
        let mir = lower(source);
        mir.verify()
            .unwrap_or_else(|error| panic!("{error:?}\n{}", mir.dump()));
        for function in mir.functions() {
            for continuation in &function.continuations {
                if !continuation.kind.is_suspending() {
                    continue;
                }
                assert_eq!(
                    crate::mir::continuation_capability::machine_entry_blocker(
                        function,
                        continuation,
                        mir.types()
                    ),
                    None,
                    "verified continuation c{} of '{}' must be machine-entry capable:\n{}",
                    continuation.id.0,
                    function.name,
                    mir.dump()
                );
            }
        }
    }
}

#[test]
fn verifier_rejects_task_wait_with_effect_metadata() {
    let mut mir = lower("fn main() { let result = parallel { | 1 }; println(result.0) }");
    let wait = mir
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.continuations)
        .find(|continuation| continuation.kind == MirContinuationKind::TaskWait)
        .expect("parallel must have an internal wait");
    wait.callee = Some(MirFunctionId(0));
    assert!(mir
        .verify()
        .unwrap_err()
        .to_string()
        .contains("invalid internal wait continuation"));
}

#[test]
fn verifier_rejects_cown_acquire_with_task_wait_kind() {
    let mut mir = lower("class Counter { var n: Int32 = 0 } fn main() { let c = Cown.new(Counter()); when (c) |x| { println(x.n) } }");
    let acquire = mir
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.continuations)
        .find(|continuation| continuation.kind == MirContinuationKind::CownAcquire)
        .expect("when must have an internal acquisition");
    acquire.kind = MirContinuationKind::TaskWait;
    assert!(mir
        .verify()
        .unwrap_err()
        .to_string()
        .contains("invalid internal wait continuation"));
}

#[test]
fn infinite_loop_does_not_create_unreachable_continuations() {
    for source in [
        "fn main() { loop { branch { println(1) } } }",
        "fn main() { loop { loop { break }; branch { println(1) } } }",
        "eff time { @suspends fn sleep(duration: Duration) -> Unit } fn main() effects { time } { loop { time.sleep(1ms) }; time.sleep(1ms) }",
    ] {
        let mir = lower(source);
        if let Err(error) = mir.verify() { panic!("{error}\n{}", mir.dump()); }
    }
}

#[test]
fn verifier_rejects_condition_wait_without_the_entire_lease_set() {
    let mut mir = lower("class State { var n: Int32 = 0 } fn main() { let a = Cown.new(State()); let b = Cown.new(State()); when (a, b) |x, y| until x.n == 1 { () } }");
    let wait = mir
        .functions
        .iter_mut()
        .flat_map(|f| &mut f.blocks)
        .flat_map(|b| &mut b.statements)
        .find_map(|s| match s {
            MirStatement::CownAcquire {
                wait_for_change: true,
                arguments,
                ..
            } => Some(arguments),
            _ => None,
        })
        .unwrap();
    wait.pop();
    assert!(mir.verify().is_err());
}
