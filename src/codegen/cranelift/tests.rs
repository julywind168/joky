use super::*;
use crate::{hir::CoreProgram, sema, syntax};

mod local_ssa;
mod pending_operations;

#[test]
fn multi_function_object_emission_is_deterministic() {
    let mir = lower(
        r#"
        fn a() -> Int32 { 1 }
        fn b() -> Int32 { 2 }
        fn c() -> Int32 { 3 }
        fn main() { println(a() + b() + c()) }
        "#,
    );
    let first = crate::codegen::emit_mir_object_with_entries(&mir, false).unwrap();
    let second = crate::codegen::emit_mir_object_with_entries(&mir, false).unwrap();
    assert!(!first.bytes.is_empty());
    assert_eq!(first.bytes, second.bytes);
}

fn lower(source: &str) -> MirProgram {
    let program = syntax::parse_program(source).expect("source should parse");
    let types = sema::check_program(&program).expect("source should type check");
    let core = CoreProgram::lower(program, types).expect("source should lower to HIR");
    MirProgram::lower(&core).expect("HIR should lower to MIR")
}

#[test]
fn resumed_cown_body_can_store_through_a_new_payload_borrow() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        class State {
            var value: Int32 = 0
            fn set(value: Int32) { self.value = value }
        }
        fn main() effects { time } {
            time.sleep(1ms)
            let state = Cown.new(State())
            when (state) |s| { s.set(42) }
            if when (state) |s| { s.value } != 42 { panic("resumed store failed") }
        }
    "#,
    );
    let access = mir
        .functions
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.statements)
        .find_map(|s| match s {
            MirStatement::Store { access, .. } => Some(*access),
            _ => None,
        })
        .unwrap();
    let main = mir.functions.iter_mut().find(|f| f.name == "main").unwrap();
    // Model an inlined setter. Source field mutation remains method-only;
    // the payload borrow is created after the suspension, never spilled.
    let statement = main
        .blocks
        .iter_mut()
        .flat_map(|b| &mut b.statements)
        .find(|s| matches!(s, MirStatement::MethodCall { .. }))
        .unwrap();
    let MirStatement::MethodCall {
        destination,
        receiver,
        arguments,
        ..
    } = statement
    else {
        unreachable!()
    };
    *statement = MirStatement::Store {
        destination: *destination,
        receiver: *receiver,
        access,
        value: arguments[0].value,
    };
    mir.verify().unwrap();
    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert!(joky_runtime::host::testing::machine_resumptions(&scope) > 0);
    scope.close_and_wait();
    assert_eq!(joky_runtime::host::testing::managed_objects(&scope), 0);
}

#[test]
fn failed_cown_fast_attempt_cleans_native_and_resumed_owned_locals() {
    for before in ["", "time.sleep(1ms)"] {
        let mir = lower(&format!(
            r#"
            eff time {{ @suspends fn sleep(duration: Duration) -> Unit }}
            class State {{ var value: Int32 = 0 }}
            fn acquire(a: Cown(State), b: Cown(State)) effects {{ time }} {{
                let text = "owned" + " local"
                {before}
                when (a, b) |x, y| {{ println(x.value + y.value) }}
                println(text)
            }}
            fn main() effects {{ time }} {{
                let a = Cown.new(State())
                acquire(a, a)
            }}
        "#
        ));
        mir.verify().unwrap();
        let scope = joky_runtime::host::RuntimeScope::new();
        let _guard = scope.enter();
        let mut backend = CraneliftBackend::new().unwrap();
        assert!(backend.compile_and_run_program(&mir, &scope).is_err());
        scope.close_and_wait();
        assert_eq!(joky_runtime::host::testing::managed_objects(&scope), 0);
        assert_eq!(joky_runtime::host::testing::resource_counts(&scope), [0; 6]);
    }
}

#[test]
fn aot_machine_entries_are_program_local_and_deterministic() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        fn wait() -> String effects { time } {
            time.sleep(1ms)
            time.sleep(1ms)
            "done"
        }
        fn main() effects { time } { println(wait()) }
        "#,
    );
    let emit = |mir: &MirProgram| {
        let object = crate::codegen::emit_mir_object_with_entries(mir, false).unwrap();
        assert!(!object.bytes.is_empty());
        object.machine_entries
    };
    let first = emit(&mir);
    let expected_count = mir
        .functions
        .iter()
        .flat_map(|f| &f.continuations)
        .filter(|c| c.kind.is_suspending())
        .count();
    assert!(expected_count >= 3);
    assert_eq!(first.len(), expected_count);
    assert!(first.iter().all(|(key, _)| *key != 0));
    assert!(first.windows(2).all(|pair| pair[0].0 < pair[1].0));

    let mut jit = CraneliftBackend::new().unwrap();
    let jit_key = jit.continuation_entry_key(MirFunctionId(0), MirContinuationId(0));
    assert_eq!(
        first,
        emit(&mir),
        "JIT allocation must not affect AOT entries"
    );
    let mut other_jit = CraneliftBackend::new().unwrap();
    assert!(other_jit.continuation_entry_key(MirFunctionId(0), MirContinuationId(0)) > jit_key);
    assert_eq!(
        jit.continuation_entry_key(MirFunctionId(0), MirContinuationId(0)),
        jit_key
    );

    mir.functions.reverse();
    assert_eq!(
        first,
        emit(&mir),
        "entry identity must not depend on emission order"
    );
}

#[test]
fn function_pending_runtime_imports_execute_ready_handshake() {
    use cranelift_codegen::ir::InstBuilder;
    use cranelift_frontend::FunctionBuilder;
    use joky_runtime_abi::symbols::{
        CONTINUATION_POLL_FUNCTION_PENDING_SYMBOL, CONTINUATION_TAKE_FUNCTION_PENDING_RESULT_SYMBOL,
    };

    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    let pointer = backend.module.target_config().pointer_type();
    let call_conv = backend.module.isa().default_call_conv();
    let ids =
        runtime_ids::declare_continuation_runtime_ids(&mut backend, pointer, call_conv).unwrap();
    let mut signature = backend.module.make_signature();
    signature.returns.push(AbiParam::new(types::I8));
    let entry = backend
        .module
        .declare_function("test_function_pending_ready", Linkage::Local, &signature)
        .unwrap();
    let mut context = backend.module.make_context();
    context.func.signature = signature;
    let new = backend
        .module
        .declare_func_in_func(ids.continuation_new_id, &mut context.func);
    let begin = backend.module.declare_func_in_func(
        ids.continuation_begin_function_pending_id,
        &mut context.func,
    );
    let complete = backend.module.declare_func_in_func(
        ids.continuation_complete_function_pending_id,
        &mut context.func,
    );
    let free = backend
        .module
        .declare_func_in_func(ids.continuation_free_id, &mut context.func);
    let mut poll_signature = backend.module.make_signature();
    poll_signature.params.push(AbiParam::new(pointer));
    poll_signature.returns.push(AbiParam::new(types::I8));
    let poll = backend
        .module
        .declare_function(
            CONTINUATION_POLL_FUNCTION_PENDING_SYMBOL,
            Linkage::Import,
            &poll_signature,
        )
        .unwrap();
    let poll = backend.module.declare_func_in_func(poll, &mut context.func);
    let mut take_signature = backend.module.make_signature();
    take_signature.params.extend([AbiParam::new(pointer); 3]);
    take_signature.returns.push(AbiParam::new(types::I8));
    let take = backend
        .module
        .declare_function(
            CONTINUATION_TAKE_FUNCTION_PENDING_RESULT_SYMBOL,
            Linkage::Import,
            &take_signature,
        )
        .unwrap();
    let take = backend.module.declare_func_in_func(take, &mut context.func);
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let block = builder.create_block();
        builder.switch_to_block(block);
        let generation = builder.ins().iconst(types::I64, 0);
        let null = builder.ins().iconst(pointer, 0);
        let allocated = builder.ins().call(new, &[generation]);
        let handle = builder.inst_results(allocated)[0];
        builder.ins().call(begin, &[handle]);
        builder.ins().call(complete, &[handle, null, null]);
        let polled = builder.ins().call(poll, &[handle]);
        let polled_status = builder.inst_results(polled)[0];
        let taken = builder.ins().call(take, &[handle, null, null]);
        let taken_status = builder.inst_results(taken)[0];
        let status = builder.ins().bor(polled_status, taken_status);
        builder.ins().call(free, &[handle]);
        builder.ins().return_(&[status]);
        builder.seal_all_blocks();
        builder.finalize(backend.module.target_config());
    }
    backend.module.define_function(entry, &mut context).unwrap();
    backend.module.finalize_definitions().unwrap();
    let address = backend.module.get_finalized_function(entry);
    // SAFETY: the finalized test function has the C ABI () -> u8 signature.
    let run: unsafe extern "C" fn() -> u8 = unsafe { std::mem::transmute(address) };
    assert_eq!(
        unsafe { run() },
        joky_runtime::host::FunctionCallStatus::Ready as u8
    );
    scope.close_and_wait();
}

#[test]
fn standalone_resume_accepts_sync_calls_and_loops() {
    let mir = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn increment(value: Int32) -> Int32 { value + 1 }\n\
         fn wait() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             loop {\n\
                 let value = increment(value: 41)\n\
                 if value == 42 { break value } else { continue }\n\
             }\n\
         }\n\
         fn main() effects { time } { wait() }",
    );
    let wait = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait MIR function");
    let continuation = wait.continuations.first().expect("sleep continuation");
    assert!(machine_entry_blocker(wait, continuation, mir.types(),).is_none());
}

#[test]
fn machine_resume_preserves_multiple_scalar_and_managed_temporaries() {
    let mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn combine(a: Int32, b: String, c: Int32, d: String) -> String {
            if a + c == 42 { b + d } else { "wrong" }
        }
        fn wait() -> String effects { time } {
            combine(20, "before", { time.sleep(1ms); 22 }, { time.sleep(1ms); "after" })
        }
        fn main() effects { time, Check } {
            let results = parallel { | wait() }
            if results.0 != "beforeafter" { Check.failed() } else { println(results.0) }
        }
    "#,
    );
    run_task_machine_test(mir, false);
}

#[test]
fn machine_resume_preserves_spills_in_a_loop() {
    let mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn wait() -> Int32 effects { time } {
            loop {
                let answer = 20 + { time.sleep(1ms); 22 }
                if answer == 42 { break answer } else { continue }
            }
        }
        fn main() effects { time, Check } {
            let results = parallel { | wait() }
            if results.0 != 42 { Check.failed() } else { println(results.0) }
        }
    "#,
    );
    run_task_machine_test(mir, false);
}

#[test]
fn standalone_resume_accepts_owned_cleanup() {
    let mir = lower(
        "class Token { let value: Int32 = 42 }\n\
         eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             let token = Token()\n\
             { let message = \"awake\"; println(message) }\n\
             token.value\n\
         }\n\
         fn main() effects { time } { wait() }",
    );
    let wait = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait MIR function");
    let continuation = wait.continuations.first().expect("sleep continuation");
    assert!(wait.blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::DropLocal { .. }))
    }));
    assert!(
        machine_entry_blocker(wait, continuation, mir.types(),).is_none(),
        "{}",
        wait.dump()
    );
}

#[test]
fn standalone_resume_accepts_method_and_indirect_calls() {
    let mir = lower(
        "class Counter {\n\
             let base: Int32\n\
             fn add(value: Int32) -> Int32 { self.base + value }\n\
         }\n\
         eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait(counter: Counter, callback: fn(input: Int32) -> Int32) -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             callback(input: counter.add(value: 1))\n\
         }\n\
         fn main() effects { time } { wait(counter: Counter(base: 1), callback: fn (input: Int32) -> Int32 { input + 40 }) }",
    );
    let wait = mir
        .functions()
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait MIR function");
    let continuation = wait.continuations.first().expect("sleep continuation");
    assert!(wait.blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::MethodCall { .. }))
    }));
    assert!(wait.blocks.iter().any(|block| {
        block
            .statements
            .iter()
            .any(|statement| matches!(statement, MirStatement::CallIndirect { .. }))
    }));
    assert!(machine_entry_blocker(wait, continuation, mir.types(),).is_none());
}

#[test]
fn standalone_resume_accepts_non_root_task_scope_with_heap_handles() {
    let mir = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn child() -> Int32 { 1 }\n\
         fn wait() -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             let value = parallel { | child() }\n\
             42\n\
         }\n\
         fn main() effects { time } { wait() }",
    );
    let wait = mir
        .functions
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = wait.continuations.first().expect("sleep continuation");
    assert!(machine_entry_blocker(wait, continuation, mir.types()).is_none());
}

#[test]
fn machine_entry_rejects_reads_of_native_only_locals() {
    let mut mir = lower(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         fn wait(kept: Int32) -> Int32 effects { time } {\n\
             time.sleep(1ms)\n\
             kept + 1\n\
         }\n\
         fn main() effects { time } {\n\
             let value = parallel { | wait(kept: 41) }\n\
             println(value.0)\n\
         }",
    );
    mir.verify().expect("baseline MIR must verify");
    // Drop the frame slot that carries `kept` across the suspend. The
    // parameter local then exists only in the original native frame, which a
    // machine entry must never read.
    let wait = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    let continuation = wait.continuations.first_mut().expect("sleep continuation");
    assert!(
        !continuation.frame_slots.is_empty(),
        "the parameter must be saved in a frame slot"
    );
    let dropped = continuation.frame_slots.remove(0);
    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().expect("JIT backend");
    let error = backend
        .compile_and_run_program(&mir, &scope)
        .expect_err("a machine entry must not compile a read of a native-only local");
    assert!(
        error.message().contains(&format!(
            "stackless machine entry cannot resolve native-only local l{}",
            dropped.local.0
        )),
        "unexpected error: {}",
        error.message()
    );
}

fn run_task_machine_test(mir: MirProgram, move_create_before_suspend: bool) {
    let mut mir = mir;
    let wait = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .expect("wait function");
    if move_create_before_suspend {
        // Source parallel syntax joins immediately. Move the verified task
        // creation across Suspend to exercise a live scope spanning entries.
        let continuation = wait.continuations.first().unwrap().clone();
        let mut before_suspend = Vec::new();
        for block in &mut wait.blocks {
            let mut retained = Vec::new();
            for statement in block.statements.drain(..) {
                if matches!(
                    statement,
                    MirStatement::ScopeEnter { .. } | MirStatement::TaskCreate { .. }
                ) {
                    before_suspend.push(statement);
                } else {
                    retained.push(statement);
                }
            }
            block.statements = retained;
        }
        assert!(!before_suspend.is_empty());
        let statements = &mut wait.blocks[continuation.suspend_block.0].statements;
        let suspend = statements
            .iter()
            .position(|statement| matches!(statement, MirStatement::Suspend { .. }))
            .unwrap();
        statements.splice(suspend..suspend, before_suspend);
    }
    mir.verify().expect("task lifecycle MIR must verify");
    let wait = mir
        .functions
        .iter()
        .find(|function| function.name == "wait")
        .unwrap();
    for continuation in &wait.continuations {
        assert!(
            machine_entry_blocker(wait, continuation, mir.types()).is_none(),
            "{}",
            wait.dump()
        );
    }
    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().expect("JIT backend");
    backend
        .compile_and_run_program(&mir, &scope)
        .expect("machine task lifecycle should run and preserve its result");
    assert!(
        joky_runtime::host::testing::machine_resumptions(&scope) > 0,
        "test must execute a machine entry"
    );
    assert_eq!(joky_runtime::host::testing::managed_objects(&scope), 0);
}

#[test]
fn machine_task_join_restores_heap_owned_results_across_suspend() {
    for move_create in [false, true] {
        run_task_machine_test(
            lower(
                r#"
            class Token { let value: Int32 }
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            eff Check { @aborts fn failed() -> Unit }
            fn child() -> Token { Token(value: 42) }
            fn other() -> Token { Token(value: 4) }
            fn wait() -> Int32 effects { time } {
                time.sleep(1ms)
                time.sleep(1ms)
                let results = parallel {
                    | child()
                    | other()
                }
                results.0.value + results.1.value * 2 - 8
            }
            fn main() effects { time, Check } {
                let results = parallel { | wait() }
                if results.0 != 42 { Check.failed() } else { println(42) }
            }
        "#,
            ),
            move_create,
        );
    }
}

#[test]
fn machine_race_transfers_owned_winner_across_suspend() {
    for move_create in [false, true] {
        run_task_machine_test(
            lower(
                r#"
            class Token { let value: Int32 }
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            eff Check { @aborts fn failed() -> Unit }
            fn child() -> Token { Token(value: 42) }
            fn wait() -> Int32 effects { time } {
                time.sleep(1ms)
                let winner = race {
                    | child()
                    | child()
                }
                winner.value
            }
            fn main() effects { time, Check } {
                let results = parallel { | wait() }
                if results.0 != 42 { Check.failed() } else { println(42) }
            }
        "#,
            ),
            move_create,
        );
    }
}

#[test]
fn nested_suspending_calls_resume_through_machine_entries() {
    let mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn inner(value: Int32) -> Int32 effects { time } {
            time.sleep(1ms)
            value + 2
        }
        fn wait() -> Int32 effects { time } {
            time.sleep(1ms)
            let first = inner(20)
            let second = inner(first)
            second
        }
        fn main() effects { time, Check } {
            let results = parallel { | wait() }
            if results.0 != 24 { Check.failed() } else { println(results.0) }
        }
    "#,
    );
    mir.verify().expect("nested call MIR must verify");
    let wait = mir
        .functions
        .iter()
        .find(|function| function.name == "wait")
        .expect("wait function");
    for continuation in &wait.continuations {
        assert!(
            machine_entry_blocker(wait, continuation, mir.types()).is_none(),
            "{}",
            wait.dump()
        );
    }
    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().expect("JIT backend");
    backend
        .compile_and_run_program(&mir, &scope)
        .expect("nested suspending calls should complete asynchronously");
    // The outer suspend, the resumed-tail calls, and each inner suspend all
    // dispatch machine entries; the caller never blocks a worker.
    let resumptions = joky_runtime::host::testing::machine_resumptions(&scope);
    assert!(
        resumptions >= 5,
        "expected the outer entry, both calls and both inner suspends to \
         dispatch machine entries, got {resumptions}"
    );
    assert_eq!(joky_runtime::host::testing::managed_objects(&scope), 0);
}

#[test]
fn machine_scope_rethrows_failure_to_parent_handler() {
    run_task_machine_test(
        lower(
            r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Failure { @aborts fn stop(code: Int32, text: String) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn child() -> Int32 effects { Failure } { Failure.stop(code: 42, text: "failure"); 0 }
        fn wait() -> Int32 effects { time, Failure } {
            time.sleep(1ms)
            let results = parallel { | child() }
            results.0
        }
        fn main() effects { time, Check } {
            let result = do {
                let values = parallel { | wait() }
                values.0
            } with { Failure.stop(code, text) => { println(text); code } }
            if result != 42 { Check.failed() } else { println(42) }
        }
    "#,
        ),
        false,
    );
}

#[test]
fn machine_scope_claims_typed_failure_payload() {
    run_task_machine_test(
        lower(
            r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Failure { @aborts fn stop(code: Int32, token: Token) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn child() -> Int32 effects { Failure } { Failure.stop(code: 40, token: Token(value: 2)); 0 }
        fn wait() -> Int32 effects { time } {
            time.sleep(1ms)
            do {
                let results = parallel { | child() }
                results.0
            } with { Failure.stop(code, token) => code + token.value }
        }
        fn main() effects { time, Check } {
            let results = parallel { | wait() }
            if results.0 != 42 { Check.failed() } else { println(42) }
        }
    "#,
        ),
        false,
    );
}

#[test]
fn machine_scope_handles_branch_and_loop_entries() {
    run_task_machine_test(
        lower(
            r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn child(value: Int32) -> Token { Token(value: value) }
        fn wait() -> Int32 effects { time } {
            time.sleep(1ms)
            let condition = true
            let value = if condition {
                let results = parallel { | child(value: 40) }
                results.0.value
            } else {
                let winner = race {
                    | child(value: 1)
                    | child(value: 2)
                }
                winner.value
            }
            loop {
                let results = parallel { | child(value: value + 2) }
                break results.0.value
            }
        }
        fn main() effects { time, Check } {
            let results = parallel { | wait() }
            if results.0 != 42 { Check.failed() } else { println(42) }
        }
    "#,
        ),
        false,
    );
}

#[test]
fn cancelling_suspended_parent_drops_its_unclaimed_heap_child() {
    run_task_machine_test(
        lower(
            r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn child() -> Token { Token(value: 99) }
        fn fast() -> Int32 effects { time } { time.sleep(5ms); 42 }
        fn wait() -> Int32 effects { time } {
            time.sleep(60s)
            let results = parallel { | child() }
            results.0.value
        }
        fn main() effects { time, Check } {
            let result = race {
                | wait()
                | fast()
            }
            if result != 42 { Check.failed() } else { println(42) }
        }
    "#,
        ),
        true,
    );
}

#[test]
fn machine_task_cancel_uses_restored_task_handle() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn wait() -> Int32 effects { time } {
            time.sleep(1ms)
            let results = parallel { | loop { } }
            42
        }
        fn main() effects { time, Check } {
            let results = parallel { | wait() }
            if results.0 != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let wait = mir
        .functions
        .iter_mut()
        .find(|function| function.name == "wait")
        .unwrap();
    let tasks = wait
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter_map(|statement| match statement {
            MirStatement::TaskCreate { scope, task, .. } => Some((*scope, *task)),
            _ => None,
        })
        .collect::<Vec<_>>();
    for block in &mut wait.blocks {
        let mut statements = Vec::new();
        for statement in block.statements.drain(..) {
            match statement {
                statement @ MirStatement::TaskWait { scope, .. } => {
                    for (_, task) in tasks.iter().filter(|(task_scope, _)| *task_scope == scope) {
                        statements.push(MirStatement::TaskCancel { scope, task: *task });
                    }
                    statements.push(statement);
                }
                MirStatement::TaskJoin { destination, .. } => {
                    statements.push(MirStatement::Unit { destination });
                }
                MirStatement::TaskClaimResult { .. } => {}
                statement => statements.push(statement),
            }
        }
        block.statements = statements;
    }
    run_task_machine_test(mir, true);
}
