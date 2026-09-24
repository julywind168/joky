use super::*;

#[test]
fn nominal_types_have_default_debug_and_explicit_impls_override_it() {
    run_program(
        r#"
        struct Point { let x: Int32; let label: String }
        class Boxed { let point: Point }
        enum Shape { Empty; Dot(point: Point); Box(value: Boxed) }
        struct Secret { let ignored: List(Int32) }
        impl Debug for Secret { fn debug(&self) -> String { "redacted" } }
        class Ordinary { fn debug(&self) -> Int32 { 7 } }
        fn format(T: type + Debug, value: T) -> String { value.debug() }
        fn main() {
            let point = Point(x: 1, label: "p")
            if point.debug() != "Point(x: 1, label: \"p\")" { panic("struct") }
            let value = Boxed(point: point)
            echo value
            if value.debug() != "Boxed(point: Point(x: 1, label: \"p\"))" { panic("class") }
            if Shape.Empty.debug() != "Shape.Empty" { panic("empty enum") }
            if Shape.Dot(point: point).debug() != "Shape.Dot(point: Point(x: 1, label: \"p\"))" { panic("enum") }
            let owned = Shape.Box(value: value)
            echo owned
            if owned.debug() != "Shape.Box(value: Boxed(point: Point(x: 1, label: \"p\")))" { panic("owned enum") }
            if format(Secret(ignored: List(1))) != "redacted" { panic("override") }
            let ordinary = Ordinary()
            if ordinary.debug() != 7 { panic("inherent method") }
            echo ordinary
            if format(ordinary) != "Ordinary()" { panic("generic trait method") }
            if Dyn(Debug)(Ordinary()).debug() != "Ordinary()" { panic("dynamic trait method") }
            let dynamic = Dyn(Debug)(point)
            if dynamic.debug() != "Point(x: 1, label: \"p\")" { panic("dynamic default") }
            let composite = Dyn(Debug)(Some((1, true)))
            if composite.debug() != "Some((1, true))" { panic("dynamic aggregate") }
        }
    "#,
    );
}

#[test]
fn default_debug_handles_recursive_types_and_limits_depth() {
    run_program(
        r#"
        class Node { let value: Int32; let next: Option(Node) }
        fn make(depth: Int32) -> Node {
            Node(value: depth, next: if depth == 0 { None } else { Some(make(depth - 1)) })
        }
        fn main() {
            let short = make(1)
            if short.debug() != "Node(value: 1, next: Some(Node(value: 0, next: None)))" { panic("recursive") }
            let deep = make(80)
            if !deep.debug().contains("Node(<max-depth>)") { panic("depth limit") }
            if short.debug().contains("<max-depth>") { panic("context leaked") }
        }
    "#,
    );
}

#[test]
fn default_debug_rejects_unsupported_members_in_all_variants() {
    for source in [
        "struct Hidden { let values: Map(Int32, String) } fn main() { echo Hidden(values: Map.empty(Int32, String)); () }",
        "enum Hidden { Empty; Values(values: Map(Int32, String)) } fn main() { echo Hidden.Empty; () }",
        "class Hidden { let next: Option(Hidden); let values: Map(Int32, String) } fn inspect(value: &Hidden) { echo value; () } fn main() {}",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("Debug"), "{error}");
    }
}

#[test]
fn debug_formats_nested_tuples_options_and_results() {
    run_program(
        r#"
        fn formatted(T: type + Debug, value: T) -> String { value.debug() }
        fn nested(T: type + Debug, value: Option(T)) -> String { value.debug() }
        fn main() {
            if (1, "hello", true).debug() != "(1, \"hello\", true)" { panic("tuple") }
            if (1,).debug() != "(1,)" { panic("singleton") }
            let absent: Option(String) = None
            if absent.debug() != "None" { panic("none") }
            let empty: Option(Unit) = Some(())
            if empty.debug() != "Some(())" { panic("unit payload") }
            let failure: Result(Int32, String) = Err("bad\nvalue")
            if failure.debug() != "Err(\"bad\\nvalue\")" { panic("err") }
            let success: Result((Int32, Option(String)), String) = Ok((42, Some("x")))
            if formatted(success) != "Ok((42, Some(\"x\")))" { panic("nested") }
            if nested(Some((1, true))) != "Some((1, true))" { panic("generic bound") }
            let wide: Result(UInt64, String) = Ok(18446744073709551615)
            if wide.debug() != "Ok(18446744073709551615)" { panic("width") }
            echo (1, "hello")
            echo (1,)
            echo Some((absent, failure))
            ()
        }
    "#,
    );
}

#[test]
fn aggregate_debug_handles_owned_fields_and_loop_branches() {
    run_program(
        r#"
        struct Label { let text: String }
        impl Debug for Label { fn debug(&self) -> String { self.text.debug() } }
        class Value { let label: Label }
        impl Debug for Value { fn debug(&self) -> String { self.label.debug() } }
        class Wrapper {
            let value: Option((Value, Label))
            fn inspect(&self) -> String { self.value.debug() }
        }
        fn main() {
            for present in List(true, false, true) {
                let value: Option((Value, Label)) = if present {
                    Some((Value(label: Label(text: "owned")), Label(text: "shared")))
                } else { None }
                let wrapper = Wrapper(value: value)
                let expected = if present { "Some((\"owned\", \"shared\"))" } else { "None" }
                if wrapper.inspect() != expected { panic("borrowed field") }
                if wrapper.value.debug() != expected { panic("loop reuse") }
            }
            ()
        }
    "#,
    );
}

#[test]
fn debug_aggregates_require_every_payload_to_implement_debug() {
    for value in [
        "let value = (1, Hidden())",
        "let value = Some(Hidden())",
        "let value: Option(Hidden) = None",
        "let value: Result(Int32, Hidden) = Ok(1)",
        "let value: Result(Hidden, String) = Err(\"error\")",
    ] {
        for use_value in ["echo value", "value.debug()", "inspect(value)"] {
            let source = format!(
                "class Hidden {{ let values: Map(Int32, String) = Map.empty(Int32, String) }} fn inspect(T: type + Debug, value: T) {{}} fn main() {{ {value}; {use_value}; () }}"
            );
            let error = try_run_program(&source).unwrap_err().to_string();
            assert!(error.contains("Debug"), "{source}: {error}");
        }
    }
    let error = try_run_program(
        "fn inspect(T: type, value: Option(T)) -> String { value.debug() } fn main() {}",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("Debug"), "{error}");
}

#[test]
fn aggregate_debug_borrows_owned_payloads_and_skips_inactive_variants() {
    run_program(
        r#"
        class Value { let text: String }
        impl Debug for Value { fn debug(&self) -> String { self.text.debug() } }
        class Unused {}
        impl Debug for Unused { fn debug(&self) -> String { panic("inactive variant"); "unreachable" } }
        fn main() {
            let value = (Value(text: "kept"), Some(Value(text: "also kept")))
            echo value
            if value.debug() != "(\"kept\", Some(\"also kept\"))" { panic("owned tuple") }
            echo value
            let optional: Option(Unused) = None
            if optional.debug() != "None" { panic("empty option") }
            let ok: Result(String, Unused) = Ok("ok")
            if ok.debug() != "Ok(\"ok\")" { panic("ok") }
            let err: Result(Unused, String) = Err("err")
            if err.debug() != "Err(\"err\")" { panic("err") }
            let boxed = Some(Dyn(Debug)(Value(text: "boxed")))
            echo boxed
            if boxed.debug() != "Some(\"boxed\")" { panic("nested dynamic") }
        }
    "#,
    );
}

#[test]
fn debug_and_echo_preserve_primitive_types_and_managed_values() {
    run_program(
        r#"
        fn inspect(T: type + Debug, value: T) -> T { echo value }
        fn main() {
            let wide: UInt64 = echo(18446744073709551615)
            if wide.show() != "18446744073709551615" { panic("changed integer") }
            let original = "hello\n\t\"\\"
            let observed = inspect(echo(original))
            if observed != original { panic("changed string") }
            if original.debug() != "\"hello\\n\\t\\\"\\\\\"" { panic("escaping") }
            if ().debug() != "()" { panic("unit") }
            echo ()
        }
    "#,
    );
}

#[test]
fn debug_is_independent_of_show_and_requires_borrowed_string_methods() {
    run_program("struct Point { let x: Int32 } impl Show for Point { fn show() -> String { \"point\" } } fn main() { if Point(x: 1).debug() != \"Point(x: 1)\" { panic(\"default Debug must not use Show\") } }");
    for source in [
        "fn inspect(T: type, value: T) -> T { echo value } fn main() {}",
        "struct Point { let x: Int32 } impl Debug for Point { fn debug(self) -> String { \"point\" } } fn main() {}",
        "struct Point { let x: Int32 } impl Debug for Point { fn debug() -> Int32 { 1 } } fn main() {}",
        "eff Log { fn write() } struct Point { let x: Int32 } impl Debug for Point { fn debug() -> String effects { Log } { Log.write(); \"point\" } } fn main() {}",
        "trait Debug { fn debug(&self) -> Int32 } fn main() {}",
    ] {
        assert!(try_run_program(source).is_err(), "{source}");
    }
}

#[test]
fn debug_rejects_transitive_suspension() {
    let error = try_run_program(
        r#"
        fn background() { branch {} }
        class Value {}
        impl Debug for Value {
            fn debug(&self) -> String { background(); "value" }
        }
        fn main() { echo Value(); () }
    "#,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Debug.debug must be synchronous"),
        "{error}"
    );
}

#[test]
fn echo_supports_dynamic_debug_without_consuming_the_receiver() {
    run_program(
        r#"
        class Value { let text: String }
        impl Debug for Value { fn debug(&self) -> String { self.text.debug() } }
        fn inspect(value: &Dyn(Debug)) { echo value; () }
        fn main() {
            let value = Dyn(Debug)(Value(text: "dynamic"))
            echo value
            inspect(value)
            let result = value |> echo
            if result.debug() != "\"dynamic\"" { panic("dynamic debug") }
        }
    "#,
    );
}

#[test]
fn builtin_trait_dispatch_ignores_inherent_and_custom_names() {
    run_program(include_str!(
        "../../../tests/fixtures/builtin_trait_dispatch.jk"
    ));
}

#[test]
fn string_show_and_interpolation_release_shared_operands() {
    run_program(include_str!(
        "../../../tests/fixtures/string_show_ownership.jk"
    ));
}

#[test]
fn debug_formats_lists_bytes_and_duration() {
    run_program(include_str!("../../../tests/fixtures/debug_values.jk"));
}

#[test]
fn debug_lists_require_debug_on_every_element() {
    for source in [
        "fn render(T: type, value: List(T)) -> String { value.debug() } fn main() {}",
        "fn main() { let values: List(Map(Int32, String)) = List#{}; values.debug() }",
        "fn main() { List#{Map.empty(Int32, String)}.debug() }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("Debug"), "{error}");
    }
}

#[test]
fn debug_list_traversal_is_iterative_and_releases_loop_temporaries() {
    std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
        // Only fixture construction is recursive; Debug must iterate the list.
        run_program(r#"
            fn build(n: Int32) -> List(Unit) {
                if n == 0 { List#{} } else { build(n - 1).push_front(()) }
            }
            fn main() {
                let values = build(2500)
                let expected: UInt64 = 10005
                for again in List#{true, false, true} {
                    if values.debug().length() != expected { panic("long list") }
                    if List#{List#{1}, List#{2}}.debug() != "List#{{List#{{1}}, List#{{2}}}}" { panic("nested loop") }
                }
                ()
            }
        "#);
    }).unwrap().join().unwrap();
}

#[test]
fn debug_recursive_list_members_keep_the_default_debug_depth_limit() {
    run_program(
        r#"
        enum Tree { Leaf(value: Int32); Branch(children: List(Tree)) }
        fn build(n: Int32) -> Tree {
            if n == 0 { Tree.Leaf(value: 1) } else { Tree.Branch(children: List#{build(n - 1)}) }
        }
        fn main() {
            if build(1).debug() != "Tree.Branch(children: List#{{Tree.Leaf(value: 1)}})" { panic("recursive list") }
            if !build(70).debug().contains("Tree(<max-depth>)") { panic("depth limit") }
            if build(0).debug() != "Tree.Leaf(value: 1)" { panic("fresh context") }
        }
    "#,
    );
}
