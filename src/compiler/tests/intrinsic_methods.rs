use super::*;

#[test]
fn intrinsic_signatures_produce_distinct_nested_result_types() {
    run_program(
        r#"
        fn lookup(V: type, values: Map(String, V)) -> Option(V) { values.get("key") }
        fn main() {
            let unrelated: Option(Bool) = Some(true)
            let empty = Bytes()
            if !empty.is_empty() { panic("bytes constructor") }
            let data = Bytes.from_string("abc")
            let byte: Option(UInt8) = data.get(0)
            let chunk: Option(Bytes) = data.slice(1, 2)
            let text: Option(String) = chunk!.to_string()
            if text! != "bc" { panic("bytes result") }
            let values = Map#{"key" => Some("value")}
            let nested: Option(Option(String)) = lookup(values)
            if nested!! != "value" { panic("map nested option") }
            let lists = Map#{"key" => List#{1, 2}}
            let list: Option(List(Int32)) = lookup(lists)
            let tail: Option(List(Int32)) = list!.tail()
            if tail!.head()! != 2 { panic("list return") }
            let updated: Map(String, List(Int32)) = lists.insert("key", List#{3})
            if updated.get("key")!.head()! != 3 { panic("map return") }
            let items: Set(Int32) = Set#{1}
            if !items.get(1)! { panic("set alias") }
            if "42".parse(Int32)! != 42 { panic("parse") }
            if "1.5".parse(Float64)! != 1.5 { panic("parse float") }
            if Debug.debug("x") != "\"x\"" { panic("trait dispatch") }
        }
    "#,
    );
}

#[test]
fn intrinsic_declarations_cannot_change_native_signatures() {
    for declaration in [
        "struct Bytes { fn get(&self, index: UInt64) -> Option(String) }",
        "struct Bytes { fn get(&self, index: Int32) -> Option(UInt8) }",
        "struct Bytes { fn slice(&self, index: UInt64) -> Option(Bytes) }",
        "struct String { fn trim(self) -> String }",
        "struct String { fn contains(&self, needle: &String) -> Bool }",
        "struct String { fn length(&self) -> String }",
        "struct String { fn parse(&self) -> Int32 }",
        "struct Map(A: type, B: type) { fn get(&self, key: A) -> Option(A) }",
        "struct Map(A: type, B: type) { fn insert(&self, key: A, item: B) -> Map(B, A) }",
        "struct List(T: type) { fn head(&self) -> T }",
        "class MutList(T: type) { fn pop(&self) -> String }",
        "class MutList(T: type) { fn to_list(&self) -> List(Int32) }",
        "class MutList(T: type) { fn retain(&self, keep: fn(T, T) -> Bool) -> Unit }",
        "class MutMap(K: type, V: type) { fn insert(&self, key: K, value: V) -> Unit }",
        "class MutMap(K: type, V: type) { fn to_list(&self) -> List(K) }",
        "class MutMap(K: type, V: type) { fn retain(&self, keep: fn(K) -> Bool) -> Unit }",
    ] {
        let error = try_run_program(&format!("@intrinsic {declaration} fn main() {{}}"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("must match its builtin signature"),
            "{declaration}\n{error}"
        );
    }
    let error =
        try_run_program("@intrinsic struct Bytes { fn imaginary(&self) -> Unit } fn main() {}")
            .unwrap_err()
            .to_string();
    assert!(error.contains("unknown intrinsic method"), "{error}");
}

#[test]
fn intrinsic_value_method_subsets_and_arguments_are_checked() {
    for source in [
        "@intrinsic struct Bytes { fn length(&self) -> UInt64 } fn main() { Bytes().is_empty() }",
        "@intrinsic struct String { fn trim(&self) -> String } fn main() { \"a\".contains(\"a\") }",
        "@intrinsic struct String { fn trim(&self) -> String } fn main() { \"42\".parse(Int32) }",
        "@intrinsic struct Map(K: type, V: type) { fn get(&self, key: K) -> Option(V) } fn main() { Map#{1 => 2}.length() }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("unknown function"), "{error}");
    }
    for source in [
        "fn main() { Bytes().get() }",
        "fn main() { \"a\".contains() }",
        "fn main() { Map#{1 => 2}.insert(1) }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("expects exactly"), "{error}");
    }
    for source in [
        "fn main() { Bytes().get(\"index\") }",
        "fn main() { \"a\".contains(1) }",
        "fn main() { Map#{1 => 2}.get(\"key\") }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(
            error.contains("expected") && error.contains("found"),
            "{error}"
        );
    }
    run_program("@intrinsic struct String { fn parse(&self, Target: type + FromString) -> Result(Target, String) } fn main() { let value = \"42\".parse(Int32)!; if value != 42 { panic(\"parse\") } }");
}

#[test]
fn intrinsic_map_where_checks_renamed_parameters_and_generic_bounds() {
    let declaration = "@intrinsic struct Map(Key: type, Item: type) { fn get(&self, key: Key) -> Option(Item) where Item: Ord }";
    run_program(&format!(
        r#"{declaration}
        fn lookup(T: type + Ord, xs: Map(String, T)) -> Option(T) {{ xs.get("key") }}
        fn main() {{
            let floats = Map#{{"key" => 1.5}}
            if lookup(Map#{{"key" => 42}})! != 42 {{ panic("map get") }} else {{}}
        }}"#
    ));
    for body in [
        "fn main() { Map#{\"key\" => 1.5}.get(\"key\") }",
        "fn lookup(T: type + PartialOrd, xs: Map(String, T)) -> Option(T) { xs.get(\"key\") } fn main() {}",
    ] {
        let error = try_run_program(&format!("{declaration} {body}")).unwrap_err().to_string();
        assert!(error.contains("Ord"), "{error}");
    }
}
