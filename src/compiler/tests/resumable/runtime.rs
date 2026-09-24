use super::super::*;

#[test]
fn resumes_a_direct_resumable_effect_at_its_call_site() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question(prompt: String) -> String }

                    fn main() {
                        let greeting = do {
                            let name = Ask.question("Joky")
                            "hello, ".concat(name)
                        } with {
                            Ask.question(prompt) => resume(prompt)
                        }
                        println(greeting)
                    }
                "#,
        )
        .expect("resume should continue after the resumable operation");
}

#[test]
fn executes_runtime_resumable_scalar_handler() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Int32 }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => resume(42)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("scalar resumable handler should execute through runtime");
}

#[test]
fn executes_runtime_resumable_scalar_handler_without_explicit_resume() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Int32 }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => 42
                        }
                        println(value)
                    }
                "#,
        )
        .expect("resumable handlers should implicitly resume with their value");
}

#[test]
fn aborts_a_resumable_handler_after_running_a_block() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Int32 }
                    fn main() {
                        let value = do { Ask.question() + 1 } with {
                            Ask.question() => {
                                println("aborted")
                                abort 7
                            }
                        }
                        println(value)
                    }
                "#,
        )
        .expect("resumable handlers should abort after executing their block");
}

#[test]
fn aborts_a_resumable_handler_with_a_different_do_result_type() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Int32 }

                    fn recover() -> String {
                        do { Ask.question() } with {
                            Ask.question() => abort "fallback"
                        }
                    }

                    fn main() {
                        println(recover())
                    }
                "#,
        )
        .expect("resumable abort should determine the surrounding do result type");
}

#[test]
fn handles_a_suspending_resumable_effect_with_an_implicit_resume() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Input { @suspends @resumable fn read() -> Int32 }

                    fn main() {
                        let value = do { Input.read() } with {
                            Input.read() => 42
                        }
                        println(value)
                    }
                "#,
        )
        .expect("a suspending resumable effect should use the resumable handler path");
}

#[test]
fn executes_multiple_dynamic_resumable_handlers() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff First { @resumable fn value() -> Int32 }
                    eff Second { @resumable fn value() -> Int32 }
                    fn main() {
                        let first = do { First.value() } with {
                            First.value() => resume(10)
                            Second.value() => resume(20)
                        }
                        let second = do { Second.value() } with {
                            First.value() => resume(10)
                            Second.value() => resume(20)
                        }
                        println(first + second)
                    }
                "#,
        )
        .expect("a dynamic handler frame should register multiple operations");
}

#[test]
fn executes_runtime_resumable_scalar_handler_through_a_function() {
    let source = r#"
                    eff Ask { @resumable fn question() -> Int32 }
                    fn ask() -> Int32 effects { Ask } { Ask.question() }
                    fn main() {
                        let value = do { ask() } with {
                            Ask.question() => resume(42)
                        }
                        println(value)
                    }
                "#;
    Compiler::new()
        .unwrap()
        .run_program(source)
        .expect("runtime resumable handler should cross a function call");
}

#[test]
fn executes_runtime_resumable_string_handler_and_releases_payload() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> String }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => resume("Joky")
                        }
                        println(value)
                    }
                "#,
        )
        .expect("runtime String resumable handler should execute through the managed ABI");
}

#[test]
fn executes_runtime_resumable_string_handler_through_a_function() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> String }
                    fn ask() -> String effects { Ask } { Ask.question() }
                    fn main() {
                        let value = do { ask() } with {
                            Ask.question() => resume("Joky")
                        }
                        println(value)
                    }
                "#,
        )
        .expect("runtime String resumable handler should cross a function call");
}

#[test]
fn forwards_a_managed_resumable_parameter_through_a_function() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question(prompt: String) -> String }

                    fn ask() -> String effects { Ask } {
                        Ask.question("Joky")
                    }

                    fn main() {
                        let value = do { ask() } with {
                            Ask.question(prompt) => resume(prompt)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("managed resumable parameters should cross a dynamic handler frame");
}

#[test]
fn forwards_a_managed_resumable_parameter_through_a_recursive_call() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question(prompt: String) -> String }

                    fn ask(depth: Int32) -> String effects { Ask } {
                        if depth == 0 {
                            Ask.question("Joky")
                        } else {
                            ask(depth - 1)
                        }
                    }

                    fn main() {
                        let value = do { ask(1) } with {
                            Ask.question(prompt) => resume(prompt)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("managed resumable parameters should cross a recursive dynamic handler frame");
}

#[test]
fn restores_a_managed_resumable_capture_through_a_recursive_call() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> String }

                    fn ask(depth: Int32) -> String effects { Ask } {
                        if depth == 0 {
                            Ask.question()
                        } else {
                            ask(depth - 1)
                        }
                    }

                    fn main() {
                        let message = "captured"
                        let value = do { ask(1) } with {
                            Ask.question() => resume(message)
                        }
                        println(value)
                    }
                "#,
        )
        .expect("managed resumable captures should cross a recursive dynamic handler frame");
}

#[test]
fn executes_runtime_resumable_option_handler() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Option(Int32) }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => resume(Some(42))
                        }
                        println(value.unwrap_or(0))
                    }
                "#,
        )
        .expect("Option resumable payload should execute through the runtime");
}

#[test]
fn executes_runtime_resumable_none_handler() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Option(String) }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => resume(None)
                        }
                        println(value.is_none())
                    }
                "#,
        )
        .expect("None resumable payload should execute through the runtime");
}

#[test]
fn executes_runtime_resumable_result_handler() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Result(Int32, String) }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => resume(Err("failed"))
                        }
                        println(value.unwrap_or(7))
                    }
                "#,
        )
        .expect("Result resumable payload should execute through the runtime");
}

#[test]
fn executes_runtime_resumable_string_payloads_inside_option_and_result() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Option(String) }
                    eff Read { @resumable fn value() -> Result(String, String) }
                    fn main() {
                        let option = do { Ask.question() } with {
                            Ask.question() => resume(Some("hello"))
                        }
                        let result = do { Read.value() } with {
                            Read.value() => resume(Err("failed"))
                        }
                        println(option.unwrap_or("missing"))
                        println(result.unwrap_or("fallback"))
                    }
                "#,
        )
        .expect("managed Option and Result payloads should resume");
}

#[test]
fn executes_nested_runtime_resumable_option_and_result_payloads() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Ask { @resumable fn question() -> Result(Option(String), String) }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => resume(Ok(Some("nested")))
                        }
                        println(value.unwrap_or(None).unwrap_or("fallback"))
                    }
                "#,
        )
        .expect("nested Option and Result payloads should resume");
}

#[test]
fn executes_runtime_resumable_tuple_and_struct_payloads() {
    Compiler::new()
            .unwrap()
            .run_program(
                r#"
                    struct Point {
                        let x: Int32
                        let label: String
                        let pair: (Int32, String)
                    }
                    eff Build { @resumable fn point() -> Point }
                    fn main() {
                        let point = do { Build.point() } with {
                            Build.point() => resume(Point(x: 7, label: "struct", pair: (8, "nested")))
                        }
                        println(point.x)
                    }
                "#,
            )
            .expect("aggregate resumable payloads should execute through the runtime");
}

#[test]
fn executes_runtime_resumable_enum_payload() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    enum Item {
                        Point(value: String)
                        Empty
                    }
                    eff Ask { @resumable fn question() -> Item }
                    eff EmptyAsk { @resumable fn question() -> Item }
                    fn main() {
                        let value = do { Ask.question() } with {
                            Ask.question() => resume(Item.Point(value: "enum"))
                        }
                        match value {
                            Item.Point(value: point) => println(point)
                            Item.Empty => println(0)
                        }
                        let empty = do { EmptyAsk.question() } with {
                            EmptyAsk.question() => resume(Item.Empty)
                        }
                        match empty {
                            Item.Point(value: point) => println(point)
                            Item.Empty => println("empty")
                        }
                    }
                "#,
        )
        .expect("enum resumable payload should execute through the runtime");
}

#[test]
fn executes_runtime_resumable_collection_payloads() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Items { @resumable fn list() -> List(String) }
                    eff Values { @resumable fn map() -> Map(String, Int32) }
                    eff Tags { @resumable fn set() -> Set(String) }
                    fn main() {
                        let items = do { Items.list() } with {
                            Items.list() => resume(List#{"one", "two"})
                        }
                        println(items.length())
                        let values = do { Values.map() } with {
                            Values.map() => resume(Map#{"answer" => 42})
                        }
                        println(values.get("answer").unwrap_or(0))
                        let tags = do { Tags.set() } with {
                            Tags.set() => resume(Set#{"joky", "lang"})
                        }
                        println(tags.length())
                    }
                "#,
        )
        .expect("List, Map and Set resumable payloads should execute through the runtime");
}

#[test]
fn executes_runtime_resumable_class_payload() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Counter {
                        var value: Int32 = 0
                    }
                    eff Build { @resumable fn counter() -> Counter }
                    fn main() {
                        let counter = do { Build.counter() } with {
                            Build.counter() => resume(Counter(value: 41))
                        }
                        println(counter.value + 1)
                    }
                "#,
        )
        .expect("class resumable payload should execute through the runtime");
}

#[test]
fn executes_runtime_resumable_class_with_managed_field() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    class Message {
                        let text: String = ""
                    }
                    eff Build { @resumable fn message() -> Message }
                    fn main() {
                        let message = do { Build.message() } with {
                            Build.message() => resume(Message(text: "hello"))
                        }
                        println(message.text)
                    }
                "#,
        )
        .expect("class payload with managed fields should execute");
}

#[test]
fn executes_runtime_resumable_mutable_collection_payloads() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    @intrinsic
                    class MutList(T: type) {
                        fn get(&self, index: UInt64) -> Option(T)
                        fn length(&self) -> UInt64
                    }
                    @intrinsic
                    class MutMap(K: type + Hash + Eq, V: type) {
                        fn get(&self, key: K) -> Option(V)
                    }
                    eff Build { @resumable fn values() -> MutList(Int32) }
                    fn main() {
                        let values = do { Build.values() } with {
                            Build.values() => resume(MutList#{10, 32})
                        }
                        println(values.get(1).unwrap_or(0))
                    }
                "#,
        )
        .expect("mutable resumable payload should execute");
}

#[test]
fn executes_runtime_resumable_boolean_and_float_handlers() {
    Compiler::new()
        .unwrap()
        .run_program(
            r#"
                    eff Probe { @resumable fn flag() -> Bool }
                    eff Measure { @resumable fn value() -> Float64 }
                    fn main() {
                        let flag = do { Probe.flag() } with {
                            Probe.flag() => resume(true)
                        }
                        let value = do { Measure.value() } with {
                            Measure.value() => resume(1.5)
                        }
                        println(flag)
                        println(value)
                    }
                "#,
        )
        .expect("runtime scalar payloads should preserve their types");
}
