use super::*;

#[test]
fn pending_main_result_propagates_error_without_leaking_payload() {
    let error = try_run_program(
        r#"
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            fn fail() -> Result(Int32, String) effects { time } {
                time.sleep(1ms)
                Err("pending entry failed")
            }
            fn main() -> Result(Unit, String) effects { time } {
                let value = fail()?
                println(value)
                Ok(())
            }
        "#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("pending entry failed"));
}

#[test]
fn concurrent_compilers_keep_machine_entries_isolated() {
    let start = Arc::new(Barrier::new(2));
    let source = r#"
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            fn wait() -> Int32 effects { time } { time.sleep(5ms); 42 }
            fn main() effects { time } {
                let values = parallel {
                    | wait()
                }
                println(values.0)
            }
        "#;
    thread::scope(|scope| {
        let first_start = Arc::clone(&start);
        let first = scope.spawn(move || {
            first_start.wait();
            run_program(source);
        });
        let second_start = Arc::clone(&start);
        let second = scope.spawn(move || {
            second_start.wait();
            run_program(source);
        });
        first.join().expect("first compiler thread should finish");
        second.join().expect("second compiler thread should finish");
    });
}

#[test]
fn concurrent_compilers_preserve_managed_values_across_suspension() {
    let start = Arc::new(Barrier::new(2));
    let source = r#"
            eff time { @suspends fn sleep(duration: Duration) -> Unit }
            fn wait(message: String) -> String effects { time } {
                time.sleep(5ms)
                message
            }
            fn main() effects { time } { println(wait("scope-owned value")) }
        "#;
    thread::scope(|scope| {
        let first_start = Arc::clone(&start);
        let first = scope.spawn(move || {
            first_start.wait();
            run_program(source);
        });
        let second_start = Arc::clone(&start);
        let second = scope.spawn(move || {
            second_start.wait();
            run_program(source);
        });
        first.join().expect("first compiler thread should finish");
        second.join().expect("second compiler thread should finish");
    });
}

#[test]
fn suspended_task_returns_pending_and_resumes_on_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn sleeper() -> Unit effects { time } { time.sleep(1ms); println(\"resumed\") }\n\
                 fn main() effects { time } { let ignored = parallel {\n| sleeper()\n}; println(\"joined\") }",
            )
            .expect("suspended task should resume through its machine entry");
}

#[test]
fn suspended_task_resumes_a_scalar_result_through_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait() -> Int32 effects { time } { time.sleep(1ms); 42 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait()\n}; println(values.0) }",
            )
            .expect("scalar continuation result should resume through its machine entry");
}

#[test]
fn typed_suspending_operation_resumes_with_zero_initialized_result() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Input { @suspends fn read() -> Int32 }\n\
                 fn read() -> Int32 effects { Input } { Input.read() }\n\
                 fn main() effects { Input } { let values = parallel {\n| read()\n}; println(values.0) }",
            )
            .expect("typed suspending operation should complete through its continuation");
}

#[test]
fn typed_managed_suspending_operation_resumes_with_empty_result() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Input { @suspends fn read() -> String }\n\
                 fn read() -> String effects { Input } { Input.read() }\n\
                 fn main() effects { Input } { let values = parallel {\n| read()\n}; println(values.0) }",
            )
            .expect("managed suspending operation should restore its result payload");
}
#[test]
fn typed_suspending_operation_accepts_non_timer_arguments() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Input { @suspends fn read(prompt: String) -> Int32 }\n\
                 fn read() -> Int32 effects { Input } { Input.read(prompt: \"age\") }\n\
                 fn main() effects { Input } { let values = parallel {\n| read()\n}; println(values.0) }",
            )
            .expect("typed suspending operation should lower a generic argument payload");
}
#[test]
fn timer_backed_suspending_operation_can_return_a_scalar() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff Input { @suspends fn read(duration: Duration) -> Int32 }\n\
                 fn read() -> Int32 effects { Input } { Input.read(1ms) }\n\
                 fn main() effects { Input } { let values = parallel {\n| read()\n}; println(values.0) }",
            )
            .expect("timer-backed typed operation should restore its scalar result");
}

#[test]
fn repeated_suspending_operations_can_change_result_layout() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff Input {
                     @suspends fn number() -> Int32
                     @suspends fn text() -> String
                 }
                 fn load() -> String effects { Input } {
                     let number = Input.number()
                     let text = Input.text()
                     if number == 0 { text } else { text }
                 }
                 fn main() effects { Input } {
                     let values = parallel {
                         | load()
                     }
                     println(values.0)
                 }",
        )
        .expect("successive suspensions should rebuild typed result storage");
}
#[test]
fn suspended_task_can_suspend_again_from_a_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait() -> Int32 effects { time } {\n\
                     let initial = 40\n\
                     time.sleep(1ms)\n\
                     let next = initial + 1\n\
                     time.sleep(1ms)\n\
                     let final = next + 1\n\
                     time.sleep(1ms)\n\
                     final\n\
                 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait()\n}; println(values.0) }",
            )
            .expect("a machine entry should be able to schedule its next suspension");
}

#[test]
fn custom_duration_suspend_uses_the_payload_path_without_timer_special_casing() {
    use joky_runtime::host::testing::{
        complete_suspend_with_payload, machine_resumptions, register_suspend_provider,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    static REQUESTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn provider(
        handle: *mut std::ffi::c_void,
        operation: u64,
        arguments: *const u8,
        arguments_size: usize,
        _result: *mut u8,
        result_size: usize,
    ) -> u8 {
        assert_eq!(arguments_size, 8);
        assert_eq!(result_size, 0);
        assert_eq!(unsafe { arguments.cast::<u64>().read_unaligned() }, 1);
        REQUESTS.fetch_add(1, Ordering::SeqCst);
        unsafe { complete_suspend_with_payload(handle, operation, std::ptr::null(), 0) }
    }
    let source = r#"
                    eff Timer { @suspends fn wait(duration: Duration) -> Unit }

                    fn wait_for_turns() -> Int32 effects { Timer } {
                        Timer.wait(1ms)
                        Timer.wait(1ms)
                        42
                    }

                    fn main() effects { Timer } {
                        println(wait_for_turns())
                    }
                "#;
    let program = crate::syntax::parse_program(source).unwrap();
    let types = crate::sema::check_program(&program).unwrap();
    let core = crate::hir::CoreProgram::lower(program, types).unwrap();
    let mut mir = crate::mir::MirProgram::lower(&core).unwrap();
    crate::mir::passes::MirPassManager::default_pipeline()
        .run(&mut mir)
        .unwrap();
    let effect = mir.types().effects().by_name("Timer").unwrap();
    let operation = mir
        .types()
        .effects()
        .operation_by_name(effect, "wait")
        .unwrap();
    let operation = (operation.effect.0 as u64) << 32 | operation.operation as u64;
    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    let _provider = unsafe {
        register_suspend_provider(&scope, operation, provider as *mut _, std::ptr::null_mut())
    }
    .expect("custom timer provider registration");
    crate::codegen::CraneliftBackend::new()
        .unwrap()
        .compile_and_run_program(&mir, &scope)
        .expect("custom Duration suspends should use the generic payload path");
    assert_eq!(REQUESTS.load(Ordering::SeqCst), 2);
    assert_eq!(machine_resumptions(&scope), 3);
}

#[test]
fn repeated_suspension_preserves_a_managed_value() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(message: String) -> String effects { time } {\n\
                     time.sleep(1ms)\n\
                     let updated = message.concat(\"!\")\n\
                     time.sleep(1ms)\n\
                     updated\n\
                 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(\"awake\")\n}; println(values.0) }",
            )
            .expect("managed values should survive repeated suspension");
}

#[test]
fn cancelling_a_repeated_suspension_releases_its_managed_frame() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn slow() -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     let message = \"waiting\"\n\
                     time.sleep(100ms)\n\
                     if message.is_empty() { 0 } else { 1 }\n\
                 }\n\
                 fn winner() -> Int32 effects { time } { time.sleep(5ms); 42 }\n\
                 fn main() effects { time } { let value = race {\n| slow()\n| winner()\n}; println(value) }",
            )
            .expect("cancelling a re-suspended task should not leak its frame values");
}

#[test]
fn cancelling_a_repeated_suspension_handles_changed_frame_values() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Token { let value: Int32 = 1 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn slow() -> Int32 effects { time } {\n\
                     let token = Token()\n\
                     time.sleep(1ms)\n\
                     let value = token.value\n\
                     let message = \"waiting\"\n\
                     time.sleep(100ms)\n\
                     if message.is_empty() { value } else { value }\n\
                 }\n\
                 fn winner() -> Int32 effects { time } { time.sleep(5ms); 42 }\n\
                 fn main() effects { time } { let value = race {\n| slow()\n| winner()\n}; println(value) }",
            )
            .expect("cancelling a re-suspended task should clean changing frame layouts");
}

#[test]
fn suspended_task_resumes_calls_and_loops_through_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn increment(value: Int32) -> Int32 { value + 1 }\n\
                 fn wait() -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     loop {\n\
                         let value = increment(value: 41)\n\
                         if value == 42 { break value } else { continue }\n\
                     }\n\
                 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait()\n}; println(values.0) }",
            )
            .expect("resume tail calls and loops should use the machine entry");
}

#[test]
fn suspended_task_resumes_method_and_indirect_calls_through_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Counter {\n\
                     let base: Int32\n\
                     fn add(value: Int32) -> Int32 { self.base + value }\n\
                 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(counter: Counter, callback: fn(input: Int32) -> Int32) -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     callback(input: counter.add(value: 1))\n\
                 }\n\
                 fn main() effects { time } {\n\
                     let offset = 40\n\
                     let callback = fn (input: Int32) -> Int32 { input + offset }\n\
                     let values = parallel {\n| wait(counter: Counter(base: 1), callback: callback)\n}\n\
                     println(values.0)\n\
                 }",
            )
            .expect("method and indirect calls should resume through the machine entry");
}

#[test]
fn looped_borrowed_method_receivers_are_not_spilled_across_suspends() {
    // A suspending method call borrows its receiver. Inside a loop the
    // borrow is an operand of the suspend itself, and the loop back-edge
    // makes the suspend block reachable from the resume; the spill set must
    // not list that borrowed operand (the runtime hands suspend operands to
    // the provider through the operation argument storage).
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             class Gauge {\n\
                 let base: Int32 = 40\n\
                 fn bump() -> Int32 effects { time } {\n\
                     let offset = self.base\n\
                     time.sleep(1ms)\n\
                     offset + 2\n\
                 }\n\
             }\n\
             fn work(gauge: Gauge) -> Int32 effects { time } {\n\
                 loop {\n\
                     let value = gauge.bump()\n\
                     if value == 42 { break value } else { continue }\n\
                 }\n\
             }\n\
             fn main() effects { time } {\n\
                 let results = parallel {\n| work(Gauge())\n}\n\
                 println(results.0)\n\
             }",
        )
        .expect("a borrowed method receiver inside a loop should compile and resume");
    assert_eq!(compiler.take_warnings().len(), 0);
}

#[test]
fn borrowed_method_receiver_used_across_a_suspend_is_rejected() {
    // D1 decision: a borrowed receiver may not be carried across a suspend.
    // The minimal use case reads `self` after the suspend; the verifier must
    // reject it and point at reading or copying the field before suspend.
    let error = try_run_program(
        "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
         class Gauge {\n\
             let base: Int32 = 40\n\
             fn bump() -> Int32 effects { time } {\n\
                 time.sleep(1ms)\n\
                 self.base + 2\n\
             }\n\
         }\n\
         fn main() effects { time } {\n\
             let results = parallel {\n| Gauge().bump() }\n\
             println(results.0)\n\
         }",
    )
    .expect_err("a borrowed self used across a suspend must be rejected");
    assert!(
        error.to_string().contains("cannot carry borrowed local")
            && error.to_string().contains("before suspend"),
        "unexpected diagnostic: {error}"
    );
}

#[test]
fn shared_suspension_argument_survives_resume_without_leaking() {
    // MIR gives each shared argument its own ownership share at the suspend;
    // the share transfers to payload cleanup while the caller keeps its own.
    // Both sides must stay usable across the suspend and release exactly once.
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             class Bag {\n\
                 let tag: Int32\n\
                 fn describe() -> Int32 { self.tag + 1 }\n\
             }\n\
             fn hold(bag: Bag) -> Int32 effects { time } {\n\
                 time.sleep(1ms)\n\
                 bag.describe()\n\
             }\n\
             fn main() effects { time } {\n\
                 let bag = Bag(tag: 5)\n\
                 let results = parallel {\n| hold(Bag(tag: 5)) }\n\
                 println(results.0 + bag.describe())\n\
             }",
        )
        .expect("a shared parameter live across a suspend should compile and resume");
    assert_eq!(compiler.take_warnings().len(), 0);
}

#[test]
fn suspended_task_creates_and_calls_a_closure_after_resume() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(offset: Int32) -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     let callback = fn (left: Int32, right: Int32) -> Int32 { left + right + offset }\n\
                     callback(right: 1, left: 0)\n\
                 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(offset: 41)\n}; println(values.0) }",
            )
            .expect("a closure created after resume should run through the machine entry");
}

#[test]
fn suspended_task_calls_a_suspending_closure_after_resume() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn wait() -> Int32 effects { time } {\n\
                 time.sleep(1ms)\n\
                 let callback = fn () -> Int32 { time.sleep(1ms); 42 }\n\
                 callback()\n\
             }\n\
             fn main() effects { time } { let values = parallel {\n| wait()\n}; println(values.0) }",
        )
        .expect("a suspending closure should resume through an indirect Pending call");
}

#[test]
fn suspending_recursion_uses_independent_heap_continuations() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn countdown(n: Int32) -> Int32 effects { time } {\n\
                 if n == 0 { 0 } else { time.sleep(1ms); countdown(n - 1) + 1 }\n\
             }\n\
             fn main() effects { time } { let values = parallel {\n| countdown(3)\n}; println(values.0) }",
        )
        .expect("suspending recursion should use independent continuation frames");
}

#[test]
fn suspended_task_drops_owned_values_created_after_resume() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Token { let value: Int32 = 42 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait() -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     let token = Token()\n\
                     { let message = \"awake\"; println(message) }\n\
                     token.value\n\
                 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait()\n}; println(values.0) }",
            )
            .expect("owned values created after resume should be cleaned up by the machine entry");
}

#[test]
fn race_handles_owned_loop_break_after_resume() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Token { let value: Int32 = 1 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn spin() -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     loop {\n\
                         let token = Token()\n\
                         break 0\n\
                     }\n\
                 }\n\
                 fn winner() -> Int32 effects { time } { time.sleep(5ms); 42 }\n\
                 fn main() effects { time } { let value = race {\n| spin()\n| winner()\n}; println(value) }",
            )
            .expect("race should clean up loop values when a machine entry breaks normally");
}

#[test]
fn race_cancels_a_resumed_loop_at_its_machine_poll() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn spin() -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     loop { if false { break 0 } else { continue } }\n\
                 }\n\
                 fn winner() -> Int32 effects { time } { time.sleep(5ms); 42 }\n\
                 fn main() effects { time } { let value = race {\n| spin()\n| winner()\n}; println(value) }",
            )
            .expect("race cancellation should stop a loop resumed by a machine entry");
}

#[test]
fn suspended_task_restores_scalar_parameter_in_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(value: Int32) -> Int32 effects { time } { time.sleep(1ms); value + 1 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(41)\n| wait(-41)\n}; println(values.0); println(values.1) }",
            )
            .expect("scalar continuation parameters should restore through its machine entry");
}

#[test]
fn suspended_task_resumes_if_cfg_through_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(value: Int32) -> Int32 effects { time } { time.sleep(1ms); if value > 0 { value + 1 } else { value - 1 } }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(41)\n| wait(-41)\n}; println(values.0); println(values.1) }",
            )
            .expect("if CFG after suspend should resume through its machine entry");
}

#[test]
fn suspended_task_resumes_option_match_cfg_through_machine_entry() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(value: Option(Int32)) -> Int32 effects { time } { time.sleep(1ms); match value { Some(number) => number + 1; None => 0 } }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(Some(41))\n| wait(None)\n}; println(values.0); println(values.1) }",
            )
            .expect("Option match CFG after suspend should resume through its machine entry");
}

#[test]
fn suspended_task_creates_and_uses_persistent_collections_after_resume() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait() -> Int32 effects { time } {\n\
                     time.sleep(1ms)\n\
                     let values = List#{1, 2, 3}.push_front(0).reverse()\n\
                     let users = Map#{\"answer\" => 40}.insert(\"two\", 2)\n\
                     values.head().unwrap_or(0) + users.get(\"answer\").unwrap_or(0)\n\
                 }\n\
                 fn main() effects { time } { let values = parallel {\n| wait()\n}; println(values.0) }",
            )
            .expect("persistent List and Map calls after suspend should use the machine entry");
}

#[test]
fn suspended_task_creates_and_uses_mutable_collections_after_resume() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    @intrinsic
                    class MutList(T: type) {
                        fn push(&self, item: T) -> Unit
                        fn get(&self, index: UInt64) -> Option(T)
                        fn set(&self, index: UInt64, item: T) -> Unit
                    }

                    @intrinsic
                    class MutMap(K: type + Hash + Eq, V: type) {
                        fn get(&self, key: K) -> Option(V)
                        fn insert(&self, key: K, value: V) -> Option(V)
                    }

                    eff time { @suspends fn sleep(duration: Duration) -> Unit }

                    fn wait() -> Int32 effects { time } {
                        time.sleep(1ms)
                        let values = MutList#{1}
                        values.push(2)
                        values.set(0, 40)
                        let users = MutMap#{"answer" => 1}
                        let _ = users.insert("answer", 2)
                        values.get(0).unwrap_or(0) + users.get("answer").unwrap_or(0)
                    }

                    fn main() effects { time } {
                        let values = parallel {
                            | wait()
                        }
                        println(values.0)
                    }
                "#,
        )
        .expect("mutable collection calls after suspend should use the machine entry");
}

#[test]
fn suspended_task_preserves_normal_handler_cfg_through_machine_entry() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff time { @suspends fn sleep(duration: Duration) -> Unit }
                    eff Console { fn read(prompt: String) -> Int32 }

                    fn wait() -> Int32 effects { time } {
                        do {
                            time.sleep(1ms)
                            Console.read(prompt: "age")
                        } with {
                            Console.read(prompt) => 42
                        }
                    }

                    fn main() effects { time } {
                        let values = parallel {
                            | wait()
                        }
                        println(values.0)
                    }
                "#,
        )
        .expect("normal handler CFG after suspend should resume through its machine entry");
}

#[test]
fn suspended_task_moves_string_result_out_of_machine_entry_frame() {
    Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(message: String) -> String effects { time } { time.sleep(1ms); message }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(\"awake\")\n}; println(values.0) }",
            )
            .expect("managed continuation result should move out of its frame exactly once");
}

#[test]
fn suspended_task_moves_class_result_out_of_machine_entry_frame() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Counter { let value: Int32 = 42 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(counter: Counter) -> Counter effects { time } { time.sleep(1ms); counter }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(Counter())\n}; println(values.0.value) }",
            )
            .expect("owned continuation result should move out of its frame exactly once");
}

#[test]
fn suspended_task_moves_tuple_class_result_out_of_machine_entry_frame() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Counter { let value: Int32 = 42 }\n\
                 struct Pair { let counter: Counter\n let marker: Int32 = 7 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(pair: Pair) -> Pair effects { time } { time.sleep(1ms); pair }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(Pair(counter: Counter()))\n}; println(values.0.counter.value) }",
            )
            .expect("aggregate continuation result should move every owned leaf out of its frame");
}

#[test]
fn suspended_task_moves_active_option_payload_out_of_machine_entry_frame() {
    Compiler::new()
            .unwrap()
            .run_program(
                "class Counter { let value: Int32 = 42 }\n\
                 eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 fn wait(value: Option(Counter)) -> Option(Counter) effects { time } { time.sleep(1ms); value }\n\
                 fn main() effects { time } { let values = parallel {\n| wait(Some(Counter()))\n}; match values.0 { Some(counter) => println(counter.value); None => println(0) } }",
            )
            .expect("active option payload should move out of its continuation frame exactly once");
}

#[test]
fn suspended_task_moves_active_enum_payload_out_of_machine_entry_frame() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter {
                        let value: Int32 = 42
                    }

                    enum Outcome {
                        Ready(value: Counter)
                        Empty
                    }

                    eff time { @suspends fn sleep(duration: Duration) -> Unit }

                    fn wait(value: Outcome) -> Outcome effects { time } {
                        time.sleep(1ms)
                        value
                    }

                    fn main() effects { time } {
                        let values = parallel {
                            | wait(Outcome.Ready(value: Counter()))
                        }
                        match values.0 {
                            Outcome.Ready(value: counter) => println(counter.value)
                            Outcome.Empty => println(0)
                        }
                    }
                "#,
        )
        .expect("active enum payload should move out of its continuation frame exactly once");
}

#[test]
fn suspended_task_moves_active_result_payload_out_of_machine_entry_frame() {
    Compiler::new()
            .unwrap()
            .run_program(
                r#"
                    class Counter {
                        let value: Int32 = 42
                    }

                    eff time { @suspends fn sleep(duration: Duration) -> Unit }

                    fn wait(value: Result(Int32, Counter)) -> Result(Int32, Counter) effects { time } {
                        time.sleep(1ms)
                        value
                    }

                    fn main() effects { time } {
                        let values = parallel {
                            | wait(Err(Counter()))
                        }
                        match values.0 {
                            Ok(value) => println(value)
                            Err(error) => println(error.value)
                        }
                    }
                "#,
            )
            .expect("active result payload should move out of its continuation frame exactly once");
}

#[test]
fn cancelling_a_suspended_task_drops_its_tagged_frame_payload() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Token {
                        let value: Int32 = 1
                    }

                    eff time { @suspends fn sleep(duration: Duration) -> Unit }

                    fn slow(value: Option(Token)) -> Option(Token) effects { time } {
                        time.sleep(100ms)
                        value
                    }

                    fn fast() -> Option(Token) effects { time } {
                        time.sleep(1ms)
                        None
                    }

                    fn main() effects { time } {
                        let winner = race {
                            | slow(Some(Token()))
                            | fast()
                        }
                        let missing = winner.is_none()
                    }
                "#,
        )
        .expect("cancelling a suspended tagged value should release its active payload");
}

#[test]
fn nested_suspending_calls_resume_through_machine_entries_and_keep_values() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn inner() -> Int32 effects { time } { time.sleep(1ms); 40 }\n\
             fn outer() -> Int32 effects { time } {\n\
                 time.sleep(1ms)\n\
                 let ignored = parallel {\n\
                     | inner()\n\
                     | inner()\n\
                 }\n\
                 inner() + 2\n\
             }\n\
             fn main() effects { time } {\n\
                 let value = parallel {\n\
                     | outer()\n\
                 }\n\
                 println(value.0)\n\
             }",
        )
        .expect("nested suspending calls inside machine entries should resume asynchronously");
    // `outer` publishes pending from the task body; its resumed tail's calls
    // to `inner` return Pending, exit the machine entry, and the caller's
    // continuation entry is rescheduled once the callee completes.
    let warnings = compiler.take_warnings();
    assert!(
        warnings
            .iter()
            .all(|line| !line.contains("function 'outer'")),
        "{warnings:?}"
    );
}

#[test]
fn nested_suspending_callee_failure_propagates_to_the_enclosing_handler() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Boom { @aborts fn stop(message: String) -> Unit }\n\
             fn inner() -> Int32 effects { time, Boom } {\n\
                 time.sleep(30ms)\n\
                 Boom.stop(message: \"inner-boom\")\n\
                 0\n\
             }\n\
             fn outer() -> Int32 effects { time, Boom } {\n\
                 time.sleep(1ms)\n\
                 inner()\n\
             }\n\
             fn main() -> Unit effects { time, Boom } {\n\
                 do {\n\
                     let value = parallel {\n\
                         | outer()\n\
                     }\n\
                     println(value.0)\n\
                 } with {\n\
                     Boom.stop(message) => println(message)\n\
                 }\n\
             }",
        )
        .expect("a failure after a nested callee's suspend should reach the enclosing handler");
}

#[test]
fn nested_suspending_callee_completes_late_while_a_sibling_finishes_first() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn slow_inner() -> Int32 effects { time } { time.sleep(30ms); 40 }\n\
             fn slow_outer() -> Int32 effects { time } {\n\
                 time.sleep(1ms)\n\
                 slow_inner() + 2\n\
             }\n\
             fn quick() -> Int32 effects { time } { time.sleep(1ms); 7 }\n\
             fn main() effects { time } {\n\
                 let results = parallel {\n\
                     | slow_outer()\n\
                     | quick()\n\
                 }\n\
                 println(results.0)\n\
                 println(results.1)\n\
             }",
        )
        .expect("a caller waiting on a late nested callee must not block its sibling");
}

#[test]
fn nested_suspending_calls_repeat_inside_loops() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn once(n: Int32) -> Int32 effects { time } { time.sleep(1ms); n + 1 }\n\
             fn repeated() -> Int32 effects { time } {\n\
                 loop {\n\
                     let first = once(1)\n\
                     let second = once(2)\n\
                     if first == 2 { break second } else { continue }\n\
                 }\n\
             }\n\
             fn main() effects { time } {\n\
                 let value = parallel {\n\
                     | repeated()\n\
                 }\n\
                 println(value.0)\n\
             }",
        )
        .expect("repeated nested suspending calls inside a loop should each get a fresh handle");
}

#[test]
fn machine_entry_returns_owned_value_merged_from_multiple_resume_blocks() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn pick(mode: Bool, message: String) -> String effects { time } {\n\
                 time.sleep(1ms)\n\
                 if mode { message } else { message }\n\
             }\n\
             fn fresh(mode: Bool) -> String effects { time } {\n\
                 time.sleep(1ms)\n\
                 if mode { \"yes\" } else { \"no\" }\n\
             }\n\
             fn main() effects { time } {\n\
                 let results = parallel {\n\
                     | pick(mode: true, message: \"kept\")\n\
                     | pick(mode: false, message: \"kept\")\n\
                     | fresh(mode: true)\n\
                     | fresh(mode: false)\n\
                 }\n\
                 println(results.0)\n\
                 println(results.1)\n\
                 println(results.2)\n\
                 println(results.3)\n\
             }",
        )
        .expect("owned multi-block resume tails should run on machine entries");
    let warnings = compiler.take_warnings();
    assert!(
        warnings
            .iter()
            .all(|line| !line.contains("function 'pick'")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .all(|line| !line.contains("function 'fresh'")),
        "{warnings:?}"
    );
}

#[test]
fn machine_entry_returns_tagged_owned_value_merged_from_multiple_resume_blocks() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn tag(mode: Bool) -> Option(String) effects { time } {\n\
                 time.sleep(1ms)\n\
                 if mode { Some(\"payload\") } else { None }\n\
             }\n\
             fn main() effects { time } {\n\
                 let results = parallel {\n\
                     | tag(mode: true)\n\
                     | tag(mode: false)\n\
                 }\n\
                 println(results.0.unwrap_or(\"missing\"))\n\
             }",
        )
        .expect("tagged owned multi-block resume tails should run on machine entries");
    let warnings = compiler.take_warnings();
    assert!(
        warnings.iter().all(|line| !line.contains("function 'tag'")),
        "{warnings:?}"
    );
}

#[test]
fn machine_entry_resume_tail_dispatches_normal_handler_requests() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Console { fn read() -> Int32 }\n\
             fn ask() -> Int32 effects { time, Console } {\n\
                 time.sleep(1ms)\n\
                 Console.read()\n\
             }\n\
             fn main() -> Unit effects { time, Console } {\n\
                 let value = do { let results = parallel {\n\
                     | ask()\n\
                 }; results.0 } with {\n\
                     Console.read() => 42\n\
                 }\n\
                 println(value)\n\
             }",
        )
        .expect("resumed tail should dispatch normal requests through the pinned handler chain");
    let warnings = compiler.take_warnings();
    assert!(
        warnings.iter().all(|line| !line.contains("function 'ask'")),
        "{warnings:?}"
    );
}

#[test]
fn machine_entry_resume_tail_accepts_managed_request_results() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Store { fn load() -> String }\n\
             fn load() -> String effects { time, Store } {\n\
                 time.sleep(1ms)\n\
                 Store.load()\n\
             }\n\
             fn main() -> Unit effects { time, Store } {\n\
                 let value = do { let results = parallel {\n\
                     | load()\n\
                     | load()\n\
                 }; results.0 + results.1 } with {\n\
                     Store.load() => \"half+\"\n\
                 }\n\
                 println(value)\n\
             }",
        )
        .expect("managed request results should transfer through machine entry resume tails");
    let warnings = compiler.take_warnings();
    assert!(
        warnings
            .iter()
            .all(|line| !line.contains("function 'load'")),
        "{warnings:?}"
    );
}

#[test]
fn machine_entry_resume_tail_aborts_with_typed_failure() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Boom { @aborts fn stop(message: String) -> Unit }\n\
             fn task() -> Unit effects { time, Boom } {\n\
                 time.sleep(1ms)\n\
                 Boom.stop(message: \"boom\")\n\
             }\n\
             fn main() -> Unit effects { time, Boom } {\n\
                 do {\n\
                     let ignored = parallel {\n\
                         | task()\n\
                     }\n\
                     println(\"joined\")\n\
                 } with {\n\
                     Boom.stop(message) => println(message)\n\
                 }\n\
             }",
        )
        .expect("aborting from a machine entry resume tail should reach the parent handler");
    let warnings = compiler.take_warnings();
    assert!(
        warnings
            .iter()
            .all(|line| !line.contains("function 'task'")),
        "{warnings:?}"
    );
}

#[test]
fn machine_entry_resume_tail_aborts_without_payload() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Boom { @aborts fn stop() -> Unit }\n\
             fn task() -> Unit effects { time, Boom } {\n\
                 time.sleep(1ms)\n\
                 Boom.stop()\n\
             }\n\
             fn main() -> Unit effects { time, Boom } {\n\
                 do {\n\
                     let ignored = parallel {\n\
                         | task()\n\
                     }\n\
                     println(\"joined\")\n\
                 } with {\n\
                     Boom.stop() => println(\"aborted\")\n\
                 }\n\
             }",
        )
        .expect("payload-free aborts from resume tails should reach the parent handler");
}

#[test]
fn cancellation_reaches_tasks_blocked_in_nested_suspends() {
    let mut compiler = Compiler::new().unwrap();
    compiler
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Boom { @aborts fn stop(message: String) -> Unit }\n\
             fn inner() -> Int32 effects { time } {\n\
                 time.sleep(50ms)\n\
                 1\n\
             }\n\
             fn slow() -> Unit effects { time } {\n\
                 time.sleep(1ms)\n\
                 let ignored = inner()\n\
             }\n\
             fn failer() -> Unit effects { time, Boom } {\n\
                 time.sleep(2ms)\n\
                 Boom.stop(message: \"boom\")\n\
             }\n\
             fn main() -> Unit effects { time, Boom } {\n\
                 do {\n\
                     race {\n\
                         | slow()\n\
                         | failer()\n\
                     }\n\
                     println(\"completed\")\n\
                 } with {\n\
                     Boom.stop(message) => println(message)\n\
                 }\n\
             }",
        )
        .expect("cancellation should reach a task blocked in a nested suspend");
}

#[test]
fn suspended_task_resumes_a_list_result_through_machine_entry() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn wait() -> List(Int32) effects { time } {\n\
                 time.sleep(1ms)\n\
                 List(40, 2)\n\
             }\n\
             fn main() effects { time } {\n\
                 let values = parallel {\n| wait()\n}\n\
                 println(values.0.length())\n\
             }",
        )
        .expect("a list result should move through continuation result storage");
}

#[test]
fn cancelling_a_suspended_task_drops_its_list_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn slow() -> List(Int32) effects { time } {\n\
                 time.sleep(100ms)\n\
                 List(1, 2, 3)\n\
             }\n\
             fn fast() -> List(Int32) effects { time } {\n\
                 time.sleep(1ms)\n\
                 List(4)\n\
             }\n\
             fn main() effects { time } {\n\
                 let winner = race {\n| slow()\n| fast()\n}\n\
                 println(winner.length())\n\
             }",
        )
        .expect("cancelling a suspended list result should release it exactly once");
}

#[test]
fn degenerate_direct_suspend_in_task_branch_resumes_asynchronously() {
    // No suspending CALL anywhere: the branch body suspends directly. This
    // used to take the synchronous nested path and block the worker; it must
    // now publish pending and resume through a machine entry.
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             fn main() effects { time } {\n\
                 let results = parallel {\n\
                     | { time.sleep(1ms); 42 }\n\
                 }\n\
                 println(results.0)\n\
             }",
        )
        .expect("a direct suspend in a task branch should resume asynchronously");
}

#[test]
#[ignore = "stress: run manually or in a dedicated CI job; loops a cancellation race that once hung (~1/10 runs)"]
fn stress_cancellation_racing_nested_suspend_completions() {
    // Regression harness for the machine-entry dispatch vs cancellation race
    // fixed in 6f05684. A single pass rarely loses the race, so loop until a
    // time budget is exhausted and fail on the first hang, leak, or error.
    // Instrumented runs require a separately calibrated throughput floor.
    // Keep the normal floor unless the runner explicitly overrides it; never
    // turn a sanitizer report into success by lowering this liveness check.
    let minimum = std::env::var("JOKY_STRESS_MIN_ITERATIONS")
        .map(|value| {
            value
                .parse::<usize>()
                .expect("positive stress iteration floor")
        })
        .unwrap_or(300);
    assert!(minimum > 0, "stress iteration floor must be positive");
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(60);
    let mut iterations = 0;
    let mut iteration_times = Vec::new();
    while std::time::Instant::now() < deadline {
        let iteration_started = std::time::Instant::now();
        iterations += 1;
        Compiler::new()
            .unwrap()
            .run_program(
                "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
                 eff Boom { @aborts fn stop(message: String) -> Unit }\n\
                 fn inner() -> Int32 effects { time } {\n\
                     time.sleep(50ms)\n\
                     1\n\
                 }\n\
                 fn slow() -> Unit effects { time } {\n\
                     time.sleep(1ms)\n\
                     let ignored = inner()\n\
                 }\n\
                 fn failer() -> Unit effects { time, Boom } {\n\
                     time.sleep(2ms)\n\
                     Boom.stop(message: \"boom\")\n\
                 }\n\
                 fn main() -> Unit effects { time, Boom } {\n\
                     do {\n\
                         race {\n\
                             | slow()\n\
                             | failer()\n\
                         }\n\
                         println(\"completed\")\n\
                     } with {\n\
                         Boom.stop(message) => println(message)\n\
                     }\n\
                 }",
            )
            .expect("cancellation must always reach the nested suspend");
        iteration_times.push(iteration_started.elapsed());
    }
    iteration_times.sort_unstable();
    eprintln!(
        "stress iterations completed: {iterations}; elapsed={:?}; minimum={minimum}",
        started.elapsed()
    );
    eprintln!(
        "stress iteration latency: p50={:?}; p95={:?}; max={:?}",
        iteration_times[iterations / 2],
        iteration_times[(iterations - 1) * 95 / 100],
        iteration_times.last().unwrap()
    );
    // Includes compilation as well as runtime work; report the distribution
    // so sanitizer overhead can be distinguished from sporadic long stalls.
    assert!(
        iterations >= minimum,
        "only {iterations} iterations finished in the budget (minimum {minimum}); \
         inspect compilation/runtime latency and possible cancellation stalls"
    );
}

#[test]
fn echo_server_example_compiles_with_infinite_accept_loop() {
    let source = include_str!("../../../examples/networking/echo_server.jk").replace(
        "import joky/socket/tcp",
        include_str!("../../../std/joky/socket/tcp.jk"),
    );
    Compiler::new()
        .unwrap()
        .compile_object_program(&source)
        .expect("infinite accept loop with branch must compile");
}
