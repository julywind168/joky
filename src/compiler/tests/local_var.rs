use super::*;

#[test]
fn local_var_control_flow_shadowing_and_function_rebinding() {
    run_program(
        r#"
        fn expect(ok: Bool) { if !ok { panic("local var") } }
        fn main() {
            var count = 0
            var total = 0
            while count < 6 {
                count = count + 1
                if count == 2 { continue }
                if count == 5 { break }
                total = total + count
            }
            expect(count == 5)
            expect(total == 8)
            for n in List(1, 2, 3) { total = total + n }
            expect(total == 14)
            total = { var total = 8; total = total + 1; total }
            expect(total == 9)
            { let total = 100; expect(total == 100) }
            expect(total == 9)
            var call = fn () -> Int32 { 1 }
            expect(call() == 1)
            call = fn () -> Int32 { 2 }
            expect(call() == 2)
            let internal = fn () -> Int32 { var value = 1; value = value + 1; value }
            expect(internal() == 2)
            expect(internal() == 2)
        }
    "#,
    );
}

#[test]
fn local_var_owned_shared_self_assignment_and_rhs_move_survive_suspend() {
    run_program(
        r#"
        class Token { let value: Int32 }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        fn transform(old: Token) -> Token effects { time } {
            time.sleep(1ms)
            Token(value: old.value + 1)
        }
        fn main() effects { time } {
            var token = Token(value: 0)
            var text = "a"
            var count = 0
            token = token
            text = text
            count = count
            while count < 3 {
                token = transform(token)
                text = text + "b"
                count = count + 1
                time.sleep(1ms)
            }
            if token.value != 3 { panic("owned state after resume") }
            if text != "abbb" { panic("shared state after resume") }
            if count != 3 { panic("copy state after resume") }
            let moved = token
            token = Token(value: moved.value + 1)
            if token.value != 4 { panic("reinitialization") }
        }
    "#,
    );
}

#[test]
fn local_var_internal_boundaries_and_when_are_usable() {
    run_program(
        r#"
        class Counter { var n: Int32; fn bump() { self.n = self.n + 1 } }
        eff Value { fn read() -> Int32 }
        fn main() {
            var outside = 1
            let state = Cown.new(Counter(n: 0))
            when (state) |counter| { counter.bump(); outside = outside + counter.n }
            let result = do { var n = Value.read(); n = n + 1; n }
                with { Value.read() => { var n = 40; n = n + 1; n } }
            let values = parallel {
                | { var n = 1; n = n + 1; n }
                | { var n = 3; n = n + 1; n }
            }
            let mapped = @parallel(limit: 2) for item in List(1, 2) { var n = item; n = n + 1; n }
            if outside != 2 { panic("when") }
            if result != 42 { panic("handler") }
            if mapped != List(2, 3) { panic("parallel for") }
        }
    "#,
    );
}

#[test]
fn local_var_rejects_immutable_bindings_and_type_values_with_source_spans() {
    for (source, message) in [
        ("fn main() { let n = 0; n = 1 }", "immutable binding 'n'"),
        (
            "fn f(n: Int32) { n = 1 } fn main() {}",
            "immutable binding 'n'",
        ),
        (
            "fn main() { var T = Int32 }",
            "cannot hold a compile-time type",
        ),
        (
            "fn main() { var n: Int32 = 1; n = true }",
            "expected Int32, found Bool",
        ),
        (
            "fn main() { var n = 1; { let n = 2; n = 3 } }",
            "immutable binding 'n'",
        ),
    ] {
        let error = try_run_program(source).expect_err(source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(error.span().is_some(), "{error}");
        assert!(error.to_string().contains(message), "{source}: {error}");
    }
}

#[test]
fn local_var_rejects_every_outer_capture_boundary() {
    for body in [
        "let f = fn () -> Int32 { n }",
        "branch { n = 2 }",
        "let values = parallel {\n| n\n| 0\n}",
        "let value = race {\n| n\n| 0\n}",
        "let values = @parallel(limit: 2) for item in List(1, 2) { item + n }",
        "let _ = do { n + Value.read() } with { Value.read() => 0 }",
        "let _ = do { Value.read() } with { Value.read() => n }",
        "let f = fn () -> Int32 { when (state) |counter| { n = counter.n }; 0 }",
    ] {
        let source = format!("class Counter {{ var n: Int32 }} eff Value {{ fn read() -> Int32 }} fn main() {{ var n = 1; let state = Cown.new(Counter(n: 0)); {body}; () }}");
        let error = try_run_program(&source).expect_err(&source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(
            error
                .to_string()
                .contains("cannot capture outer mutable binding 'n'"),
            "{body}: {error}"
        );
        assert!(error.span().is_some(), "{body}");
    }
}

#[test]
fn local_var_move_and_join_errors_are_source_diagnostics() {
    for (body, expected) in [
        (
            "let taken = token; println(token.value)",
            "use of moved binding 'token'",
        ),
        (
            "if flag { let taken = token }; ()",
            "inconsistent initialization",
        ),
        (
            "while flag { let taken = token }; ()",
            "inconsistent initialization",
        ),
        (
            "if flag { let taken = token }; token = Token(value: 2)",
            "inconsistent initialization",
        ),
    ] {
        let source = format!("class Token {{ let value: Int32 }} fn f(flag: Bool) {{ var token = Token(value: 1); {body} }} fn main() {{}}");
        let error = try_run_program(&source).expect_err(&source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(error.to_string().contains(expected), "{body}: {error}");
        assert!(error.span().is_some(), "{error}");
    }
}

#[test]
fn local_var_reassignment_cannot_invalidate_argument_receiver_or_callee_borrows() {
    for source in [
        "class Token {} fn use_it(token: &Token, flag: Unit) {} fn main() { var token = Token(); use_it(token, { token = Token(); () }) }",
        "class Token { fn use_it(flag: Unit) {} } fn main() { var token = Token(); token.use_it({ token = Token(); () }) }",
        "fn main() { var call = fn (n: Int32) -> Int32 { n }; let result = call({ call = fn (n: Int32) -> Int32 { n + 1 }; 0 }); () }",
    ] {
        let error = try_run_program(source).expect_err(source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(error.to_string().contains("while a borrow is still in use"), "{error}");
        assert!(error.span().is_some(), "{error}");
    }
}

#[test]
fn local_var_shadowing_preserves_explicit_closure_snapshots_and_aliases() {
    run_program(
        r#"
        fn expect(ok: Bool) { if !ok { panic("closure snapshot") } }
        fn main() {
            var n = 1
            let snapshot = n
            let read = fn () -> Int32 { snapshot }
            let alias = read
            n = 2
            var snapshot = 100
            expect(alias() == 1)
            snapshot = 101
            expect(alias() == 1)
            let text = "original"
            let read_text = fn () -> String { text }
            let text_alias = read_text
            var text = "replacement"
            expect(text_alias() == "original")
            {
                let text = "nested"
                expect(text_alias() == "original")
            }
            expect(text == "replacement")
        }
    "#,
    );
}

#[test]
fn local_var_declarations_stay_inside_their_concurrent_arm() {
    for mode in ["parallel", "race"] {
        let source = format!(
            "fn main() {{ let value = {mode} {{\n| var private = 1\n| private = 2\n}}; () }}"
        );
        let error = try_run_program(&source).expect_err(&source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(
            error.to_string().contains("unknown value 'private'"),
            "{error}"
        );
    }
}

#[test]
fn local_var_move_closure_keeps_persistent_private_state() {
    run_program(
        r#"
        fn expect(ok: Bool) { if !ok { panic("mutable capture") } }
        fn twice(read: fn() -> Int32) -> Int32 { read() + read() }
        class Token { let value: Int32 }
        fn main() {
            var n = 1
            let read = move fn() -> Int32 { n = n + 1; n }
            expect(read() == 2)
            expect(read() == 3)
            n = 100
            expect(read() == 4)
            expect(n == 100)
            let alias = read
            expect(twice(alias) == 11)
            var text = "a"
            let append = move fn() -> String { text = text + "b"; text }
            expect(append() == "ab")
            expect(append() == "abb")
            text = "outside"
            expect(append() == "abbb")
            expect(text == "outside")
            var token = Token(value: 1)
            let replace = move fn() -> Int32 { token = Token(value: token.value + 1); token.value }
            expect(replace() == 2)
            expect(replace() == 3)
            token = Token(value: 20)
            expect(token.value == 20)
            var literal = 10
            expect((move fn() -> Int32 { literal = literal + 1; literal })() == 11)
        }
    "#,
    );
}

#[test]
fn local_var_move_capture_rejects_consumed_bindings_and_environment_moves() {
    for (body, expected) in [
        ("var n = 1; let read = move fn() -> Int32 { n }; println(n)", "use of moved binding 'n'"),
        ("var n = \"a\"; let read = move fn() -> String { n }; println(n)", "use of moved binding 'n'"),
        ("var n = 1; let read = move fn() -> Int32 { n }; let again = move fn() -> Int32 { n }", "use of moved binding 'n'"),
        ("var n = 1; let read = move fn() -> Int32 { let nested = move fn() -> Int32 { n }; nested() }", "borrowed closure environment"),
        ("var token = Token(value: 1); let take = move fn() -> Token { token }", "cannot move an owned capture"),
    ] {
        let source = format!("class Token {{ let value: Int32 }} fn main() {{ {body}; () }}");
        let error = try_run_program(&source).expect_err(&source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(error.span().is_some(), "{error}");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn local_var_move_capture_shadowing_and_nested_local_state() {
    run_program(
        r#"
        fn expect(ok: Bool) { if !ok { panic("capture shadowing") } }
        fn main() {
            var n = 1
            let next = move fn() -> Int32 {
                n = n + 1
                { var n = 100; n = n + 1; expect(n == 101) }
                n
            }
            expect(next() == 2)
            expect(next() == 3)
            var seed = 10
            let nested = move fn() -> Int32 {
                var local = seed
                let increment = move fn() -> Int32 { local = local + 1; local }
                seed = seed + 1
                increment()
            }
            expect(nested() == 11)
            expect(nested() == 12)
        }
    "#,
    );
}

#[test]
fn local_var_move_capture_rejects_actual_suspension_and_hidden_waits() {
    for body in [
        "time.sleep(1ms); n",
        "wait(); n",
        "let result = parallel {\n| 1\n| 2\n}; n",
        "let result = do { Ask.get() } with { Ask.get() => hidden() }; n",
    ] {
        let source = format!("eff time {{ @suspends fn sleep(duration: Duration) -> Unit }} eff Ask {{ fn get() -> Int32 }} fn hidden() -> Int32 {{ let values = parallel {{\n| 1\n| 2\n}}; values.0 }} fn wait() effects {{ time }} {{ time.sleep(1ms) }} fn main() {{ var n = 1; let read = move fn() -> Int32 {{ {body} }} }}");
        let error = try_run_program(&source).expect_err(&source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(
            error
                .to_string()
                .contains("mutable captures cannot suspend"),
            "{error}"
        );
        assert!(error.span().is_some(), "{error}");
    }
}

#[test]
fn local_var_move_capture_pure_body_allows_pending_abi_widening() {
    run_program(
        r#"
        fn main() {
            var n = 1
            let counter = move fn() -> Int32 { n = n + 1; n }
            let dormant = fn() -> Int32 {
                let values = parallel {
                    | 1
                    | 2
                }
                values.0 + values.1
            }
            if counter() != 2 { panic("pending ABI") }
            if counter() != 3 { panic("persistent state") }
        }
    "#,
    );
}

#[test]
fn local_var_move_capture_rejects_task_duplication_and_field_borrow_conflicts() {
    for source in [
        "fn main() { var n = 1; let read = move fn() -> Int32 { n = n + 1; n }; let results = parallel {\n| read()\n| read()\n}; () }",
        "class Token {} fn consume(token: &Token, flag: Unit) {} fn main() { var token = Token(); let read = move fn() -> Unit { consume(token, { token = Token(); () }) }; read() }",
    ] {
        let error = try_run_program(source).expect_err(source);
        assert_eq!(error.stage(), crate::diagnostic::Stage::Semantic, "{error}");
        assert!(error.span().is_some(), "{error}");
    }
}

#[test]
fn local_var_move_capture_specializes_generic_state_and_parameters() {
    run_program(
        r#"
        fn make(T: type, initial: T) -> fn(T) -> Unit {
            var state = initial
            move fn(value: T) -> Unit { state = value }
        }
        fn main() {
            let integer = make(Int32, 1)
            integer(2)
            let text = make(String, "a")
            text("b")
        }
    "#,
    );
}
