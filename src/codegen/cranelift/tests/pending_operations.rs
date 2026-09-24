use super::*;
use cranelift_codegen::ir::InstBuilder;
use cranelift_frontend::FunctionBuilder;
use joky_runtime::host::{
    testing::{
        complete_suspend_with_payload as complete_standalone_suspend, machine_resumptions,
        register_suspend_provider as register_standalone_provider, with_function_pending_boundary,
        ContinuationHandle,
    },
    FunctionCallStatus, RuntimeScope,
};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

#[test]
fn captured_suspending_closure_uses_indirect_pending_call() {
    let mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn main() effects { time, Check } {
            let value = 42
            let f = fn () -> Int32 { time.sleep(1ms); value }
            if f() != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    scope.wait_for_idle();
    drop(_guard);
    scope.close_and_wait();
}

#[test]
fn ordinary_pending_call_chain_resumes_from_generated_machine_entries() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf(value: Int32) -> Int32 effects { time } {
            time.sleep(1ms)
            value + 1
        }
        fn middle(value: Int32) -> Int32 effects { time } {
            let saved = 20
            let result = leaf(value)
            saved + result
        }
        fn main() effects { time, Check } {
            let result = middle(21)
            if result != 42 { Check.failed() } else { println(result) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 3);
}

#[test]
fn ordinary_pending_ready_branch_stays_on_the_native_path() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf(pending: Bool) -> Int32 effects { time } {
            if pending { time.sleep(1ms); 21 } else { 21 }
        }
        fn middle() -> Int32 effects { time } { leaf(false) * 2 }
        fn main() effects { time, Check } {
            if middle() != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 0);
}

#[test]
fn ordinary_pending_chain_preserves_parent_across_repeated_operations() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf(value: Int32) -> Int32 effects { time } {
            time.sleep(1ms)
            let first = value + 1
            time.sleep(1ms)
            let second = first + 1
            time.sleep(1ms)
            second
        }
        fn middle() -> Int32 effects { time } { leaf(40) }
        fn main() effects { time, Check } {
            if middle() != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 5);
}

#[test]
fn ordinary_pending_resumed_entry_calls_a_second_suspending_function() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf(value: Int32) -> Int32 effects { time } {
            time.sleep(1ms)
            value
        }
        fn middle() -> Int32 effects { time } { leaf(20) + leaf(22) }
        fn main() effects { time, Check } {
            if middle() != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 5);
}

#[test]
fn ordinary_pending_resumed_call_can_return_ready_with_managed_values() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf(pending: Bool) -> String effects { time } {
            if pending { time.sleep(1ms); "a" } else { "b" }
        }
        fn middle() -> String effects { time } { leaf(true) + leaf(false) }
        fn main() effects { time, Check } {
            if middle() != "ab" { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    // The inline second leaf hands off through a fresh entry to restore the
    // saved frame, just like a completed asynchronous call.
    assert_eq!(machine_resumptions(&scope), 4);
}

#[test]
fn ordinary_pending_resumed_method_transfers_receiver_and_managed_spill() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        struct Part {
            let text: String = "a"
            fn get() -> String effects { time } { time.sleep(1ms); self.text }
        }
        fn middle() -> String effects { time } {
            let part = Part()
            part.get() + part.get()
        }
        fn main() effects { time, Check } {
            if middle() != "aa" { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 5);
}

#[test]
fn ordinary_pending_resumed_call_failure_releases_the_waiting_chain() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Missing { @suspends fn value() -> Int32 }
        fn first() -> Int32 effects { time } { time.sleep(1ms); 20 }
        fn second() -> Int32 effects { Missing } { Missing.value() }
        fn middle() -> Int32 effects { time, Missing } { first() + second() }
        fn main() effects { time, Missing } { println(middle()) }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let scope = RuntimeScope::new();
        let _guard = scope.enter();
        let mut backend = CraneliftBackend::new().unwrap();
        sender
            .send(backend.compile_and_run_program(&mir, &scope))
            .unwrap();
    });
    let error = receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    worker.join().unwrap();
    assert!(
        matches!(&error, CodegenError::RuntimeError { message } if message.contains("chain terminated with status 2")),
        "{error:?}"
    );
}

#[test]
fn ordinary_pending_loop_allocates_a_fresh_call_activation_on_each_resume() {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    unsafe extern "C" fn counter(
        handle: *mut c_void,
        operation: u64,
        _arguments: *const u8,
        _arguments_size: usize,
        _result: *mut u8,
        _result_size: usize,
    ) -> u8 {
        let value = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let bytes = value.to_ne_bytes();
        unsafe { complete_standalone_suspend(handle, operation, bytes.as_ptr(), bytes.len()) }
    }
    let mut mir = lower(
        r#"
        eff Counter { @suspends fn next() -> Int32 }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf() -> Int32 effects { Counter } { Counter.next() }
        fn middle() -> Int32 effects { Counter } {
            loop {
                let value = leaf()
                if value == 3 { break value * 14 } else { continue }
            }
        }
        fn main() effects { Counter, Check } {
            if middle() != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let effect = mir.types().effects().by_name("Counter").unwrap();
    let operation = mir
        .types()
        .effects()
        .operation_by_name(effect, "next")
        .unwrap();
    let id = (operation.effect.0 as u64) << 32 | operation.operation as u64;
    let _provider = unsafe {
        register_standalone_provider(&scope, id, counter as *mut c_void, std::ptr::null_mut())
    }
    .expect("counter provider registration");
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 7);
    assert_eq!(NEXT.load(std::sync::atomic::Ordering::Acquire), 3);
}

#[test]
fn ordinary_pending_call_chain_transfers_managed_results_and_frame_locals() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf() -> String effects { time } {
            time.sleep(1ms)
            "answer"
        }
        fn middle() -> String effects { time } {
            let saved = "the "
            let result = leaf()
            saved + result
        }
        fn main() effects { time, Check } {
            if middle() != "the answer" { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 3);
}

#[test]
fn ordinary_pending_method_preserves_receiver_across_nested_call() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf() -> Int32 effects { time } { time.sleep(1ms); 20 }
        struct Counter {
            let value: Int32 = 22
            fn answer() -> Int32 effects { time } { leaf() + self.value }
        }
        fn main() effects { time, Check } {
            if Counter().answer() != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 3);
}

#[test]
fn ordinary_pending_rejected_operation_propagates_failed_to_root() {
    let mut mir = lower(
        r#"
        eff Missing { @suspends fn value() -> Int32 }
        fn leaf() -> Int32 effects { Missing } { Missing.value() }
        fn middle() -> Int32 effects { Missing } { leaf() + 1 }
        fn main() effects { Missing } { println(middle()) }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    let error = backend.compile_and_run_program(&mir, &scope).unwrap_err();
    assert!(
        matches!(&error, CodegenError::RuntimeError { message } if message == "Pending root returned Failed"),
        "{error:?}"
    );
    scope.close_and_wait();
    assert_eq!(machine_resumptions(&scope), 0);
}

#[test]
fn ordinary_pending_ready_result_keeps_native_managed_ownership() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf(pending: Bool) -> String effects { time } {
            if pending { time.sleep(1ms); "answer" } else { "answer" }
        }
        fn middle() -> String effects { time } {
            let saved = "the "
            let result = leaf(false)
            saved + result
        }
        fn main() effects { time, Check } {
            if middle() != "the answer" { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 0);
}

unsafe extern "C" fn inline_provider(
    handle: *mut c_void,
    operation: u64,
    _arguments: *const u8,
    _arguments_size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    unsafe { ContinuationHandle::from_raw(handle) }
        .complete_suspend(operation)
        .into()
}

unsafe extern "C" fn inline_number_provider(
    handle: *mut c_void,
    operation: u64,
    arguments: *const u8,
    _arguments_size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    let value = unsafe { std::ptr::read_unaligned(arguments.cast::<i32>()) };
    let bytes = (value as u64).to_ne_bytes();
    unsafe { complete_standalone_suspend(handle, operation, bytes.as_ptr(), bytes.len()) }
}

#[test]
fn ordinary_pending_chain_resuspends_with_inline_provider_completion() {
    let mut mir = lower(
        r#"
        eff Fetch { @suspends fn number(value: Int32) -> Int32 }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf() -> Int32 effects { Fetch } {
            let first = Fetch.number(20)
            let second = Fetch.number(22)
            first + second
        }
        fn middle() -> Int32 effects { Fetch } { leaf() }
        fn main() effects { Fetch, Check } {
            if middle() != 42 { Check.failed() } else { println(42) }
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let effect = mir.types().effects().by_name("Fetch").unwrap();
    let operation = mir
        .types()
        .effects()
        .operation_by_name(effect, "number")
        .unwrap();
    let id = (operation.effect.0 as u64) << 32 | operation.operation as u64;
    let _provider = unsafe {
        register_standalone_provider(
            &scope,
            id,
            inline_number_provider as *mut c_void,
            std::ptr::null_mut(),
        )
    }
    .expect("inline provider registration");
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    assert_eq!(machine_resumptions(&scope), 4);
}

#[test]
fn ordinary_pending_chain_finishes_when_a_resumed_operation_is_rejected() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Missing { @suspends fn value() -> Int32 }
        fn leaf() -> Int32 effects { time, Missing } {
            time.sleep(1ms)
            Missing.value()
        }
        fn middle() -> Int32 effects { time, Missing } { leaf() + 1 }
        fn main() effects { time, Missing } { println(middle()) }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    // Bound the regression: a stranded parent must fail the test rather than
    // leave the entire test runner waiting forever for its scope lease.
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let scope = RuntimeScope::new();
        let _guard = scope.enter();
        let mut backend = CraneliftBackend::new().unwrap();
        let result = backend.compile_and_run_program(&mir, &scope);
        sender.send(result).unwrap();
    });
    let error = receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    worker.join().unwrap();
    assert!(
        matches!(&error, CodegenError::RuntimeError { message } if message.contains("chain terminated with status 2")),
        "{error:?}"
    );
}

unsafe extern "C" fn finish_leaf(handle: *mut c_void) {
    unsafe { ContinuationHandle::from_raw(handle) }.complete();
}

unsafe extern "C" fn finish_parent(handle: *mut c_void) {
    let handle = unsafe { ContinuationHandle::from_raw(handle) };
    unsafe {
        let sender =
            &*std::ptr::read_unaligned(handle.frame_pointer().cast::<*const Sender<(u8, u64)>>());
        let sender = sender.clone();
        let mut result = 0_u64;
        let status = handle.take_function_pending_result((&mut result as *mut u64).cast(), 8);
        handle.complete();
        sender.send((status as u8, result)).unwrap();
    }
}

#[test]
fn jit_pending_operation_imports_deliver_results_to_the_hidden_parent() {
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    const OPERATION: u64 = 0xfeed_3001;
    let _provider = unsafe {
        register_standalone_provider(
            &scope,
            OPERATION,
            inline_provider as *mut c_void,
            std::ptr::null_mut(),
        )
    }
    .expect("inline provider registration");
    let mut backend = CraneliftBackend::new().unwrap();
    let pointer = backend.module.target_config().pointer_type();
    let call_conv = backend.module.isa().default_call_conv();
    let ids =
        runtime_ids::declare_continuation_runtime_ids(&mut backend, pointer, call_conv).unwrap();
    for provider in [false, true] {
        let mut context = backend.module.make_context();
        context
            .func
            .signature
            .params
            .extend([AbiParam::new(pointer); 2]);
        context
            .func
            .signature
            .returns
            .push(AbiParam::new(types::I8));
        let entry = backend
            .module
            .declare_function(
                if provider {
                    "test_pending_provider"
                } else {
                    "test_pending_timer"
                },
                Linkage::Local,
                &context.func.signature,
            )
            .unwrap();
        let start = backend.module.declare_func_in_func(
            if provider {
                ids.continuation_start_pending_provider_id
            } else {
                ids.continuation_start_pending_timer_id
            },
            &mut context.func,
        );
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            let handle = builder.block_params(block)[0];
            let parent = builder.block_params(block)[1];
            let operation = builder.ins().iconst(types::I64, OPERATION as i64);
            let call = if provider {
                let null = builder.ins().iconst(pointer, 0);
                builder
                    .ins()
                    .call(start, &[handle, operation, null, null, parent])
            } else {
                let duration = builder.ins().iconst(types::I64, 1);
                builder
                    .ins()
                    .call(start, &[handle, operation, duration, parent])
            };
            let status = builder.inst_results(call)[0];
            builder.ins().return_(&[status]);
            builder.seal_all_blocks();
            builder.finalize(backend.module.target_config());
        }
        backend.module.define_function(entry, &mut context).unwrap();
        backend.module.finalize_definitions().unwrap();
        let address = backend.module.get_finalized_function(entry);
        let run: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u8 =
            unsafe { std::mem::transmute(address) };
        let leaf = ContinuationHandle::new(&scope, 0);
        let parent = ContinuationHandle::new(&scope, 0);
        let (sender, receiver) = mpsc::channel::<(u8, u64)>();
        unsafe {
            let slot = leaf.allocate_result(8);
            std::ptr::write_unaligned(slot.cast::<u64>(), 42);
            assert!(leaf.set_resume_callback(finish_leaf as *mut c_void));
            parent.allocate_suspend_result(8);
            let frame = parent.allocate_frame(std::mem::size_of::<usize>());
            std::ptr::write_unaligned(frame.cast::<*const Sender<(u8, u64)>>(), &sender);
            assert!(parent.set_resume_callback(finish_parent as *mut c_void));
            assert!(parent.begin_function_pending());
        }
        assert_eq!(
            with_function_pending_boundary(&scope, || {
                assert_eq!(
                    unsafe { run(leaf.as_raw(), parent.as_raw()) },
                    FunctionCallStatus::Pending as u8
                );
                assert_eq!(parent.poll_function_pending(), FunctionCallStatus::Pending);
                assert!(
                    receiver.try_recv().is_err(),
                    "no resume inside the native boundary"
                );
                FunctionCallStatus::Pending
            }),
            FunctionCallStatus::Pending
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            (FunctionCallStatus::Ready as u8, 42)
        );
        leaf.free();
        parent.free();
    }
    scope.wait_for_idle();
}

#[test]
fn ordinary_pending_call_aborts_during_suspension() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf() -> Int32 effects { time } {
            time.sleep(1ms)
            42
        }
        fn middle() -> Int32 effects { time, Check } {
            let x = leaf()
            Check.failed()
            x
        }
        fn main() effects { time, Check } {
            println(middle())
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    let error = backend
        .compile_and_run_program(&mir, &scope)
        .expect_err("the unhandled abort must reach the caller after worker resumption");
    assert!(error
        .message()
        .contains("unhandled aborting effect operation 'Check.failed'"));
    // There is no handler in this program; worker migration must not hide its failure.
}

#[test]
fn ordinary_pending_call_with_multiple_suspensions_then_abort() {
    let mut mir = lower(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Check { @aborts fn failed() -> Unit }
        fn leaf(value: Int32) -> Int32 effects { time } {
            time.sleep(1ms)
            value
        }
        fn middle() -> Int32 effects { time, Check } {
            let x = leaf(10)
            let y = leaf(20)
            Check.failed()
            x + y
        }
        fn main() effects { time, Check } {
            println(middle())
        }
    "#,
    );
    let suspending = crate::mir::suspending_analysis::compute_suspending_functions(&mir.functions);
    crate::mir::suspending_analysis::materialize_direct_call_continuations(
        &mut mir.functions,
        &suspending,
    );
    mir.verify().unwrap();
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let mut backend = CraneliftBackend::new().unwrap();
    let error = backend
        .compile_and_run_program(&mir, &scope)
        .expect_err("the unhandled abort must reach the caller after worker resumption");
    assert!(error
        .message()
        .contains("unhandled aborting effect operation 'Check.failed'"));
    // The failure after both suspensions must reach the host thread.
}
