use super::*;

#[test]
fn shared_fields_read_through_nested_class_borrows_are_retained() {
    run_program(
        r#"
        class Inner { let text: String }
        class Outer { let inner: Inner; fn read() -> String { self.inner.text } }
        fn main() {
            let value = Outer(inner: Inner(text: "retained"))
            println(value.read())
            if value.read() != "retained" { panic("dangling shared field") }
        }
    "#,
    );
}

#[test]
fn cancelled_owned_call_result_accepts_a_null_class_handle() {
    run_program(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        class Box { let text: String }
        fn wait() -> Box effects { time } { let value = Box(text: "cancel"); time.sleep(30ms); value }
        fn fast() -> Box { Box(text: "ready") }
        fn main() effects { time } {
            let winner = race {
                | wait()
                | fast()
            }
        }
    "#,
    );
}

#[test]
fn consuming_receiver_survives_a_suspending_call() {
    run_program(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        class C {
            let value: Int32
            fn finish(self) -> Int32 effects { time } { time.sleep(1ms); self.value }
        }
        fn main() effects { time } {
            let c = C(value: 42)
            if c.finish() != 42 { panic("resumed owner") }
        }
    "#,
    );
}

#[test]
fn rejected_provider_does_not_release_a_borrowed_class_argument() {
    assert!(try_run_program(
        r#"
        class C { let text: String }
        eff E { @suspends fn request(c: &C) -> Unit }
        fn main() effects { E } { let c = C(text: "owned by caller"); E.request(c) }
    "#
    )
    .is_err());
}

#[test]
fn calls_reject_overlapping_or_invalidated_class_borrows() {
    for body in [
        "pair(c, c)",
        "with_flag(c, c.close())",
        "with_flag(c, if flag { c.close() } else { () })",
    ] {
        let source = format!("class C {{ fn close(self) {{}} }} fn pair(a: &C, b: &C) {{}} fn with_flag(a: &C, flag: Unit) {{}} fn invalid(flag: Bool) {{ let c = C(); {body} }} fn main() {{}}");
        assert!(try_run_program(&source).is_err(), "accepted {body}");
    }
    assert!(try_run_program(
        r#"
        class Child {}
        class Parent {
            var child: Child
            fn replace(&self) { self.child = Child() }
        }
        fn use_child(child: &Child, flag: Unit) {}
        fn invalid() { let p = Parent(child: Child()); use_child(p.child, p.replace()) }
        fn main() {}
    "#
    )
    .is_err());
}

#[test]
fn borrowed_class_parameters_preserve_the_owner() {
    run_program(
        r#"
        class C {
            var value: Int32
            fn increment(&self) { self.value = self.value + 1 }
            fn add(&self, other: &C) { self.value = self.value + other.value }
        }
        fn increment(c: &C) { c.increment() }
        fn main() {
            let a = C(value: 19); let b = C(value: 21)
            increment(a); increment(b)
            a.add(b)
            if a.value != 42 { panic("borrow") }
        }
    "#,
    );
}

#[test]
fn borrowed_effect_parameters_cannot_escape_through_a_handler() {
    assert!(try_run_program(
        r#"
        class C {}
        eff E { fn borrow(value: &C) -> C }
        fn main() {
            let c = C()
            let escaped = do { E.borrow(c) } with { E.borrow(value) => value }
        }
    "#
    )
    .is_err());
}

#[test]
fn borrowed_effect_handler_keeps_the_callers_class_alive() {
    run_program(
        r#"
        class C { let value: Int32 }
        eff E { fn read(value: &C) -> Int32 }
        fn main() {
            let c = C(value: 42)
            let first = do { E.read(c) } with { E.read(value) => value.value }
            if first != 42 { panic("handler") }
            if c.value != 42 { panic("owner") }
        }
    "#,
    );
}

#[test]
fn traits_preserve_consuming_and_borrowed_receivers() {
    run_program(
        r#"
        trait Finish { fn finish(self) -> Int32 }
        class C { let value: Int32 }
        impl Finish for C { fn finish(self) -> Int32 { self.value } }
        fn main() { let c = C(value: 42); if c.finish() != 42 { panic("finish") } }
    "#,
    );
    assert!(try_run_program(
        r#"
        trait Finish { fn finish(self) -> Int32 }
        class C {}
        impl Finish for C { fn finish(&self) -> Int32 { 42 } }
        fn main() {}
    "#
    )
    .is_err());
}

#[test]
fn class_receiver_modes_mutate_borrow_and_transfer_ownership() {
    run_program(
        r#"
        class Counter {
            var value: Int32
            fn increment(&self) { self.value = self.value + 1 }
            fn read() -> Int32 { self.value }
            fn take(self) -> Counter { self }
            fn finish(self) -> Int32 { self.value }
        }
        fn main() {
            let c = Counter(value: 40)
            c.increment()
            c.increment()
            if c.read() != 42 { panic("borrowed mutation") }
            let moved = c.take()
            if moved.finish() != 42 { panic("owned receiver") }
            Counter(value: 0).increment()
        }
    "#,
    );
}

#[test]
fn consuming_class_receiver_rejects_reuse_on_control_flow_paths() {
    for body in [
        "c.close(); c.read()",
        "if flag { c.close() }; c.read()",
        "while flag { c.close() }; 0",
    ] {
        let source = format!("class C {{ let value: Int32; fn close(self) {{}} fn read(&self) -> Int32 {{ self.value }} }} fn run(flag: Bool) -> Int32 {{ let c = C(value: 1); {body} }} fn main() {{}}");
        assert!(try_run_program(&source).is_err(), "accepted {body}");
    }
}

#[test]
fn borrowed_class_receiver_cannot_escape_or_be_consumed() {
    for method in [
        "fn invalid(&self) -> C { self }",
        "fn invalid(&self) { self.close() }",
        "fn invalid(&self) { let copy = self }",
        "fn invalid(&self) { let f = move fn () { self.close() }; f() }",
        "fn invalid(&self) { let box = Box(value: self) }",
    ] {
        let source = format!("class Box {{ let value: C }} class C {{ fn close(self) {{}} {method} }} fn main() {{}}");
        assert!(try_run_program(&source).is_err(), "accepted {method}");
    }
}

#[test]
fn receiver_moves_do_not_affect_shadowed_or_unrelated_bindings() {
    run_program(
        r#"
        class C { fn close(self) {} fn read(&self) {} }
        fn first() { let c = C(); c.close() }
        fn second() { let c = C(); c.read() }
        fn main() {
            first(); second()
            let c = C()
            { let c = C(); c.close() }
            c.read()
        }
    "#,
    );
}

#[test]
fn drops_nested_class_fields_and_replaced_values() {
    let source = "
            class Child {
                let value: Int32
            }

            class Parent {
                var child: Child

                fn replace(child: Child) {
                    self.child = child
                }
            }

            fn main() {
                let parent = Parent(child: Child(value: 1))
                parent.replace(child: Child(value: 2))
            }
        ";

    run_program(source);
}

#[test]
fn drops_classes_owned_by_value_aggregates() {
    let source = "
            class Token {
                let value: Int32
            }

            struct Boxed {
                let token: Token
            }

            enum MaybeToken {
                Some(token: Token)
                None
            }

            fn consume_box(value: Boxed) {}
            fn consume_tuple(value: (Token, Int32)) {}
            fn consume_maybe(value: MaybeToken) {}

            fn main() {
                consume_box(value: Boxed(token: Token(value: 1)))
                consume_tuple(value: (Token(value: 2), 2))
                consume_maybe(value: MaybeToken.Some(token: Token(value: 3)))
                consume_maybe(value: MaybeToken.None)
            }
        ";
    run_program(source);
}

#[test]
fn generated_structs_preserve_nested_ownership() {
    let source = r#"
        class Token {
            let value: Int32
        }

        fn Box(T: type) -> type {
            struct { let value: T }
        }

        fn consume(value: Box(Token)) {}

        fn main() {
            consume(Box(Token)(value: Token(value: 1)))
        }
    "#;
    run_program(source);
}

#[test]
fn generated_structs_are_move_only_when_they_contain_classes() {
    let source = r#"
        class Token { let value: Int32 }

        fn Box(T: type) -> type {
            struct { let value: T }
        }

        fn main() {
            let value = Box(Token)(value: Token(value: 1))
            let moved = value
            let invalid = value
        }
    "#;

    assert!(try_run_program(source).is_err());
}

#[test]
fn generated_struct_defaults_release_owned_values() {
    let source = r#"
        class Token { let value: Int32 }

        fn Box(T: type) -> type {
            struct { let value: T = Token(value: 7) }
        }

        fn main() {
            let value: Box(Token) = Box(Token)()
            let moved = value
        }
    "#;
    run_program(source);
}

#[test]
fn generated_enum_variants_preserve_nested_ownership() {
    let source = r#"
        class Token { let value: Int32 }

        fn Maybe(T: type) -> type {
            enum {
                Some(value: T)
                None
            }
        }

        fn consume(value: Maybe(Token)) {}

        fn main() {
            consume(Maybe(Token).Some(value: Token(value: 7)))
            let empty: Maybe(Token) = Maybe(Token).None
            consume(empty)
        }
    "#;
    run_program(source);
}

#[test]
fn builtin_first_class_option_preserves_owned_payloads() {
    let source = r#"
        class Token { let value: Int32 }

        fn main() {
            let option: Option(Token) = Option(Token).Some(value: Token(value: 7))
            let moved = option
        }
    "#;
    run_program(source);
}
#[test]
fn borrows_owned_struct_receivers_and_destructures_owned_enums() {
    let source = r#"
            class Token {
                let value: Int32

                fn read() -> Int32 {
                    value
                }
            }

            struct Holder {
                let token: Token

                fn read() -> Int32 {
                    token.read()
                }
            }

            enum MaybeToken {
                Some(token: Token)
                None
            }

            fn read_maybe(value: MaybeToken) -> Int32 {
                match value {
                    MaybeToken.Some(token) => token.read()
                    MaybeToken.None => 0
                }
            }

            fn main() {
                let holder = Holder(token: Token(value: 20))
                let maybe = MaybeToken.Some(token: Token(value: 2))
                let result = holder.read() + holder.read() + read_maybe(value: maybe)
            }
        "#;
    run_program(source);
}
#[test]
fn drops_owned_aggregate_class_fields_when_replaced() {
    let source = "
            class Token {
                let value: Int32
            }

            struct Payload {
                let token: Token
            }

            class Owner {
                var payload: Payload

                fn replace(payload: Payload) {
                    self.payload = payload
                }
            }

            fn main() {
                let owner = Owner(payload: Payload(token: Token(value: 1)))
                owner.replace(payload: Payload(token: Token(value: 2)))
            }
        ";
    run_program(source);
}
#[test]
fn moves_owned_fields_out_of_consumed_value_aggregates() {
    let source = "
            class Token {
                let value: Int32

                fn read() -> Int32 {
                    value
                }
            }

            struct Pair {
                let left: Token
                let right: Token
            }

            fn take_left(pair: Pair) -> Token {
                pair.left
            }

            fn main() {
                let token = take_left(pair: Pair(left: Token(value: 1), right: Token(value: 2)))
                let value = token.read()
            }
        ";
    run_program(source);
}

#[test]
fn compiles_and_releases_managed_string_expressions() {
    let source = r#"
            fn main() {
                let greeting = "hello"
                let message = greeting + " world"
                let same = message == "hello world"
                let different = message != "other"
                let empty = "".is_empty()
                let length = message.byte_count()
                println(message)
                println(message)
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_string_search_methods_and_releases_arguments() {
    let source = r#"
            fn main() {
                let text = "hello world"
                let prefix = text.starts_with("hello")
                let suffix = text.ends_with("world")
                let found = text.contains("lo wo")
                let missing = text.contains("joky")
                println("string search ok")
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_unicode_string_queries() {
    let source = r#"
            fn main() {
                let value = "é😀"
                let scalars = value.scalar_count()
                let graphemes = value.grapheme_count()
                let length = value.length()
                let ascii = value.is_ascii()
                let joined = value.concat("!")
                let trimmed = joined.trim()
                let upper = trimmed.to_upper()
                let lower = upper.to_lower()
                println("unicode queries ok")
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_and_matches_options() {
    let source = "
            fn select(found: Bool) -> Option(Int32) {
                if found { Some(42) } else { None }
            }

            fn main() {
                let value = select(false)
                let missing = value.is_none()
                let fallback = value.unwrap_or(10)
                let result = match value {
                    Some(number) => number
                    None => 0
                }
            }
        ";
    run_program(source);
}

#[test]
fn options_preserve_managed_string_values() {
    let source = "
            fn selected() -> Option(String) { Some(\"hello\") }
            fn main() {
                let value = selected()
                let text = value.unwrap_or(\"fallback\")
                println(text)
            }
        ";
    run_program(source);
}

#[test]
fn compiles_and_matches_tuple_payloads() {
    run_program(
        r#"
            fn split(value: Result((Int32, Int32), String)) -> Int32 {
                match value {
                    Ok((left, right)) => left + right
                    Err(message) => 0
                }
            }

            fn main() {
                if split(Ok((20, 22))) != 42 { panic("result tuple") }
                if split(Err("bad")) != 0 { panic("error") }
                let pair = (1, 2)
                let sum = match pair {
                    (left, right) => left + right
                }
                if sum != 3 { panic("top-level tuple") }
                let unit = match () {
                    () => 1
                }
                if unit != 1 { panic("unit tuple") }
            }
        "#,
    );
}

#[test]
fn compiles_and_matches_map_entry_tuples() {
    run_program(
        r#"
            fn main() {
                var table: Map(Int32, Int32) = Map#{}
                table = table.insert(1, 10)
                table = table.insert(2, 20)
                var total = 0
                for entry in table.entries() {
                    match entry {
                        (key, value) => { total = total + key + value }
                    }
                }
                if total != 33 { panic("entry tuples") }
            }
        "#,
    );
}

#[test]
fn compiles_and_matches_results() {
    let source = "
            fn parse(ok: Bool) -> Result(Int32, String) {
                if ok { Ok(42) } else { Err(\"bad\") }
            }

            fn main() {
                let value = parse(false)
                let failed = value.is_err()
                let fallback = value.unwrap_or(10)
                let result = match value {
                    Ok(number) => number
                    Err(message) => 0
                }
            }
        ";
    run_program(source);
}

#[test]
fn results_release_managed_string_payloads() {
    let source = "
            fn fail() -> Result(Int32, String) { Err(\"bad\") }
            fn main() {
                let message = match fail() {
                    Ok(_) => \"ok\"
                    Err(error) => error
                }
                println(message)
            }
        ";
    run_program(source);
}

#[test]
fn compiles_unwrap_and_propagation_for_options_and_results() {
    let source = r#"
            fn force(value: Option(Int32)) -> Int32 {
                value!
            }

            fn propagate(value: Result(Int32, String)) -> Result(Int32, String) {
                let number = value?
                Ok(number + 1)
            }

            fn main() {
                let option_value = force(Some(4))
                let result_value = propagate(Ok(5))!
                let total = option_value + result_value
            }
        "#;
    run_program(source);
}

#[test]
fn unwrap_keeps_shared_payloads_alive() {
    let source = r#"
            fn force(value: Option(String)) -> String {
                value!
            }

            fn main() {
                let text = force(Some("hello"))
                println(text)
            }
        "#;
    run_program(source);
}

#[test]
fn unwrap_moves_owned_payloads() {
    let source = r#"
            class Token {
                let value: Int32
            }

            fn force(value: Option(Token)) -> Token {
                value!
            }

            fn main() {
                let token = force(Some(Token(value: 7)))
                let value = token.value
            }
        "#;
    run_program(source);
}

#[test]
fn propagation_returns_option_and_result_failures() {
    let source = r#"
            fn option(value: Option(String)) -> Option(String) {
                let text = value?
                Some(text)
            }

            fn result(value: Result(Int32, String)) -> Result(Int32, String) {
                let number = value?
                Ok(number)
            }

            fn main() {
                let option_value = option(None)
                let result_value = result(Err("bad"))
                let option_missing = option_value.is_none()
                let result_failed = result_value.is_err()
            }
        "#;
    run_program(source);
}

#[test]
fn qualified_trait_calls_preserve_receiver_ownership() {
    run_program(include_str!(
        "../../../tests/fixtures/trait_qualified_methods.jk"
    ));
    let error = try_run_program(
        r#"
        trait Finish { fn finish(self) -> Unit }
        class C { let value: Int32 }
        impl Finish for C { fn finish(self) -> Unit {} }
        fn main() { let value = C(value: 42); Finish.finish(value); Finish.finish(value) }
    "#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("after move"), "{error}");
}

#[test]
fn qualified_trait_calls_preserve_effects_and_borrowed_arguments() {
    run_program(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        trait Wait { fn read(&self, other: &C) -> Int32 effects { time } }
        trait Fast { fn read(&self, other: &C) -> Int32 }
        class C { let value: Int32 }
        impl Wait for C {
            fn read(&self, other: &C) -> Int32 effects { time } {
                let result = self.value + other.value
                time.sleep(1ms)
                result
            }
        }
        impl Fast for C { fn read(&self, other: &C) -> Int32 { self.value - other.value } }
        fn read(T: type + Wait + Fast, value: &T, other: &C) -> Int32 effects { time } {
            Wait.read(value, other)
        }
        fn main() effects { time } {
            let a = C(value: 41)
            let b = C(value: 1)
            if read(a, b) != 42 { panic("suspending generic method") }
            if Fast.read(a, b) != 40 { panic("borrowed argument") }
        }
    "#,
    );
}

#[test]
fn tuple_destructuring_moves_nested_owned_values_and_supports_mutable_captures() {
    run_program(include_str!("../../../tests/fixtures/tuple_bindings.jk"));
}

#[test]
fn tuple_destructuring_rejects_duplicate_refutable_and_wrong_arity_patterns() {
    for binding in [
        "let (a, a) = (1, 2)",
        "let (a, b, c) = (1, 2)",
        "let (Some(a), b) = (Some(1), 2)",
        "let (a, b) = 1",
    ] {
        assert!(
            try_run_program(&format!("fn main() {{ {binding}; () }}")).is_err(),
            "{binding}"
        );
    }
}
