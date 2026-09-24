use super::super::*;

#[test]
fn resumes_through_a_named_function_call_chain() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question(prompt: String) -> String }

                    fn read_name() -> String effects { Ask } {
                        Ask.question("name?")
                    }

                    fn welcome() -> String effects { Ask } {
                        let name = read_name()
                        "hello, ".concat(name)
                    }

                    fn main() {
                        let message = do { welcome() } with {
                            Ask.question(prompt) => resume("Joky")
                        }
                        println(message)
                    }
                "#,
        )
        .expect("resume should continue through named function calls");
}

#[test]
fn forwards_a_parameterized_resumable_handler_through_dynamic_frame() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question(prompt: Int32) -> Int32 }

                    fn read() -> Int32 effects { Ask } {
                        Ask.question(41)
                    }

                    fn main() {
                        let value = do { read() } with {
                            Ask.question(prompt) => resume(prompt)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("parameterized resumable handler should forward its argument");
}

#[test]
fn forwards_a_later_parameter_from_a_resumable_handler() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Pair { @resumable fn choose(left: Int32, right: Int32) -> Int32 }

                    fn main() {
                        let value = do { Pair.choose(left: 7, right: 42) } with {
                            Pair.choose(left, right) => resume(right)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("resumable handler should forward a later scalar parameter");
}

#[test]
fn nested_resumable_handlers_choose_the_nearest_frame() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Int32 }

                    fn main() {
                        let value = do {
                            do { Ask.question() } with {
                                Ask.question() => resume(2)
                            }
                        } with {
                            Ask.question() => resume(1)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("nested resumable handlers should use the nearest frame");
}

#[test]
fn cancelling_a_suspended_resumable_handler_cleans_its_capture() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter { var value: Int32 = 41 }
                    eff time { @suspends fn sleep(duration: Duration) -> Unit }
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn slow() -> Int32 effects { time, Calc } {
                        let counter = Counter()
                        do {
                            time.sleep(20ms)
                            Calc.adjust(value: 1)
                        } with {
                            Calc.adjust(value) => resume(counter.value + value)
                        }
                    }

                    fn fast() -> Int32 { 7 }

                    fn main() effects { time, Calc } {
                        let value = race {
                            | fast()
                            | slow()
                        }
                        println(value)
                    }
                "#,
        )
        .expect("cancelling a suspended handler task should complete cleanly");
}

#[test]
fn evaluates_pure_constant_resumable_handler_expressions() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn value() -> Int32 }

                    fn main() {
                        let value = do { Calc.value() } with {
                            Calc.value() => resume(40 + 2)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("constant resumable handler expression should execute");
}

#[test]
fn evaluates_conditional_resumable_handler_expressions() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn value() -> Int32 }

                    fn main() {
                        let value = do { Calc.value() } with {
                            Calc.value() => resume(if !false { 40 } else { 1 })
                        }
                        println(value)
                    }
                "#,
        )
        .expect("conditional resumable handler expression should execute");
}

#[test]
fn evaluates_dynamic_resumable_handler_scalar_transform() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn main() {
                        let value = do { Calc.adjust(value: 41) } with {
                            Calc.adjust(value) => resume(if value == 41 { value + 1 } else { 0 })
                        }
                        println(value)
                    }
                "#,
        )
        .expect("dynamic scalar handler expression should execute");
}

#[test]
fn deep_nested_suspending_call_chain_with_values() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn get() -> Int32 }

                    fn level_three() -> Int32 effects { Ask } {
                        Ask.get()
                    }

                    fn level_two() -> Int32 effects { Ask } {
                        level_three() + 10
                    }

                    fn level_one() -> Int32 effects { Ask } {
                        level_two() * 2
                    }

                    fn main() {
                        let result = do { level_one() } with {
                            Ask.get() => resume(5)
                        }
                        println(result)
                    }
                "#,
        )
        .expect("deep nested suspending call chain should execute and compute (5 + 10) * 2 = 30");
}

#[test]
fn multiple_suspending_calls_through_nested_functions() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Counter { @resumable fn next(current: Int32) -> Int32 }

                    fn get_two(state: Int32) -> Int32 effects { Counter } {
                        let a = Counter.next(current: state)
                        let b = Counter.next(current: a)
                        a + b
                    }

                    fn get_four() -> Int32 effects { Counter } {
                        let first = get_two(state: 0)
                        let second = get_two(state: 2)
                        first + second
                    }

                    fn main() {
                        let result = do { get_four() } with {
                            Counter.next(current) => resume(current + 1)
                        }
                        println(result)
                    }
                "#,
        )
        .expect(
            "multiple suspending calls through nested functions should execute (1+2)+(3+4) = 10",
        );
}

#[test]
fn nested_suspending_with_control_flow() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn check() -> Bool }

                    fn make_decision() -> Int32 effects { Ask } {
                        if Ask.check() {
                            100
                        } else {
                            200
                        }
                    }

                    fn process() -> Int32 effects { Ask } {
                        let value = make_decision()
                        value + 42
                    }

                    fn main() {
                        let result = do { process() } with {
                            Ask.check() => resume(true)
                        }
                        println(result)
                    }
                "#,
        )
        .expect("nested suspending with control flow should execute and return 142");
}

#[test]
fn suspending_call_chain_preserves_local_state() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Input { @resumable fn read(counter: Int32) -> Int32 }

                    fn compute(offset: Int32, counter: Int32) -> Int32 effects { Input } {
                        let base = Input.read(counter: counter)
                        base + offset
                    }

                    fn transform(counter: Int32) -> Int32 effects { Input } {
                        let x = compute(offset: 10, counter: counter)
                        let y = compute(offset: 20, counter: x)
                        x + y
                    }

                    fn main() {
                        let result = do { transform(counter: 0) } with {
                            Input.read(counter) => resume(counter + 5)
                        }
                        println(result)
                    }
                "#,
        )
        .expect("suspending call chain should preserve local state (5+10)+(10+20) = 45");
}
