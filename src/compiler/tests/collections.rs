use super::*;

#[test]
fn hash_dispatch_and_custom_collection_keys() {
    run_program(&format!(
        "{}\n{}\n{}",
        include_str!("../../../std/joky/mut_map.jk"),
        include_str!("../../../std/joky/mut_set.jk"),
        include_str!("../../../tests/fixtures/hash.jk")
    ));
}

#[test]
fn hash_rejects_invalid_signatures_effects_and_key_layouts() {
    for (source, message) in [
        ("struct Key { let id: Int32 } impl Hash for Key { fn hash(&self, state: Hasher) -> Unit {} } fn main() {}", "Hash"),
        ("struct Key { let id: Int32 } impl Hash for Key { fn hash(&self, state: &Hasher) -> UInt64 { 0 } } fn main() {}", "Hash"),
        ("eff Log { fn write() -> Unit } struct Key { let id: Int32 } impl Hash for Key { fn hash(&self, state: &Hasher) -> Unit effects { Log } { Log.write() } } fn main() {}", "Hash"),
        ("fn background() { branch {} } struct Key { let id: Int32 } impl Hash for Key { fn hash(&self, state: &Hasher) -> Unit { background() } } fn main() {}", "Hash.hash must be synchronous"),
        ("class Key { let id: Int32 } impl PartialEq for Key { fn equals(&self, other: &Self) -> Bool { self.id == other.id } } impl Eq for Key {} impl Hash for Key { fn hash(&self, state: &Hasher) -> Unit { Hash.hash(self.id, state) } } fn main() { Map.empty(Key, Int32) }", "immutable key"),
        ("fn main() { let h = Hasher(); Hash.hash(1.0, h) }", "Hash"),
        ("fn main() { Map#{(1, 1.0) => true} }", "Hash"),
        ("fn main() { Map#{(1, Some(2)) => true} }", "Hash"),
        ("fn main() { Map#{(1, List#{2}) => true} }", "Hash"),
        ("enum Key { Value(value: Int32) } fn main() { Map.empty(Key, Int32) }", "Hash"),
        ("fn make(K: type, key: (K, Int32)) -> Map((K, Int32), Bool) { Map#{key => true} } fn main() { make((1.0, 2)) }", "Hash"),
        ("class Key { let id: Int32 } impl PartialEq for Key { fn equals(&self, other: &Self) -> Bool { true } } impl Eq for Key {} impl Hash for Key { fn hash(&self, state: &Hasher) -> Unit {} } fn make(K: type, key: (K, Int32)) -> Map((K, Int32), Bool) { Map#{key => true} } fn main() { make((Key(id: 1), 2)) }", "__MapKey"),
    ] {
        let error = try_run_program(source).expect_err(source).to_string();
        assert!(error.contains(message), "{source}\n{error}");
    }
}

#[test]
fn compiles_persistent_scalar_lists() {
    let source = r#"
            fn main() {
                let values = List(1, 2, 3)
                let extended = values.push_front(0)
                let first = extended.head().unwrap_or(99)
                let rest = extended.tail()
                let empty: List(Int32) = List#{}
                let missing = empty.head().is_none()
                let empty_check = empty.is_empty()
                let values_length = values.length()
                let empty_length = empty.length()
                let reversed = values.reverse()
                let reversed_first = reversed.head().unwrap_or(99)
            }
        "#;
    run_program(source);
}

#[test]
fn runs_enum_rest_patterns_and_renamed_fields() {
    run_program(
        r#"
        enum State {
            Sasl(first: Int32, password: String, required: Bool)
            Done
        }

        class Box { let value: Int32 }
        enum Mixed { Value(text: String, box: Box); Done }

        fn required(state: State) -> Bool {
            match state {
                State.Sasl(required: is_required, ..) => is_required
                State.Done => false
            }
        }

        fn main() {
            if !required(state: State.Sasl(first: 1, password: "pw", required: true)) {
                panic("rest pattern lost the renamed field")
            }
            let password = match State.Sasl(first: 1, password: "pw", required: true) {
                State.Sasl(password: text, ..) => text
                State.Done => "missing"
            }
            if password != "pw" {
                panic("rest pattern lost the shared payload")
            }
            let value: Option(Int32) = Some(1)
            match value {
                Some(..) => ()
                None => panic("option rest pattern matched the wrong variant")
            }
            match Mixed.Value(text: "mixed", box: Box(value: 1)) {
                Mixed.Value(text: _, ..) => ()
                Mixed.Done => ()
            }
        }
        "#,
    );
}

#[test]
fn compiles_tagged_collection_literals() {
    let source = r#"
            fn main() {
                let values = List#{1, 2, 3}
                let tags = Set#{"compiler", "language"}
                let users = Map#{"alice" => 1, "bob" => 2}
                println(values.length())
                println(tags.contains_key("compiler"))
                println(users.get("bob").unwrap_or(0))
                let empty_list: List(Int32) = List#{}
                let empty_set: Set(String) = Set#{}
                let empty_map: Map(String, Int32) = Map#{}
                println(empty_list.is_empty())
                println(empty_set.is_empty())
                println(empty_map.is_empty())
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_generated_struct_type_functions() {
    run_program(
        r#"
        fn Box(T: type) -> type {
            struct { let value: T }
        }

        fn main() {
            let value: Box(Int64) = Box(Int64)(value: 42)
            println(value.value)
        }
        "#,
    );
}

#[test]
fn compiles_generated_struct_field_defaults() {
    run_program(
        r#"
        fn Box(T: type) -> type {
            struct {
                let value: T = 7
            }
        }

        fn main() {
            let value: Box(Int64) = Box(Int64)()
            println(value.value)
        }
        "#,
    );
}

#[test]
fn compiles_nested_generated_struct_defaults() {
    run_program(
        r#"
        fn Box(T: type) -> type {
            struct { let value: T = 7 }
        }

        fn Pair(T: type) -> type {
            struct {
                let left: Box(T) = Box(T)(value: 11)
                let right: Box(T) = Box(T)()
            }
        }

        fn main() {
            let pair: Pair(Int64) = Pair(Int64)()
            println(pair.left.value)
            println(pair.right.value)
        }
        "#,
    );
}

#[test]
fn compiles_generated_enum_variant_constructors() {
    run_program(
        r#"
        fn Maybe(T: type) -> type {
            enum {
                Some(value: T)
                None
            }
        }

        fn consume(value: Maybe(Int64)) {
            let moved = value
        }

        fn main() {
            consume(Maybe(Int64).Some(value: 42))
            let empty: Maybe(Int64) = Maybe(Int64).None
            consume(empty)
        }
        "#,
    );
}

#[test]
fn matches_generated_enum_variants_through_type_bindings() {
    run_program(
        r#"
        fn Maybe(T: type) -> type {
            enum {
                Some(value: T)
                None
            }
        }

        fn main() {
            let M = Maybe(Int64)
            let value: M = M.Some(value: 42)
            let result = match value {
                M.Some(value: number) => number
                M.None => 0
            }
            println(result)
        }
        "#,
    );
}

#[test]
fn compiles_builtin_option_and_result_with_first_class_type_syntax() {
    run_program(
        r#"
        fn main() {
            let option: Option(Int64) = Option(Int64).Some(value: 42)
            let missing: Option(Int64) = Option(Int64).None
            let result: Result(Int64, String) =
                Result(Int64, String).Ok(value: option.unwrap_or(0))
            let failure: Result(Int64, String) =
                Result(Int64, String).Err(value: "bad")
            println(result.unwrap_or(0))
            println(failure.unwrap_or(7))
            println(missing.is_none())
        }
        "#,
    );
}

#[test]
fn matches_builtin_option_and_result_through_type_bindings() {
    run_program(
        r#"
        fn main() {
            let O = Option(Int64)
            let R = Result(Int64, String)
            let option: O = O.Some(value: 42)
            let result: R = R.Ok(value: 7)
            let option_value = match option {
                O.Some(value: number) => number
                O.None => 0
            }
            let result_value = match result {
                R.Ok(value: number) => number
                R.Err(value: _) => 0
            }
            println(option_value + result_value)
        }
        "#,
    );
}

#[test]
fn compiles_and_releases_lists_of_strings() {
    let source = r#"
            fn main() {
                let values = List("first", "second")
                let prefix = "zero"
                let extended = values.push_front(prefix)
                let first = extended.head().unwrap_or("missing")
                let tail = extended.tail().unwrap_or(List#{})
                let second = tail.head().unwrap_or("missing")
                let reversed = values.reverse()
                let last = reversed.head().unwrap_or("missing")
            }
        "#;
    run_program(source);
}
#[test]
fn compiles_lists_of_immutable_composite_values() {
    let cases = [
        (
            "tuple",
            r#"fn main() {
                    let values = List((1, "one"), (2, "two")).reverse()
                    let value = values.head().unwrap_or((0, "missing"))
                    let number = value.0
                    let text = value.1
                }"#,
        ),
        (
            "struct",
            r#"struct Entry { let name: String; let value: Int32 }
                fn main() {
                    let values = List(Entry(name: "first", value: 1), Entry(name: "second", value: 2)).reverse()
                    let value = values.head().unwrap_or(Entry(name: "missing", value: 0))
                    let name = value.name
                    let number = value.value
                }"#,
        ),
        (
            "enum",
            r#"enum Item { Text(value: String); Number(value: Int32) }
                fn main() {
                    let values = List(Item.Text(value: "hello"), Item.Number(value: 2)).reverse()
                    let value = values.head().unwrap_or(Item.Number(value: 0))
                    let text = match value {
                        Item.Text(value: text) => text
                        Item.Number(value: _) => "missing"
                    }
                }"#,
        ),
        (
            "option",
            r#"fn main() {
                    let values: List(Option(String)) = List(Some("value"), None).reverse()
                    let value = values.head().unwrap_or(None).unwrap_or("missing")
                }"#,
        ),
        (
            "result",
            r#"fn ok_result() -> Result(Int32, String) { Ok(7) }
                fn err_result() -> Result(Int32, String) { Err("bad") }
                fn main() {
                    let values = List(ok_result(), err_result()).reverse()
                    let value = values.head().unwrap_or(ok_result()).unwrap_or(0)
                }"#,
        ),
        (
            "nested list",
            r#"fn main() {
                    let values = List(List(1, 2), List(3, 4)).reverse()
                    let inner: List(Int32) = values.head().unwrap_or(List#{})
                    let first = inner.head().unwrap_or(0)
                    let tail = inner.tail().unwrap_or(List#{})
                    let second = tail.head().unwrap_or(0)
                }"#,
        ),
        (
            "generic struct",
            r#"struct Entry { let name: String; let value: Int32 }
                fn first(T: type, values: List(T)) -> Option(T) { values.head() }
                fn main() {
                    let value = first(List(Entry(name: "item", value: 1))).unwrap_or(Entry(name: "missing", value: 0))
                    let name = value.name
                }"#,
        ),
    ];

    for (_name, source) in cases {
        run_program(source);
    }
}

#[test]
fn monomorphizes_generic_functions_for_concrete_calls() {
    let source = r#"
            fn identity(T: type, value: T) -> T {
                value
            }

            fn list_length(T: type, value: List(T)) -> UInt64 {
                value.length()
            }

            fn main() {
                let integer = identity(42)
                let boolean = identity(true)
                let values = List(1, 2, 3)
                let length = list_length(value: values)
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_unified_type_value_generic_calls() {
    let source = r#"
            fn identity(T: type, value: T) -> T {
                value
            }

            fn main() {
                let integer = identity(Int64, 42)
                let text = identity(String, "ok")
                println(text)
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_bound_and_constructed_type_values() {
    let source = r#"
            fn identity(T: type, value: T) -> T {
                value
            }

            fn main() {
                let Number = Int64
                let Values = List(Number)
                let value = identity(Number, 42)
                let values = identity(Values, List(1, 2))
                println(value)
                println(values.length())
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_user_defined_type_functions() {
    let source = r#"
            fn BoxOf(T: type) -> type { List(T) }

            fn identity(T: type, value: T) -> T { value }

            fn main() {
                let BoxI32 = BoxOf(Int64)
                let values = identity(BoxI32, List(1, 2))
                println(values.length())
            }
        "#;
    run_program(source);
}

#[test]
fn compiles_composed_type_functions_in_generics_and_closures() {
    run_program(
        r#"
        fn Wrapped(T: type) -> type {
            let Payload: type = List(T)
            Option(Payload)
        }
        fn Number() -> type { Int64 }
        fn identity(T: type, value: T) -> T { value }
        fn forward(T: type, value: T) -> Option(List(T)) {
            let R = Wrapped(T)
            identity(R, Some(List(value)))
        }
        fn main() {
            let N: type = Number()
            let number: N = 42
            let f = fn () -> Int64 { identity(N, 42) }
            if f() != number { panic("type alias leaked into closure") }
            let result = forward(N, number)!.head()!
            if result != number { panic("incorrect specialization") }
            let other = forward(String, "ok")!.head()!
            if other != "ok" { panic("incorrect string specialization") }
        }
    "#,
    );
}

#[test]
fn compiles_type_function_annotations_with_associated_types() {
    run_program(
        r#"
        trait Source { type Item; fn next(&self) -> Option(Item) }
        struct Items { let value: Int64 }
        impl Source for Items {
            type Item = Int64
            fn next() -> Option(Self.Item) { Some(self.value) }
        }
        fn Maybe(T: type) -> type { Option(T) }
        fn next(I: type + Source, iter: I) -> Maybe(I.Item) { iter.next() }
        fn main() { println(next(Items(value: 42))!) }
    "#,
    );
}

#[test]
fn infers_generic_types_from_later_arguments_and_return_context() {
    let source = r#"
            fn choose(T: type, left: T, right: T) -> T {
                right
            }

            fn identity(T: type, value: T) -> T {
                value
            }

            fn main() {
                let option = choose(None, Some(1))
                let number: Int64 = identity(1)
                let checked = option.unwrap_or(0)
            }
        "#;
    run_program(source);
}

#[test]
fn monomorphizes_transitive_generic_calls() {
    let source = r#"
            fn identity(T: type, value: T) -> T {
                value
            }

            fn forward(T: type, value: T) -> T {
                identity(value)
            }

            fn forward_list(T: type, value: List(T)) -> List(T) {
                identity(value)
            }

            fn main() {
                let value = forward(42)
                let values = forward_list(List(1, 2, 3))
                let first = values.head().unwrap_or(0)
            }
        "#;
    run_program(source);
}

#[test]
fn monomorphizes_functions_with_inferred_type_parameters() {
    let source = r#"
            fn length(value: Map(K, V)) -> UInt64 {
                value.length()
            }

            fn main() {
                let values: Map(String, Int32) = Map#{}
                let count = length(values)
                println(count)
            }
        "#;
    run_program(source);
}

#[test]
fn runs_maps_with_immutable_struct_keys() {
    let source = r#"
            struct Point {
                let x: Int32
                let y: Int32
            }

            impl Hash for Point {}

            impl Eq for Point {}

            fn main() {
                let values: Map(Point, String) = Map#{}
                let values = values.insert(Point(x: 1, y: 2), "point")
                println(values.get(Point(x: 1, y: 2)).unwrap_or("missing"))
            }
        "#;
    run_program(source);
}
#[test]
fn runs_set_using_the_persistent_map_representation() {
    let source = r#"
            type Set(T: type + Hash + Eq)

            fn empty(T: type + Hash + Eq) -> Set(T) {
                Set#{}
            }

            fn main() {
                let values: Set(Int32) = empty(Int32)
                let updated = values.insert(1, true)
                println(if values.is_empty() { "set ok" } else { "set bad" })
                println(if updated.contains_key(1) { "set ok" } else { "set bad" })
                let removed = updated.remove(1)
                println(if removed.is_empty() { "set ok" } else { "set bad" })
            }
        "#;
    run_program(source);
}
#[test]
fn resolves_builtin_type_values_in_generic_instances() {
    let source = r#"
            fn empty(T: type) -> List(T) {
                List#{}
            }

            fn main() {
                let values = empty(Int32)
                let missing = values.head().is_none()
            }
        "#;
    run_program(source);
}

#[test]
fn generic_instances_distinguish_named_types() {
    let source = r#"
            struct Point { let value: Int32 }
            struct Size { let value: Int32 }

            fn identity(T: type, value: T) -> T {
                value
            }

            fn main() {
                let point = identity(Point(value: 1))
                let size = identity(Size(value: 2))
            }
        "#;
    run_program(source);
}

#[test]
fn parses_strings_with_type_value_arguments() {
    let source = "
            fn main() {
                let integer = \"42\".parse(Int32)
                let decimal = \"1.5\".parse(Float64)
                let invalid = \"bad\".parse(Int32)
                let answer = match integer { Ok(value) => value; Err(_) => 0 }
                let fallback = invalid.unwrap_or(0)
                println(\"parse ok\")
            }
        ";
    run_program(source);
}

#[test]
fn inlines_typed_module_constants() {
    let source = r#"
            const BASE = 40
            const ANSWER = BASE + 2
            const LABEL = "answer"

            fn main() {
                println(LABEL)
                println(ANSWER)
            }
        "#;
    run_program(source);
}

#[test]
fn shows_builtin_values_and_monomorphized_generics() {
    let source = r#"
            trait Show {
                fn show(&self) -> String
            }

            fn forward(T: type + Show, value: T) {
                println(value)
            }

            fn log(T: type + Show, value: T) {
                forward(value)
            }

            fn main() {
                let text = 42.show();
                println(text);
                println(1.5);
                println(true);
                log(99);
                log("generic")
            }
        "#;
    run_program(source);
}

#[test]
fn shows_user_defined_impls() {
    let source = r#"
            struct Point {
                let x: Int32
            }

            class Counter {
                let value: Int32
            }

            impl Show for Point {
                fn show() -> String {
                    if self.x == 1 { "Point" } else { "Other" }
                }
            }

            impl Show for Counter {
                fn show() -> String {
                    if self.value == 1 { "Counter" } else { "Other" }
                }
            }

            fn log(T: type + Show, value: T) {
                println(value)
            }

            fn main() {
                let point = Point(x: 1)
                println(point)
                let text = point.show()
                println(text)
                log(point)
                let counter = Counter(value: 1)
                println(counter)
            }
        "#;
    run_program(source);
}

#[test]
fn runs_custom_trait_impls_through_generic_functions() {
    let source = r#"
            trait Inspect {
                fn debug(&self) -> String
            }

            struct Point { let x: Int32 }
            class Counter { let value: Int32 }

            impl Inspect for Point {
                fn debug() -> String { "point" }
            }

            impl Inspect for Counter {
                fn debug() -> String { "counter" }
            }

            fn dump(T: type + Inspect, value: T) -> String {
                value.debug()
            }

            fn main() {
                println(dump(Point(x: 1)))
                println(dump(Counter(value: 1)))
            }
        "#;
    run_program(source);
}

#[test]
fn runs_trait_methods_with_arguments_and_multiple_methods() {
    let source = r#"
            trait Compare {
                fn equals(&self, other: Self) -> Bool
            }

            trait Inspect {
                fn debug(&self) -> String
                fn type_name(&self) -> String
            }

            struct Point { let x: Int32 }

            impl Compare for Point {
                fn equals(other: Point) -> Bool { self.x == other.x }
            }

            impl Inspect for Point {
                fn debug() -> String { "point" }
                fn type_name() -> String { "Point" }
            }

            fn same(T: type + Compare, left: T, right: T) -> Bool {
                left.equals(right)
            }

            fn main() {
                let point = Point(x: 1)
                println(same(point, Point(x: 1)))
                println(Point(x: 1).debug())
                println(Point(x: 1).type_name())
            }
        "#;
    run_program(source);
}

#[test]
fn runs_generic_functions_with_associated_types() {
    let source = r#"
            trait Iterator {
                type Item
                fn next(&self) -> Option(Item)
            }

            struct Values { let value: Int32 }

            impl Iterator for Values {
                type Item = Int32
                fn next() -> Option(Int32) { Some(self.value) }
            }

            fn first(I: type + Iterator, iter: I) -> Option(I.Item) {
                iter.next()
            }

            fn main() {
                println(first(Values(value: 7))!)
            }
        "#;
    run_program(source);
}
