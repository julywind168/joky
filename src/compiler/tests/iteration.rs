use super::*;

#[test]
fn for_collects_in_order_with_index_skip_and_prefix_break() {
    run_program(
        r#"
        fn main() {
            let u0: UInt64 = 0; let u1: UInt64 = 1; let u2: UInt64 = 2; let u3: UInt64 = 3;
            let input = List(10, 20, 30, 40)
            let output = for (i, item) in input {
                if i == u1 { continue } else { }
                if i == u3 { break } else { item + 1 }
            }
            if output.length() != u2 { panic("wrong count") } else { }
            if output.head()! != 11 { panic("wrong first") } else { }
            if output.tail()!.head()! != 31 { panic("wrong second") } else { }
            let indices = for (i, item) in input { i }
            if indices.reverse().head()! != u3 { panic("wrong index") } else { }
        }
    "#,
    );
}

#[test]
fn for_empty_unit_shared_and_nested_outputs_release_ownership() {
    run_program(
        r#"
        fn main() {
            let u0: UInt64 = 0; let u1: UInt64 = 1; let u2: UInt64 = 2; let u3: UInt64 = 3;
            let empty: List(String) = List#{}
            let none = @parallel(limit: 3) for item in empty { item }
            if !none.is_empty() { panic("empty input") } else { }
            let input = List("a", "b", "c")
            let units: List(Unit) = for item in input { () }
            if units.length() != u3 { panic("Unit count") } else { }
            let unit = units.head()!
            let nested = @parallel(limit: 2) for item in input { List(item, item) }
            if nested.length() != u3 { panic("nested count") } else { }
            if nested.head()!.head()! != "a" { panic("nested value") } else { }
        }
    "#,
    );
}

#[test]
fn parallel_for_preserves_order_across_suspend_skip_and_nested_limit_one() {
    run_program(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        fn main() effects { time } {
            let u0: UInt64 = 0; let u1: UInt64 = 1; let u2: UInt64 = 2; let u3: UInt64 = 3;
            let bound: UInt64 = 2
            let result = @parallel(limit: bound) for (i, item) in List("a", "b", "c", "d") {
                if i == u0 { time.sleep(5ms) } else { time.sleep(0ms) }
                if i == u1 { continue } else { item }
            }
            if result.length() != u3 { panic("skip") } else { }
            if result.head()! != "a" { panic("completion order leaked") } else { }
            if result.tail()!.head()! != "c" { panic("skip order") } else { }
            let nested = @parallel(limit: 1) for outer in List(1, 2) {
                let inner = @parallel(limit: 1) for item in List(10, 20) {
                    time.sleep(0ms)
                    item + outer
                }
                inner.head()!
            }
            if nested.head()! != 11 { panic("nested") } else { }
            if nested.tail()!.head()! != 12 { panic("nested order") } else { }
        }
    "#,
    );
}

#[test]
fn for_allows_nested_loop_break_and_iteration_closures() {
    run_program(
        r#"
        fn main() {
            let u0: UInt64 = 0; let u1: UInt64 = 1; let u2: UInt64 = 2; let u3: UInt64 = 3;
            let input = List(1, 2)
            let output = @parallel(limit: 2) for item in input {
                let offset = loop { break 3 }
                let compute = fn(value: Int32) -> Int32 { value + item }
                compute(offset)
            }
            if output.head()! != 4 { panic("closure capture") } else { }
            if output.tail()!.head()! != 5 { panic("closure isolation") } else { }
            let filtered = for item in List(Some(1), None, Some(3)) {
                match item { Some(value) => value, None => continue }
            }
            if filtered.length() != u2 { panic("match continue") } else { }
        }
    "#,
    );
}

#[test]
fn pipe_closures_and_trailing_closures_desugar_to_closure_calls() {
    run_program(
        r#"
        fn apply(value: Int32, action: fn(input: Int32) -> Int32) -> Int32 { action(value) }
        fn produce(action: fn() -> Int32) -> Int32 { action() }
        fn pick(count: Int32, action: fn() -> Int32) -> Int32 {
            if count > 0 { action() } else { 0 }
        }
        fn main() {
            let doubled = apply(4, |input: Int32| -> Int32 { input * 2 })
            if doubled != 8 { panic("pipe closure with parameter") } else { }

            let constant = produce(|| -> Int32 { 7 })
            if constant != 7 { panic("empty pipe closure") } else { }

            let trailing = apply(5) |input: Int32| -> Int32 { input + 1 }
            if trailing != 6 { panic("trailing closure on call") } else { }

            let bare = produce || -> Int32 { 9 }
            if bare != 9 { panic("trailing closure without arguments") } else { }

            let omitted = produce { 10 }
            if omitted != 10 { panic("trailing closure with omitted ||") } else { }

            let picked = pick(2) { 11 }
            if picked != 11 { panic("trailing closure with omitted || and arguments") } else { }

            let factor = 3
            let captured = apply(2) |input: Int32| -> Int32 { input * factor }
            if captured != 6 { panic("trailing closure capture") } else { }

            let moved = apply(2, move |input: Int32| -> Int32 { input + factor })
            if moved != 5 { panic("move pipe closure") } else { }
        }
    "#,
    );
}

#[test]
fn for_rejects_invalid_limits_bindings_and_escaping_control() {
    for (source, expected) in [
        (
            "fn main() { for x in 1 { x } }",
            "for requires a List, a Cursor implementation, or an IntoCursor container",
        ),
        (
            "fn main() { for (x, x) in List(1) { x } }",
            "bindings must be distinct",
        ),
        (
            "fn main() { @parallel(limit: 0) for x in List(1) { x } }",
            "greater than zero",
        ),
        (
            "fn main() { @parallel(limit: 2) for x in List(1) { break } }",
            "break cannot exit a parallel for",
        ),
        (
            "fn main() { for x in List(1) { break () } }",
            "only supports bare break",
        ),
        (
            "fn main() { for x in List(1) { let f = fn() -> Unit { continue } } }",
            "continue",
        ),
        (
            "fn main() { @parallel(size: 2) for x in List(1) { x } }",
            "limit",
        ),
        ("fn main() { @parallel(limit: 2) 42 }", "for expression"),
    ] {
        let error = try_run_program(source).expect_err(source).to_string();
        assert!(error.contains(expected), "expected {expected:?}: {error}");
    }
}

#[test]
fn parallel_for_rejects_repeated_unique_capture() {
    let error = try_run_program(
        r#"
        class Token { let value: Int32 }
        fn main() {
            let u0: UInt64 = 0; let u1: UInt64 = 1; let u2: UInt64 = 2; let u3: UInt64 = 3;
            let token = Token(value: 2)
            let values = @parallel(limit: 2) for item in List(1, 2) { token.value + item }
        }
    "#,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot capture owned or borrowed local"),
        "{error}"
    );
}

#[test]
fn parallel_for_abort_cancels_and_drains_before_parent_handler() {
    run_program(
        r#"
        eff Failure { @aborts fn stop(message: String) -> Unit }
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        fn main() effects { time } {
            let u0: UInt64 = 0; let u1: UInt64 = 1; let u2: UInt64 = 2; let u3: UInt64 = 3;
            let caught = do {
                let output = @parallel(limit: 2) for item in List(1, 2, 3) {
                    if item == 1 { time.sleep(60000ms) } else { Failure.stop("failed") }
                    item
                }
                "not caught"
            } with { Failure.stop(message) => message }
            if caught != "failed" { panic("lost failure payload") } else { }
        }
    "#,
    );
}

#[test]
fn for_scopes_child_branches_per_iteration_even_on_continue_and_break() {
    run_program(
        r#"
        eff time { @suspends fn sleep(duration: Duration) -> Unit }
        eff Failure { @aborts fn stop() -> Unit }
        fn main() effects { time } {
            let result = for item in List(1, 2, 3) {
                branch { time.sleep(0ms) }
                if item == 1 { continue } else { }
                if item == 3 { break } else { item }
            }
            if result.head()! != 2 { panic("branch iteration") } else { }
            let caught = do {
                let output = @parallel(limit: 1) for item in List(1, 2) {
                    branch { Failure.stop() }
                    continue
                }
                0
            } with { Failure.stop() => 7 }
            if caught != 7 { panic("branch failure") } else { }
        }
    "#,
    );
}

#[test]
fn parallel_for_uses_file_provider_and_iteration_handler_frames() {
    let path = std::env::temp_dir().join(format!("joky-for-file-{}", std::process::id()));
    std::fs::write(&path, "contents").unwrap();
    let source = format!(
        r#"
        eff file {{ @suspends fn read(path: String) -> Result(String, String) }}
        eff Marker {{ fn value() -> String }}
        fn main() effects {{ file }} {{
            let result = @parallel(limit: 2) for item in List("a", "b", "c") {{
                do {{
                    let content = file.read("{}")!
                    if content != "contents" {{ panic("file content") }} else {{ }}
                    Marker.value()
                }} with {{ Marker.value() => item }}
            }}
            if result.head()! != "a" {{ panic("handler first") }} else {{ }}
            if result.tail()!.head()! != "b" {{ panic("handler second") }} else {{ }}
            if result.reverse().head()! != "c" {{ panic("handler last") }} else {{ }}
        }}
    "#,
        path.display()
    );
    let result = try_run_program(&source);
    std::fs::remove_file(path).unwrap();
    result.unwrap();
}

#[test]
fn for_generic_instantiation_and_lexical_capture_shadowing() {
    run_program(
        r#"
        fn copy_items(T: type, input: List(T)) -> List(T) {
            @parallel(limit: 2) for item in input { item }
        }
        class Token { let value: Int32 }
        fn main() {
            let item = Token(value: 99)
            let collect = fn() -> List(Int32) { for item in List(1, 2) { item } }
            let result = copy_items(String, List("a", "b"))
            if result.head()! != "a" { panic("generic List") } else { }
            if collect().head()! != 1 { panic("shadowed capture") } else { }
            let integers = copy_items(Int32, List(3, 4))
            if integers.head()! != 3 { panic("second instance") } else { }
        }
    "#,
    );
}

#[test]
fn unit_values_round_trip_through_collection_storage() {
    run_program(
        r#"
        fn main() {
            let units: List(Unit) = for item in List(1, 2) { () }
            let mutable = MutList#{units.head()!}
            let index: UInt64 = 0
            let unit = mutable.get(index)!
            let popped = mutable.pop()!
            let mapped = Map#{"key" => ()}
            let value = mapped.get("key")!
            let mutable_map = MutMap#{"key" => ()}
            let other = mutable_map.get("key")!
        }
    "#,
    );
}
