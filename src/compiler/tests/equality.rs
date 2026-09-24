use super::*;

#[test]
fn partial_eq_builtin_and_custom_dispatch() {
    run_program(include_str!("../../../tests/fixtures/partial_eq.jk"));
}

#[test]
fn partial_eq_integer_widths_and_float32() {
    for ty in [
        "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64",
    ] {
        run_program(&format!(
            r#"
            fn same(T: type + Eq, a: &T, b: &T) -> Bool {{ PartialEq.equals(a, b) && (a == b) }}
            fn main() {{
                let a: {ty} = 7
                let b: {ty} = 7
                if !same(a, b) || (a != b) {{ panic("integer equality") }} else {{}}
            }}
        "#
        ));
    }
    run_program(
        r#"
        fn main() {
            let zero: Float32 = 0.0
            let nan = zero / zero
            if (nan == nan) || !PartialEq.equals(zero, -zero) { panic("IEEE equality") }
        }
    "#,
    );
}

#[test]
fn partial_eq_borrows_classes_and_evaluates_operands_once_in_order() {
    run_program(
        r#"
        class Value { let key: Int32 }
        impl PartialEq for Value {
            fn equals(&self, other: &Self) -> Bool { self.key == other.key }
        }
        impl Eq for Value {}
        class Counter {
            var value: Int32
            fn next(&self, expected: Int32) -> Value {
                if self.value != expected { panic("evaluation order") }
                self.value = self.value + 1
                Value(key: self.value)
            }
        }
        fn main() {
            let left = Value(key: 1)
            let right = Value(key: 1)
            if (left != right) || !PartialEq.equals(left, right) { panic("borrowed equality") }
            if left.key != right.key { panic("operands consumed") }
            let counter = Counter(value: 0)
            if (counter.next(0) == counter.next(1)) || (counter.value != 2) { panic("evaluation count") }
        }
    "#,
    );
}

#[test]
fn partial_eq_rejects_invalid_impls_bounds_and_implicit_hash() {
    for (source, message) in [
        ("fn same(T: type, a: T, b: T) -> Bool { a == b } fn main() {}", "comparison"),
        ("fn eq(T: type + Eq, value: T) {} fn main() { eq(1.0) }", "Eq"),
        ("struct Value { let value: Int32 } impl PartialEq for Value { fn equals(&self, other: Self) -> Bool { true } } fn main() {}", "PartialEq"),
        ("class Value {} impl Eq for Value {} fn main() {}", "PartialEq"),
        ("trait Eq { fn equals(&self, other: Self) -> Bool } fn main() {}", "Eq"),
        ("eff Log { fn write() -> Unit } struct Value {} impl PartialEq for Value { fn equals(&self, other: &Self) -> Bool effects { Log } { Log.write(); true } } fn main() {}", "PartialEq"),
        ("struct Value { let value: Int32 } impl Hash for Value {} impl PartialEq for Value { fn equals(&self, other: &Self) -> Bool { true } } impl Eq for Value {} fn main() { Map.empty(Value, Int32) }", "explicit Hash.hash"),
        ("struct Inner { let value: Int32 } impl PartialEq for Inner { fn equals(&self, other: &Self) -> Bool { true } } impl Eq for Inner {} struct Key { let inner: Inner } impl Hash for Key {} impl Eq for Key {} fn main() { Map.empty(Key, Int32) }", "PartialEq"),
        ("struct Value { let value: Int32 } impl Hash for Value {} impl PartialEq for Value { fn equals(&self, other: &Self) -> Bool { true } } impl Eq for Value {} fn make(K: type + Hash + Eq) -> Map(K, Int32) { Map.empty(K, Int32) } fn main() { make(Value) }", "explicit Hash.hash"),
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains(message), "{source}\n{error}");
    }
}

#[test]
fn partial_eq_rejects_transitive_suspension() {
    let error = try_run_program(
        r#"
        fn background() { branch {} }
        class Value {}
        impl PartialEq for Value {
            fn equals(&self, other: &Self) -> Bool { background(); true }
        }
        fn main() { let a = Value(); let b = Value(); println(a == b) }
    "#,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("PartialEq.equals must be synchronous"),
        "{error}"
    );
}

#[test]
fn partial_eq_structural_keys_work_in_mutable_maps_and_sets() {
    run_program(&format!(
        r#"
        {}
        {}
        struct Inner {{ let text: String }}
        struct Key {{ let inner: Inner; let count: Int32 }}
        impl Eq for Key {{}}
        impl Hash for Key {{}}
        fn main() {{
            let key = Key(inner: Inner(text: "hello"), count: 1)
            let other = Key(inner: Inner(text: "hel" + "lo"), count: 1)
            if key != other {{ panic("structural equality") }} else {{}}
            let map = MutMap(Key, Int32)()
            let _ = map.insert(key, 7)
            if map.get(other).unwrap_or(0) != 7 {{ panic("mutable map lookup") }} else {{}}
            let _ = map.insert(other, 9)
            if map.get(key).unwrap_or(0) != 9 {{ panic("mutable map update") }} else {{}}
            if map.remove(other).unwrap_or(0) != 9 {{ panic("mutable map remove") }} else {{}}
            let set = MutSet(Key)()
            let _ = set.add(key)
            if !set.contains(other) {{ panic("mutable set lookup") }} else {{}}
            let _ = set.remove(other)
            if !set.is_empty() {{ panic("mutable set remove") }} else {{}}
        }}
    "#,
        include_str!("../../../std/joky/mut_map.jk"),
        include_str!("../../../std/joky/mut_set.jk")
    ));
}

#[test]
fn partial_eq_borrowed_value_parameters_work_through_dynamic_wrappers() {
    run_program(
        r#"
        trait Matches { fn matches(&self, other: &String) -> Bool }
        struct Pattern { let text: String }
        impl Matches for Pattern {
            fn matches(&self, other: &String) -> Bool { self.text == other }
        }
        fn main() {
            let matcher = Dyn(Matches)(Pattern(text: "hello"))
            let text = "hel" + "lo"
            if !matcher.matches(text) { panic("dynamic borrowed value") }
            if text != "hello" { panic("borrowed argument changed") }
        }
    "#,
    );
}
