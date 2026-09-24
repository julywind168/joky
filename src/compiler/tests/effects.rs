use super::*;

#[test]
fn dispatches_a_normal_effect_from_a_called_function() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Int32 }

                    fn ask() -> Int32 effects { Console } {
                        Console.read(prompt: "age")
                    }

                    fn load_age() -> Int32 effects { Console } {
                        ask()
                    }

                    fn main() {
                        let value = do {
                            load_age()
                        } with {
                            Console.read(prompt) => 42
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal effects should reach a caller handler through a function call");
}

#[test]
fn dispatches_a_normal_effect_through_a_generated_handler_thunk() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(value: Int64) -> Int64 }

                    fn ask() -> Int64 effects { Console } {
                        Console.read(value: 40)
                    }

                    fn main() {
                        let value = do {
                            ask()
                        } with {
                            Console.read(value) => value + 2
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal effects should dispatch through a generated handler thunk");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_multiple_arguments_and_capture() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn combine(left: Int64, right: Int64) -> Int64 }

                    fn ask() -> Int64 effects { Console } {
                        Console.combine(left: 20, right: 20)
                    }

                    fn main() {
                        let offset: Int64 = 1
                        let value = do {
                            ask()
                        } with {
                            Console.combine(left, right) => left + right + offset
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal handler thunks should support multi-argument captures");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_an_aggregate_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    struct Pair { let left: Int64, let right: Int64 }
                    eff Console { fn read(value: Int64) -> Pair }

                    fn fallback() -> Pair {
                        Pair(left: 20, right: 21)
                    }

                    fn ask() -> Pair effects { Console } {
                        Console.read(value: 0)
                    }

                    fn main() {
                        let value = do {
                            ask()
                        } with {
                            Console.read(value) => fallback()
                        }
                        println(value.left)
                        println(value.right)
                    }
                "#,
        )
        .expect("normal handler thunks should flatten aggregate results");
}

#[test]
fn dispatches_a_normal_effect_thunk_without_operation_arguments() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Clock { fn now() -> Int64 }

                    fn fallback() -> Int64 { 42 }

                    fn ask() -> Int64 effects { Clock } {
                        Clock.now()
                    }

                    fn main() {
                        let value = do { ask() } with {
                            Clock.now() => fallback()
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal handler thunks should support parameterless operations");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_a_managed_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter { var value: Int64 = 42 }
                    eff Console { fn read(value: Int64) -> Counter }

                    fn fallback() -> Counter { Counter() }

                    fn ask() -> Counter effects { Console } {
                        Console.read(value: 0)
                    }

                    fn main() {
                        let value = do { ask() } with {
                            Console.read(value) => fallback()
                        }
                        println(value.value)
                    }
                "#,
        )
        .expect("normal handler thunks should transfer managed results");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_a_string_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(value: Int64) -> String }

                    fn fallback() -> String { "fallback" }

                    fn ask() -> String effects { Console } {
                        Console.read(value: 0)
                    }

                    fn main() {
                        let value = do { ask() } with {
                            Console.read(value) => fallback()
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal handler thunks should transfer String results");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_a_shared_aggregate_argument() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    struct Request { let prompt: String, let retries: Int32 }
                    eff Console { fn read(request: Request) -> String }

                    fn fallback(request: Request) -> String {
                        request.prompt
                    }

                    fn ask() -> String effects { Console } {
                        Console.read(request: Request(prompt: "age", retries: 1))
                    }

                    fn main() {
                        let value = do { ask() } with {
                            Console.read(request) => fallback(request)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal handler thunks should decode shared aggregate arguments");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_a_shared_capture_without_a_task() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> String }

                    fn ask() -> String effects { Console } {
                        Console.read(prompt: "age")
                    }

                    fn main() {
                        let prefix = "handled"
                        let value = do { ask() } with {
                            Console.read(prompt) => prefix
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal handler thunks should retain shared captures without a task");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_an_owned_class_argument() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter { let value: Int64 }
                    eff Console { fn read(counter: Counter) -> Int64 }

                    fn ask(counter: Counter) -> Int64 effects { Console } {
                        Console.read(counter: counter)
                    }

                    fn main() {
                        let value = do {
                            ask(Counter(value: 41))
                        } with {
                            Console.read(counter) => counter.value + 1
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal handler thunks should transfer owned operation arguments");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_an_owned_mutable_argument() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    @intrinsic pub class MutList(T: type) {
                        fn length(&self) -> UInt64
                        fn push(&self, item: T) -> Unit
                    }
                    eff Build { fn make(values: MutList(Int32)) -> MutList(Int32) }

                    fn ask(values: MutList(Int32)) -> MutList(Int32) effects { Build } {
                        Build.make(values: values)
                    }

                    fn main() {
                        let values = MutList(Int32)()
                        values.push(7)
                        let result = do { ask(values) } with {
                            Build.make(values) => values
                        }
                        println(result.length())
                    }
                "#,
        )
        .expect("normal handler thunks should transfer owned mutable arguments");
}

#[test]
fn dispatches_a_normal_effect_thunk_with_a_nested_owned_argument() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter { let value: Int64 }
                    struct Request { let counter: Counter }
                    eff Console { fn read(request: Request) -> Int64 }

                    fn ask(request: Request) -> Int64 effects { Console } {
                        Console.read(request: request)
                    }

                    fn main() {
                        let result = do {
                            ask(Request(counter: Counter(value: 41)))
                        } with {
                            Console.read(request) => request.counter.value + 1
                        }
                        println(result)
                    }
                "#,
        )
        .expect("normal handler thunks should transfer nested owned arguments");
}

#[test]
fn normal_effect_handler_can_return_a_class_and_continue_body() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Counter }

                    class Counter { var i: Int64 = 42 }

                    fn read_age() -> Int64 {
                        do {
                            let c = Console.read(prompt: "age")
                            c.i
                        } with {
                            Console.read(prompt) => Counter()
                        }
                    }

                    fn main() { println(read_age()) }
                "#,
        )
        .expect("normal effect handlers should preserve class results at the call site");
}

#[test]
fn normal_class_effect_result_continues_across_a_named_function() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Counter }

                    class Counter { var i: Int64 = 42 }

                    fn load_counter() -> Counter effects { Console } {
                        Console.read(prompt: "age")
                    }

                    fn read_age() -> Int64 {
                        do {
                            let c = load_counter()
                            c.i
                        } with {
                            Console.read(prompt) => Counter()
                        }
                    }

                    fn main() { println(read_age()) }
                "#,
        )
        .expect("normal class effect results should resume through named calls");
}

#[test]
fn normal_class_effect_result_continues_across_a_method_call() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Counter }

                    class Counter { var i: Int64 = 42 }
                    class Reader {
                        fn load() -> Counter effects { Console } {
                            Console.read(prompt: "age")
                        }
                    }

                    fn read_age() -> Int64 {
                        do {
                            let reader = Reader()
                            let c = reader.load()
                            c.i
                        } with {
                            Console.read(prompt) => Counter()
                        }
                    }

                    fn main() { println(read_age()) }
                "#,
        )
        .expect("normal class effect results should resume through method calls");
}

#[test]
fn normal_effect_handler_continues_with_string_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> String }

                    fn main() {
                        let value = do {
                            let text = Console.read(prompt: "name")
                            text.concat("!")
                        } with {
                            Console.read(prompt) => "Joky"
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal effect handlers should continue with String results");
}

#[test]
fn normal_effect_handler_can_return_unit_through_runtime_thunk() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn write(message: String) -> Unit }

                    fn main() {
                        do { Console.write(message: "hello") } with {
                            Console.write(message) => println(message)
                        }
                    }
                "#,
        )
        .expect("Unit normal handlers should use the runtime thunk");
}

#[test]
fn normal_effect_handler_accepts_wildcard_parameters_in_runtime_thunk() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn write(message: String) -> Unit }

                    fn main() {
                        do { Console.write(message: "hello") } with {
                            Console.write(_) => println("handled")
                        }
                    }
                "#,
        )
        .expect("wildcard normal handler parameters should use the runtime thunk");
}

#[test]
fn normal_effect_handler_continues_with_option_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Option(Int64) }

                    fn main() {
                        let value = do {
                            let result = Console.read(prompt: "age")
                            result.unwrap_or(7)
                        } with {
                            Console.read(prompt) => Some(35)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal effect handlers should continue with Option results");
}

#[test]
fn normal_effect_handler_continues_with_result_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Result(Int64, String) }

                    fn main() {
                        let value = do {
                            let result = Console.read(prompt: "age")
                            result.unwrap_or(7)
                        } with {
                            Console.read(prompt) => Ok(35)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal effect handlers should continue with Result results");
}

#[test]
fn normal_effect_handler_continues_through_a_capture_free_closure() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Int64 }

                    fn main() {
                        let ask = fn () -> Int64 {
                            Console.read(prompt: "age")
                        }
                        let value = do {
                            ask() + 1
                        } with {
                            Console.read(prompt) => 41
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal effects should continue through a capture-free closure");
}

#[test]
fn normal_effect_handler_keeps_a_complex_direct_body_out_of_private_tasks() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Int64 }

                    fn main() {
                        let value = do {
                            let answer = Console.read(prompt: "age")
                            answer + 1
                        } with {
                            Console.read(prompt) => 41
                        }
                        println(value)
                    }
                "#,
        )
        .expect("complex normal handler bodies should use the runtime frame");
}

#[test]
fn normal_effect_handler_frame_is_inherited_by_nested_parallel_body() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Int64 }

                    fn ask() -> Int64 effects { Console } {
                        Console.read(prompt: "age")
                    }

                    fn main() {
                        let values = do {
                            parallel {
                                | ask()
                                | ask()
                            }
                        } with {
                            Console.read(prompt) => 41
                        }
                        println(values.0)
                        println(values.1)
                    }
                "#,
        )
        .expect("normal handler frames should cover nested parallel bodies");
}

#[test]
fn normal_effect_handler_frame_reaches_recursive_function_calls() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Clock { fn now() -> Int32 }

                    fn recurse(depth: Int32) -> Int32 effects { Clock } {
                        if depth == 0 {
                            Clock.now()
                        } else {
                            recurse(depth - 1)
                        }
                    }

                    fn main() {
                        let value = do {
                            recurse(2)
                        } with {
                            Clock.now() => 42
                        }
                        println(value)
                    }
                "#,
        )
        .expect("recursive normal effects should use the active runtime frame");
}

#[test]
fn abortive_effect_in_normal_handler_body_reaches_outer_abort_handler() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read() -> Int32 }
                    eff Failure { @aborts fn stop() -> Unit }

                    fn main() -> Int32 {
                        do {
                            do {
                                Failure.stop()
                                1
                            } with {
                                Console.read() => 41
                            }
                        } with {
                            Failure.stop() => 7
                        }
                    }
                "#,
        )
        .expect("an abort from a normal handler body should reach the outer handler");
}

#[test]
fn abortive_method_called_in_a_normal_handler_body_uses_task_transport() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read() -> Int32 }
                    eff Failure { @aborts fn stop() -> Unit }

                    class Worker {
                        fn fail() -> Int32 effects { Failure } {
                            Failure.stop()
                            0
                        }
                    }

                    fn main() -> Int32 {
                        do {
                            do {
                                Worker().fail()
                            } with {
                                Console.read() => 41
                            }
                        } with {
                            Failure.stop() => 7
                        }
                    }
                "#,
        )
        .expect("an abortive method should propagate through a normal handler task");
}

#[test]
fn abort_handler_can_run_a_block_before_aborting() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Failure { @aborts fn stop(message: String) -> Unit }

                    fn recover() -> Int32 {
                        do { Failure.stop(message: "failure") } with {
                            Failure.stop(message) => {
                                println(message)
                                abort 7
                            }
                        }
                    }

                    fn main() {
                        println(recover())
                    }
                "#,
        )
        .expect("abort handlers should run a block before producing the do result");
}

#[test]
fn normal_effect_handler_continues_through_a_copy_capture_closure() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Console { fn read(prompt: String) -> Int64 }

                    fn main() {
                        let offset: Int64 = 1
                        let ask = fn () -> Int64 {
                            Console.read(prompt: "age") + offset
                        }
                        let value = do {
                            ask()
                        } with {
                            Console.read(prompt) => 41
                        }
                        println(value)
                    }
                "#,
        )
        .expect("normal effects should continue through a copy-capturing closure");
}

#[test]
fn dynamic_handler_frame_survives_a_stackless_sleep() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff time { @suspends fn sleep(duration: Duration) -> Unit }
                    eff Console { fn read() -> Int32 }

                    fn ask() -> Int32 effects { time, Console } {
                        time.sleep(1ms)
                        Console.read()
                    }

                    fn main() -> Unit effects { time } {
                        let value = do { ask() } with { Console.read() => 42 }
                        println(value)
                    }
                "#,
        )
        .expect("a dynamic handler frame should survive time.sleep");
}

#[test]
fn parallel_branches_get_isolated_handler_frames() {
    // Each branch enters its own handler frame over the inherited chain. The
    // two branches handle the same operation with different arms, so a shared
    // or crossed frame would serve one branch's request from the other arm.
    Compiler::new()
        .unwrap()
        .run_program(
            "eff Ask { fn question() -> Int32 }\n\
             fn main() effects { Ask } {\n\
                 do {\n\
                     let results = parallel {\n\
                         | do { Ask.question() } with { Ask.question() => 7 }\n\
                         | do { Ask.question() } with { Ask.question() => 30 }\n\
                     }\n\
                     println(results.0 + results.1)\n\
                     println(Ask.question())\n\
                 } with {\n\
                     Ask.question() => 41\n\
                 }\n\
             }",
        )
        .expect("each branch should serve its request from its own handler frame");
}

#[test]
fn internal_operations_never_reach_a_user_handler_frame() {
    // Task cancellation (the race loser) and Cown release at `when`-block end
    // are internal operations. They must never surface as effect requests on
    // the enclosing user frame: the arm aborts, so any spurious request would
    // propagate an abort and fail the program.
    Compiler::new()
        .unwrap()
        .run_program(
            "eff time { @suspends fn sleep(duration: Duration) -> Unit }\n\
             eff Trap { @aborts fn sprung() -> Unit }\n\
             class Counter {\n\
                 var value: Int32 = 0\n\
                 fn bump() { self.value = self.value + 1 }\n\
             }\n\
             fn slow() -> Int32 effects { time } {\n\
                 time.sleep(100ms)\n\
                 1\n\
             }\n\
             fn quick() -> Int32 effects { time } {\n\
                 time.sleep(1ms)\n\
                 2\n\
             }\n\
             fn main() effects { time, Trap } {\n\
                 do {\n\
                     let counter = Cown.new(Counter(value: 0))\n\
                     let bumped = when (counter) |state| {\n\
                         state.bump()\n\
                         state.value\n\
                     }\n\
                     println(bumped)\n\
                     let winner = race {\n| slow()\n| quick()\n}\n\
                     println(winner)\n\
                 } with {\n\
                     Trap.sprung() => println(\"internal operation reached a user handler\")\n\
                 }\n\
             }",
        )
        .expect("internal cancel and cown release must not invoke the user handler frame");
}

#[test]
fn a_resumable_arm_can_express_at_most_one_resume() {
    // One-shot continuation constraint, language level: `resume(...)` is only
    // meaningful as the handler arm's terminal expression (the resume
    // payload). A second resume inside the arm body is not a function and is
    // rejected; structurally the MIR verifier then enforces exactly one
    // resume marker per continuation, and the runtime CAS rejects a second
    // dispatch (see the handler frame one-shot tests).
    let error = try_run_program(
        "eff Ask { @resumable fn question(prompt: String) -> String }\n\
         fn main() effects { Ask } {\n\
             let message = do { Ask.question(\"name?\") } with {\n\
                 Ask.question(prompt) => { resume(\"a\"); resume(\"b\") }\n\
             }\n\
             println(message)\n\
         }",
    )
    .expect_err("a handler arm cannot call resume twice");
    assert!(
        error.to_string().contains("unknown function 'resume'"),
        "unexpected diagnostic: {error}"
    );
}
