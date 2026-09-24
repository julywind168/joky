use super::*;

#[test]
fn cown_pending_restores_loops_and_returned_capabilities() {
    run_program(include_str!("../../../tests/fixtures/cown_pending.jk"));
}

#[test]
fn cown_abort_releases_multiple_leases_before_nested_handlers() {
    run_program(include_str!("../../../tests/fixtures/cown_abort.jk"));
}

#[test]
fn cown_lease_cleanup_respects_inner_and_outer_loop_targets() {
    run_program(
        r#"
        class State {
            var value: Int32 = 0
            fn bump() { self.value = self.value + 1 }
        }
        fn main() {
            let counter = Cown.new(State())
            loop { when (counter) |state| { state.bump(); break } }
            when (counter) |state| {
                loop { state.bump(); break }
                state.bump()
            }
            let value = when (counter) |state| { state.value }
            if value != 3 { panic("incorrect lease cleanup") }
        }
        "#,
    );
}

#[test]
fn cown_abort_releases_lease_before_returning_failure() {
    for call in ["Failure.stop(state.text)", "fail(state.text)"] {
        let source = format!(
            r#"
            eff Failure {{ @aborts fn stop(message: String) -> Unit }}
            class Payload {{ let text: String }}
            fn fail(message: String) effects {{ Failure }} {{ Failure.stop(message) }}
            fn main() effects {{ Failure }} {{
                let payload = Cown.new(Payload(text: "owned" + " text"))
                when (payload) |state| {{ {call} }}
            }}
            "#
        );
        let error = try_run_program(&source).unwrap_err().to_string();
        assert!(
            error.contains("unhandled aborting effect operation 'Failure.stop'"),
            "{call}: {error}"
        );
    }
}

#[test]
fn cown_payload_drops_nested_fields_and_closure_captures() {
    run_program(include_str!("../../../tests/fixtures/cown_payload_drop.jk"));
}

#[test]
fn cown_payload_drops_after_suspended_task_cancellation() {
    run_program(include_str!(
        "../../../tests/fixtures/cown_payload_cancel.jk"
    ));
}

#[test]
fn cown_payload_drops_after_task_abort() {
    let result = try_run_program(
        r#"
        eff Failure { @aborts fn stop(message: String) -> Unit }
        class Payload { let text: String }
        fn fail(message: String) effects { Failure } { Failure.stop(message) }
        fn main() effects { Failure } {
            let results = parallel {
                | {
                    let payload = Cown.new(Payload(text: "owned" + " text"))
                    fail("stop")
                    let text = when (payload) |state| { state.text }
                    println(text)
                }
                | {}
            }
        }
        "#,
    );
    let error = result.unwrap_err().to_string();
    assert!(error.contains("Failure.stop"), "{error}");
}

#[test]
fn cown_payload_drops_native_collection_elements() {
    run_program(
        r#"
        @intrinsic class MutList(T: type) {
            fn push(&self, item: T) -> Unit
            fn length(&self) -> UInt64
        }
        fn main() {
            let values = MutList(String)()
            values.push("owned" + " item")
            let payload = Cown.new(values)
            let count = when (payload) |items| { items.length() }
            let expected: UInt64 = 1
            if count != expected { panic("missing item") }
        }
        "#,
    );
}

#[test]
fn compiles_intrinsic_mut_list_declaration() {
    let source = r#"
            @intrinsic
            pub class MutList(T: type) {
                fn length(&self) -> UInt64
                fn capacity(&self) -> UInt64
                fn push(&self, item: T) -> Unit
                fn get(&self, index: UInt64) -> Option(T)
                fn set(&self, index: UInt64, item: T) -> Unit
                fn pop(&self) -> Option(T)
            }

            fn main() {
                let values = MutList(Int32)()
                values.push(42)
                println(values.length())
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_intrinsic_mut_list_with_first_class_type_syntax() {
    run_program(
        r#"
        @intrinsic
        pub class MutList(T: type) {
            fn length(&self) -> UInt64
            fn push(&self, item: T) -> Unit
        }

        fn main() {
            let values: MutList(Int64) = MutList(Int64)()
            values.push(42)
            println(values.length())
        }
        "#,
    );
}

#[test]
fn checks_intrinsic_generic_method_parameters_after_instance_substitution() {
    let source = r#"
        @intrinsic
        pub class MutList(T: type) {
            fn length(&self) -> UInt64
            fn push(&self, item: T) -> Unit
        }

        fn main() {
            let values: MutList(Int64) = MutList(Int64)()
            values.push("wrong")
        }
        "#;
    assert!(try_run_program(source).is_err());
}

#[test]
fn compiles_intrinsic_mut_map_and_set_with_first_class_type_syntax() {
    run_program(
        r#"
        @intrinsic
        pub class MutMap(K: type + Hash + Eq, V: type) {
            fn length(&self) -> UInt64
            fn insert(&self, key: K, value: V) -> Option(V)
            fn get(&self, key: K) -> Option(V)
        }

        @intrinsic
        pub class MutSet(T: type + Hash + Eq) {
            fn length(&self) -> UInt64
            fn add(&self, item: T) -> Bool
        }

        fn main() {
            let values: MutMap(String, Int64) = MutMap(String, Int64)()
            let _ = values.insert(key: "answer", value: 42)
            let tags: MutSet(String) = MutSet(String)()
            let _ = tags.add(item: "joky")
            println(values.length() + tags.length())
        }
        "#,
    );
}

#[test]
fn checks_intrinsic_generic_method_return_types() {
    let source = r#"
        @intrinsic
        pub class MutList(T: type) {
            fn pop(&self) -> Option(T)
        }

        fn main() {
            let values: MutList(String) = MutList(String)()
            let item: Option(String) = values.pop()
        }
        "#;
    run_program(source);
}

#[test]
fn runs_a_single_cown_when_lease_and_releases_payload() {
    let source = r#"
            class Counter {
                var value: Int32 = 0
                fn increment() { self.value = self.value + 1 }
            }

            fn main() {
                let counter = Cown.new(Counter(value: 0))
                let updated = when (counter) |state| {
                    state.increment()
                    state.value
                }
                println(updated)
            }
        "#;
    run_program(source);
}

#[test]
fn runs_an_implicit_when_binding_for_simple_cown_names() {
    let source = r#"
            class Counter {
                var value: Int32 = 0
                fn increment() { self.value = self.value + 1 }
            }

            fn main() {
                let counter = Cown.new(Counter(value: 0))
                when (counter) {
                    counter.increment()
                }
                let updated = when (counter) {
                    counter.value
                }
                println(updated)
            }
        "#;
    run_program(source);
}

#[test]
fn rejects_non_pointer_cown_payloads() {
    let source = "struct Pair { let left: Int32 } fn main() { Cown.new(Pair(left: 1)) }";
    assert!(try_run_program(source).is_err());
}

#[test]
fn compiles_duration_literals_with_unit_and_composite_forms() {
    let source = r#"
            fn keep(value: Duration) -> Duration { value }
            fn main() {
                let short: Duration = 1000ms
                let second = 1s
                let long = 1h10m100s
                let _a = keep(short)
                let _b = keep(second)
                let _c = keep(long)
            }
        "#;
    run_program(source);
}

#[test]
fn runs_a_multi_cown_when_lease() {
    let source = r#"
            class Counter {
                var value: Int32 = 0
                fn set(value: Int32) { self.value = value }
            }

            fn main() {
                let source = Cown.new(Counter(value: 4))
                let target = Cown.new(Counter(value: 0))
                let updated = when (source, target) |src, dst| {
                    dst.set(value: src.value + 1)
                    dst.value
                }
                println(updated)
            }
        "#;
    run_program(source);
}

#[test]
fn rejects_nested_cown_when_leases() {
    let source = r#"
            class Counter { var value: Int32 = 0 }

            fn main() {
                let first = Cown.new(Counter(value: 1))
                let second = Cown.new(Counter(value: 2))
                let total = when (first) |left| {
                    when (second) |right| {
                        left.value + right.value
                    }
                }
                println(total)
            }
        "#;
    assert!(try_run_program(source).is_err());
}

#[test]
fn rejects_duplicate_cowns_in_a_when_lease() {
    let source = r#"
            class Counter { var value: Int32 = 0 }
            fn main() {
                let counter = Cown.new(Counter(value: 1))
                when (counter, counter) |left, right| { left.value }
            }
        "#;
    assert!(try_run_program(source).is_err());
}

#[test]
fn contended_cown_progresses_under_branch_oversubscription() {
    // 16 branches over the worker pool (moderate oversubscription on small machines): heavily oversubscribed
    // contention. Every branch must finish all of its acquire/release rounds
    // (no waiter is left behind) and every payload must be released.
    const BRANCHES: usize = 16;
    const ROUNDS: usize = 8;
    let bump = "when (counter) |state| { state.bump() }; ";
    let arms = (0..BRANCHES)
        .map(|index| {
            format!(
                "| {{ {}{} }}",
                bump.repeat(ROUNDS),
                index // each arm yields a distinct value so joins are observable
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let sum = (0..BRANCHES)
        .map(|index| format!("results.{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let source = format!(
        "class Counter {{\n\
         \x20   var value: Int32 = 0\n\
         \x20   fn bump() {{ self.value = self.value + 1 }}\n\
         }}\n\
         fn main() {{\n\
         \x20   let counter = Cown.new(Counter(value: 0))\n\
         \x20   let results = parallel {{\n{arms}\n}}\n\
         \x20   let total = when (counter) |state| {{ state.value }}\n\
         \x20   println(total)\n\
         \x20   println({sum})\n\
         }}"
    );
    Compiler::new()
        .unwrap()
        .run_program(&source)
        .expect("every oversubscribed branch should make progress on the contended cown");
}

#[test]
fn region_inherits_through_factories_and_tasks() {
    run_program(
        r#"
        class State { var value: Int32 = 0; fn bump() { self.value = self.value + 1 } }
        fn make() -> Cown(State) { Cown.new(State()) }
        fn main() {
            let total = region {
                let state = make()
                let _ = parallel {
                    | when (state) |s| { s.bump() }
                    | when (state) |s| { s.bump() }
                }
                when (state) |s| { s.value }
            }
            if total != 2 { panic("region result") }
        }
    "#,
    );
}

#[test]
fn region_rejects_direct_and_indirect_escape() {
    for body in [
        "let bad = region { Cown.new(State()) }",
        "let bad = region { Some(Cown.new(State())) }",
        "let bad = region { List(Cown.new(State())) }",
        "let bad = region { make() }",
        "let bad = region { for i in List(1, 2) { make() } }",
        "let bad = region { @parallel(limit: 2) for i in List(1, 2) { make() } }",
        "var outer = make(); region { outer = make() }",
        "let outer = Cown.new(Boxed()); region { let inner = make(); when (outer) |b| { b.set(inner) } }",
        "let outer = Cown.new(Boxed()); region { put(outer, make()) }",
        "let bad = region { let s = make(); fn() -> Int32 { when (s) |v| { v.value } } }",
    ] {
        let source = format!(r#"
            class State {{ var value: Int32 = 0 }}
            class Boxed {{ var next: Option(Cown(State)) = None; fn set(value: Cown(State)) {{ self.next = Some(value) }} }}
            fn make() -> Cown(State) {{ Cown.new(State()) }}
            fn put(target: Cown(Boxed), value: Cown(State)) {{ when (target) |b| {{ b.set(value) }} }}
            fn main() {{ {body}; () }}
        "#);
        let error = Compiler::new().unwrap().compile_object_program(&source).unwrap_err().to_string();
        assert!(error.contains("region"), "{body}: {error}");
    }
}

#[test]
fn region_reclaims_cycles_and_preserves_ancestor_cowns() {
    run_program(
        r#"
        class Node { var next: Option(Cown(Node)) = None; var value: Int32 = 0; fn link(other: Cown(Node)) { self.next = Some(other) }; fn set(value: Int32) { self.value = value } }
        fn main() {
            let outer = Cown.new(Node())
            region {
                let a = Cown.new(Node())
                let b = Cown.new(Node())
                when (a, b) |x, y| { x.link(b); y.link(a) }
                when (outer) |x| { x.set(7) }
            }
            let value = when (outer) |x| { x.value }
            if value != 7 { panic("ancestor released") }
        }
    "#,
    );
}

#[test]
fn region_drains_suspensions_and_releases_cycles_before_root_closes() {
    let source = r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        class Node {
            var next: Option(Cown(Node)) = None
            fn link(other: Cown(Node)) { self.next = Some(other) }
        }
        fn make() -> Cown(Node) { Cown.new(Node()) }
        fn child() effects { time } {
            let a = make()
            time.sleep(1ms)
            let b = make()
            when (a, b) |x, y| { x.link(b); y.link(a) }
        }
        fn main() effects { time } {
            var n = 0
            while n < 5 {
                region {
                    time.sleep(1ms)
                    branch { child() }
                    let _ = parallel {
                        | child()
                        | child()
                    }
                }
                n = n + 1
            }
        }
    "#;
    let program = syntax::parse_program(source).unwrap();
    let types = sema::check_program(&program).unwrap();
    let core = CoreProgram::lower(program, types).unwrap();
    let mut mir = MirProgram::lower(&core).unwrap();
    MirPassManager::default_pipeline()
        .run(&mut mir)
        .unwrap_or_else(|error| panic!("{error:?}\n{}", mir.dump()));
    let scope = joky_runtime::host::RuntimeScope::new();
    let _guard = scope.enter();
    let _providers = super::super::providers::NativeProviderRegistry::install(mir.types(), &scope);
    let mut backend = CraneliftBackend::new().unwrap();
    backend.compile_and_run_program(&mir, &scope).unwrap();
    scope.wait_for_idle();
    assert_eq!(
        joky_runtime::host::testing::managed_objects(&scope),
        0,
        "explicit regions retained objects in the root"
    );
    scope.close_and_wait();
}

#[test]
fn region_cleanup_covers_break_continue_abort_and_cancellation() {
    run_program(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Failure { @aborts fn stop() -> Unit }
        class State { var value: Int32 = 0 }
        fn main() effects { time } {
            var n = 0
            while n < 3 {
                n = n + 1
                region { let c = Cown.new(State()); if n < 3 { continue } else { break } }
            }
            do { region { let c = Cown.new(State()); Failure.stop() } } with { Failure.stop() => () }
            let _ = race {
                | region { let c = Cown.new(State()); time.sleep(60s); 1 }
                | { time.sleep(2ms); 2 }
            }
            region { let c = Cown.new(State()); () }
        }
    "#,
    );
}

#[test]
fn region_rejects_mutable_alias_escape_and_indirect_entry_under_lease() {
    for body in [
        "let items = MutList(Cown(State))(); region { items.push(make()) }",
        "let items = MutList(Cown(State))(); region { append(items, make()) }",
        "let outer = make(); when (outer) |s| { enter() }",
        "let outer = make(); when (outer) |s| { indirect() }",
        "let outer = make(); region { let f = fn() -> Unit { when (outer) |s| { s.link(make()) } }; f() }",
    ] {
        let source = format!(
            r#"
            @intrinsic class MutList(T: type) {{ fn push(&self, item: T) -> Unit }}
            class State {{ var next: Option(Cown(State)) = None; fn link(n: Cown(State)) {{ self.next = Some(n) }} }}
            fn make() -> Cown(State) {{ Cown.new(State()) }}
            fn append(items: &MutList(Cown(State)), item: Cown(State)) {{ items.push(item) }}
            fn enter() {{ region {{ () }} }}
            fn indirect() {{ enter() }}
            fn main() {{ {body}; () }}
        "#
        );
        let error = Compiler::new()
            .unwrap()
            .compile_object_program(&source)
            .unwrap_err()
            .to_string();
        assert!(error.contains("region"), "{body}: {error}");
    }
}

#[test]
fn region_accepts_nested_ancestor_results_and_plain_snapshots() {
    run_program(
        r#"
        class State { var value: Int32 = 42 }
        fn main() {
            region {
                let outer = Cown.new(State())
                let alias = region { let inner = Cown.new(State()); outer }
                let text = region {
                    let inner = Cown.new(State())
                    let number = when (inner) |s| { s.value }
                    if number == 42 { "plain" + " snapshot" } else { "bad" }
                }
                if text != "plain snapshot" { panic("snapshot") }
                if when (alias) |s| { s.value } != 42 { panic("ancestor") }
            }
        }
    "#,
    );
}

#[test]
fn region_rejects_named_syntax_and_destructors_with_cowns() {
    assert!(syntax::parse_program("fn main() { region request {} }").is_err());
    let source = r#"
        class State { var value: Int32 = 0 }
        class Watch { let state: Cown(State) }
        impl Drop for Watch { fn drop(&self) { println(when (self.state) |s| { s.value }) } }
        fn helper(state: Cown(State)) { let watch = Watch(state) }
        fn main() { region { helper(Cown.new(State())) } }
    "#;
    let error = Compiler::new()
        .unwrap()
        .compile_object_program(source)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("destructor requires a synchronous"),
        "{error}"
    );
}

#[test]
fn region_preserves_branch_failure_and_error_propagation() {
    run_program(
        r#"
        eff Failure { @aborts fn stop() -> Unit }
        class State {}
        fn main() {
            let result = do {
                region { let c = Cown.new(State()); branch { Failure.stop() }; 0 }
            } with { Failure.stop() => 7 }
            if result != 7 { panic("lost region child failure") }
        }
    "#,
    );
    run_program(
        r#"
        class State {}
        fn fail() -> Result(Int32, String) {
            region {
                let c = Cown.new(State())
                let value: Result(Int32, String) = Err("stopped")
                let result = value?
                Ok(result)
            }
        }
        fn main() { if fail().is_ok() { panic("lost error") } }
    "#,
    );
}

#[test]
fn region_loop_exits_observe_child_failure() {
    for transfer in ["break", "continue"] {
        run_program(&format!(
            r#"
            eff Failure {{ @aborts fn stop() -> Unit }}
            class State {{}}
            fn main() {{
                let result = do {{
                    var n = 0
                    while n < 1 {{
                        n = n + 1
                        region {{
                            let c = Cown.new(State())
                            branch {{ Failure.stop() }}
                            region {{ {transfer} }}
                        }}
                    }}
                    0
                }} with {{ Failure.stop() => 7 }}
                if result != 7 {{ panic("lost loop child failure") }}
            }}
        "#
        ));
    }
}

#[test]
fn when_until_parks_rechecks_multiple_cowns_and_cancels() {
    run_program(include_str!("../../../tests/fixtures/cown_until.jk"));
}

#[test]
fn when_until_rejects_impure_or_non_boolean_conditions() {
    for guard in [
        "s.value",
        "{ s.value = 1; true }",
        "s.change()",
        "{ println(1); true }",
    ] {
        let source = format!("class State {{ var value: Int32 = 0; fn change() -> Bool {{ self.value = 1; true }} }} fn main() {{ let a = Cown.new(State()); when (a) |s| until {guard} {{ () }} }}");
        assert!(
            Compiler::new().unwrap().run_program(&source).is_err(),
            "{guard}"
        );
    }
}
