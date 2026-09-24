use super::*;

#[test]
fn invokes_synthetic_handler_with_a_managed_string_capture() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Logic { @resumable fn check(value: Bool) -> Bool }

                    fn main() {
                        let message: String = "hello"
                        let value = do { Logic.check(value: true) } with {
                            Logic.check(value) => resume((message == "hello") && value)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("synthetic handler should retain and release a String capture");
}

#[test]
fn invokes_synthetic_handler_with_an_owned_mut_list_capture() {
    Compiler::new()
            .unwrap()
            .run_program(
                r#"
                    @intrinsic
                    class MutList(T: type) {
                        fn length(&self) -> UInt64
                    }
                    eff Logic { @resumable fn check(value: Bool) -> Bool }

                    fn main() {
                        let values = MutList#{10, 32}
                        let value = do { Logic.check(value: true) } with {
                            Logic.check(value) => resume({ let size: UInt64 = values.length(); value })
                        }
                        println(value)
                    }
                "#,
            )
            .expect("synthetic handler should move and release a MutList capture");
}

#[test]
fn invokes_synthetic_handler_with_an_owned_mut_map_capture() {
    Compiler::new()
            .unwrap()
            .run_program(
                r#"
                    @intrinsic
                    class MutMap(K: type + Hash + Eq, V: type) {
                        fn length(&self) -> UInt64
                    }
                    eff Logic { @resumable fn check(value: Bool) -> Bool }

                    fn main() {
                        let values = MutMap(Int32, Int32)()
                        let value = do { Logic.check(value: true) } with {
                            Logic.check(value) => resume({ let size: UInt64 = values.length(); value })
                        }
                        println(value)
                    }
                "#,
            )
            .expect("synthetic handler should move and release a MutMap capture");
}

#[test]
fn invokes_synthetic_handler_with_an_owned_mut_set_capture() {
    Compiler::new()
            .unwrap()
            .run_program(
                r#"
                    @intrinsic
                    class MutSet(T: type + Hash + Eq) {
                        fn length(&self) -> UInt64
                    }
                    eff Logic { @resumable fn check(value: Bool) -> Bool }

                    fn main() {
                        let values = MutSet#{"ready"}
                        let value = do { Logic.check(value: true) } with {
                            Logic.check(value) => resume({ let size: UInt64 = values.length(); value })
                        }
                        println(value)
                    }
                "#,
            )
            .expect("synthetic handler should move and release a MutSet capture");
}

#[test]
fn invokes_synthetic_handler_with_a_nested_owned_capture() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    @intrinsic
                    class MutList(T: type) {
                        fn length(&self) -> UInt64
                    }
                    struct Holder {
                        let marker: Int32
                        let values: MutList(Int32)
                    }
                    eff Logic { @resumable fn check(value: Bool) -> Bool }

                    fn main() {
                        let holder = Holder(marker: 7, values: MutList(Int32)())
                        let value = do { Logic.check(value: true) } with {
                            Logic.check(value) => resume({
                                let size: UInt64 = holder.values.length()
                                value
                            })
                        }
                        println(value)
                    }
                "#,
        )
        .expect("synthetic handler should move and release nested owned captures");
}

#[test]
fn invokes_synthetic_handler_with_an_owned_class_inside_a_struct() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter {
                        var value: Int32 = 0
                    }
                    struct Holder {
                        let counter: Counter
                    }
                    eff Logic { @resumable fn check(value: Bool) -> Bool }

                    fn main() {
                        let holder = Holder(counter: Counter(value: 41))
                        let value = do { Logic.check(value: true) } with {
                            Logic.check(value) => resume({
                                let current: Int32 = holder.counter.value
                                value
                            })
                        }
                        println(value)
                    }
                "#,
        )
        .expect("synthetic handler should own class fields inside aggregates");
}

#[test]
fn invokes_typed_synthetic_handler_with_int32_captures() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn main() {
                        let left: Int32 = 2
                        let right: Int32 = 3
                        let value = do { Calc.adjust(value: 20) } with {
                            Calc.adjust(value) => resume(value + left + right)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("Int32 synthetic handler should retain typed captures");
}

#[test]
fn invokes_typed_synthetic_handler_with_float64_captures() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Float64) -> Float64 }

                    fn main() {
                        let left: Float64 = 0.5
                        let right: Float64 = 1.5
                        let value = do { Calc.adjust(value: 20.0) } with {
                            Calc.adjust(value) => resume(value + left + right)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("Float64 synthetic handler should retain typed captures");
}

#[test]
fn invokes_typed_synthetic_handler_with_bool_captures() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Logic { @resumable fn choose(value: Bool) -> Bool }

                    fn main() {
                        let left: Bool = true
                        let right: Bool = false
                        let value = do { Logic.choose(value: true) } with {
                            Logic.choose(value) => resume(left || (value && right))
                        }
                        println(value)
                    }
                "#,
        )
        .expect("Bool synthetic handler should retain typed captures");
}

#[test]
fn invokes_mixed_typed_synthetic_handler_captures() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn main() {
                        let enabled: Bool = true
                        let threshold: Float64 = 1.5
                        let value = do { Calc.adjust(value: 20) } with {
                            Calc.adjust(value) => resume({
                                let copied = threshold
                                if enabled { value + 1 } else { value }
                            })
                        }
                        println(value)
                    }
                "#,
        )
        .expect("mixed typed synthetic handler captures should execute");
}

#[test]
fn invokes_resumable_handler_with_class_capture() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter {
                        var value: Int32 = 0
                    }
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn main() {
                        let counter = Counter(value: 41)
                        let value = do { Calc.adjust(value: 1) } with {
                            Calc.adjust(value) => resume(counter.value + value)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("resumable handler should support a class capture");
}

#[test]
fn invokes_resumable_handler_with_class_capture_through_function() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter {
                        var value: Int32 = 0
                    }
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn ask(counter: Counter) -> Int32 effects { Calc } {
                        Calc.adjust(value: 1)
                    }

                    fn main() {
                        let counter = Counter(value: 41)
                        let value = do { ask(counter) } with {
                            Calc.adjust(value) => resume(counter.value + value)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("resumable handler should support a class capture through a function");
}

#[test]
fn invokes_resumable_handler_with_class_capture_through_recursive_method() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter {
                        var value: Int32 = 41
                        fn ask(depth: Int32) -> Int32 effects { Calc } {
                            if depth == 0 {
                                Calc.adjust(value: 1)
                            } else {
                                self.ask(depth - 1)
                            }
                        }
                    }
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }

                    fn main() {
                        let counter = Counter()
                        let value = do { counter.ask(1) } with {
                            Calc.adjust(value) => resume(counter.value + value)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("resumable handler should support a class capture through recursion");
}

#[test]
fn invokes_resumable_handler_with_class_capture_after_suspend() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter { var value: Int32 = 41 }
                    eff time { @suspends fn sleep(duration: Duration) -> Unit }
                    eff Calc { @resumable fn adjust(value: Int32) -> Int32 }
                    fn main() -> Unit effects { time } {
                        let counter = Counter()
                        let value = do {
                            time.sleep(1ms)
                            Calc.adjust(value: 1)
                        } with {
                            Calc.adjust(value) => resume(counter.value + value)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("class capture should survive a suspend");
}

#[test]
fn resumes_through_struct_and_class_method_call_chains() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question(prompt: String) -> String }

                    struct Greeter {
                        let prefix: String = "hello, "
                        fn message(prompt: String) -> String effects { Ask } {
                            self.prefix.concat(Ask.question(prompt))
                        }
                    }

                    class Session {
                        let prefix: String = "welcome, "
                        fn login() -> String effects { Ask } {
                            self.prefix.concat(Ask.question("user?"))
                        }
                    }

                    fn main() {
                        let greeter = Greeter()
                        let session = Session()
                        let message = do {
                            greeter.message(prompt: "name?")
                        } with {
                            Ask.question(prompt) => resume("Joky")
                        }
                        let welcome = do {
                            session.login()
                        } with {
                            Ask.question(prompt) => resume("Joky")
                        }
                        println(message)
                        println(welcome)
                    }
                "#,
        )
        .expect("resume should continue through struct and class methods");
}

#[test]
fn resumes_after_time_sleep_through_a_named_function_call_chain() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff time { @suspends fn sleep(duration: Duration) -> Unit }
                    eff Ask { @resumable fn question(prompt: String) -> String }

                    fn read_name() -> String effects { Ask } {
                        Ask.question("name?")
                    }

                    fn welcome() -> String effects { Ask } {
                        "hello, ".concat(read_name())
                    }

                    fn main() effects { time } {
                        let message = do {
                            time.sleep(1ms)
                            welcome()
                        } with {
                            Ask.question(prompt) => resume("Joky")
                        }
                        println(message)
                    }
                "#,
        )
        .expect("resume should continue after a stackless time.sleep");
}
