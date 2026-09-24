use super::*;

#[test]
fn intrinsic_where_constraints_apply_only_to_the_declared_method() {
    run_program(
        r#"
        fn main() {
            let xs = MutList#{1.0}
            xs.push(2.0)
            if xs.get(1)! != 2.0 { panic("float list") }
            if List#{1.0}.head()! != 1.0 { panic("float list") }
        }
    "#,
    );
    let declaration = "@intrinsic class MutList(Item: type) { fn push(&self, item: Item) -> Unit where Item: PartialEq + Debug }";
    run_program(&format!("{declaration} fn add(T: type + Ord + Debug, xs: &MutList(T), x: T) {{ xs.push(x) }} fn main() {{ let xs = MutList#{{1}}; add(xs, 2) }}"));
    let declaration = "@intrinsic class MutList(Item: type) { fn push(&self, item: Item) -> Unit where Item: Ord }";
    run_program(&format!(
        "{declaration} fn main() {{ let xs = MutList#{{1.0}} }}"
    ));
    let error = try_run_program(&format!(
        "{declaration} fn main() {{ let xs = MutList#{{1.0}}; xs.push(2.0) }}"
    ))
    .unwrap_err()
    .to_string();
    assert!(error.contains("Ord"), "{error}");
}

#[test]
fn intrinsic_where_constraints_check_multiple_parameters_and_custom_traits() {
    let declaration = "@intrinsic class MutMap(K: type, V: type) { fn insert(&self, key: K, value: V) -> Option(V) where K: Ord + Hash, V: Ord }";
    run_program(&format!(
        "{declaration} fn main() {{ let xs = MutMap#{{1 => 2}}; let _ = xs.insert(3, 4); () }}"
    ));
    let error = try_run_program(&format!(
        "{declaration} fn main() {{ let xs = MutMap#{{1 => 2.0}}; xs.insert(3, 4.0) }}"
    ))
    .unwrap_err()
    .to_string();
    assert!(error.contains("Ord"), "{error}");
    let declaration = "trait Ready { fn ready(&self) -> Bool } struct Item { let key: Int32 } @intrinsic class MutList(T: type) { fn push(&self, item: T) -> Unit where T: Ready }";
    let body = "fn append(T: type + Ready, xs: &MutList(T), x: T) { xs.push(x) } fn main() { let xs = MutList#{Item(key: 1)}; append(xs, Item(key: 2)) }";
    run_program(&format!(
        "{declaration} impl Ready for Item {{ fn ready(&self) -> Bool {{ true }} }} {body}"
    ));
    let error = try_run_program(&format!("{declaration} {body}"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("Ready"), "{error}");
}

#[test]
fn intrinsic_where_declarations_are_validated_even_when_unused() {
    for (declaration, expected) in [
        (
            "class MutList(T: type) { fn push(&self, x: T) -> Unit where U: Ord }",
            "unknown type parameter",
        ),
        (
            "class MutList(T: type) { fn push(&self, x: T) -> Unit where T: Missing }",
            "Missing",
        ),
        (
            "class MutList(T: type) { fn push(&self, x: T) -> Unit where T: Ord, T: Debug }",
            "duplicate where predicate",
        ),
        (
            "class MutList(T: type) { fn sort(&self) -> Unit }",
            "requires where T: Ord",
        ),
        (
            "struct List(Item: type) { fn sorted(&self) -> List(Item) where Item: PartialOrd }",
            "requires where Item: Ord",
        ),
        ("class MutList(T: type, U: type) {}", "type parameter count"),
    ] {
        let error = try_run_program(&format!("@intrinsic {declaration} fn main() {{}}"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{declaration}\n{error}");
    }
    run_program("@intrinsic class MutList(Item: type) { fn sort(&self) -> Unit where Item: Ord } fn main() { let xs = MutList#{2, 1}; xs.sort() }");
}

#[test]
fn sorting_builtin_custom_stable_and_borrowed() {
    run_program(include_str!("../../../tests/fixtures/sorting.jk"));
}

#[test]
fn sorting_state_replacement_releases_shared_class_fields() {
    run_program(
        r#"
        class State {
            var values: List(String)
            var text: String
            fn replace(&self) {
                self.values = self.values.tail()!
                self.text = self.text + "!"
            }
        }
        fn main() {
            let state = State(values: List#{"first", "second", "third"}, text: "hello")
            state.replace()
            state.replace()
            if state.text != "hello!!" { panic("field replacement") }
        }
    "#,
    );
}

#[test]
fn sorting_integer_widths_and_boundaries() {
    for (ty, low, high) in [
        ("Int8", "-128", "127"),
        ("Int16", "-32768", "32767"),
        ("Int32", "-2147483648", "2147483647"),
        ("Int64", "-9223372036854775808", "9223372036854775807"),
        ("UInt8", "0", "255"),
        ("UInt16", "0", "65535"),
        ("UInt32", "0", "4294967295"),
        ("UInt64", "0", "18446744073709551615"),
    ] {
        run_program(&format!(
            r#"
            fn main() {{
                let low: {ty} = {low}
                let high: {ty} = {high}
                let one: {ty} = 1
                let input = List#{{high, low, one, high, low}}
                if input.sorted() != List#{{low, low, one, high, high}} {{ panic("integer sorted") }} else {{}}
                let mutable = MutList#{{high, low, one, high, low}}
                mutable.sort()
                if (mutable.get(0)! != low) || (mutable.get(2)! != one) || (mutable.get(4)! != high) {{ panic("integer sort") }} else {{}}
            }}
        "#
        ));
    }
}

#[test]
fn sorting_matches_reference_across_merge_boundaries() {
    let mut source =
        String::from("fn expect(b: Bool) { if !b { panic(\"merge boundaries\") } } fn main() {\n");
    for (case, len) in [0, 1, 2, 3, 7, 8, 9, 31, 32, 33, 257, 1025]
        .into_iter()
        .enumerate()
    {
        let input = (0..len)
            .map(|i| ((i * 73 + 11) % 41) - 20)
            .collect::<Vec<i32>>();
        let mut expected = input.clone();
        expected.sort();
        let format = |values: &[i32]| {
            values
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        source.push_str(&format!(
            "let xs{case}: List(Int32) = List#{{{}}}\nexpect(xs{case}.sorted() == List#{{{}}})\n",
            format(&input),
            format(&expected)
        ));
    }
    source.push('}');
    run_program(&source);
}

#[test]
fn sorting_requires_ord_even_for_empty_lists_and_generic_bodies() {
    for source in [
        "fn main() { List#{1.0, 2.0}.sorted() }",
        "fn main() { List.empty(Float64).sorted() }",
        "fn main() { let xs = MutList#{1.0}; xs.sort() }",
        "fn f(T: type, xs: List(T)) -> List(T) { xs.sorted() } fn main() {}",
        "fn f(T: type + PartialOrd, xs: &MutList(T)) { xs.sort() } fn main() {}",
        "fn main() { List#{List#{1.0}}.sorted() }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("Ord"), "{source}\n{error}");
    }
    for source in [
        "fn main() { List#{1}.sorted(1) }",
        "fn main() { let xs = MutList#{1}; xs.sort(1) }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("expects exactly 0"), "{error}");
    }
    let source = format!(
        "{} fn main() {{ let xs = MutList#{{1.0}}; xs.sort() }}",
        include_str!("../../../std/joky/mut_list.jk")
    );
    assert!(try_run_program(&source)
        .unwrap_err()
        .to_string()
        .contains("Ord"));
}

#[test]
fn sorting_uses_ord_and_evaluates_receiver_once() {
    run_program(
        r#"
        struct Desc { let key: Int32 }
        impl PartialEq for Desc { fn equals(&self, other: &Self) -> Bool { self.key == other.key } }
        impl Eq for Desc {}
        impl PartialOrd for Desc {
            fn partial_compare(&self, other: &Self) -> Option(Ordering) { PartialOrd.partial_compare(other.key, self.key) }
        }
        impl Ord for Desc { fn compare(&self, other: &Self) -> Ordering { Ord.compare(other.key, self.key) } }
        class Counter {
            var count: Int32
            fn values(&self) -> List(Desc) {
                self.count = self.count + 1
                List#{Desc(key: 1), Desc(key: 3), Desc(key: 2)}
            }
            fn mutable(&self) -> MutList(Int32) {
                self.count = self.count + 1
                MutList#{3, 2, 1}
            }
        }
        fn main() {
            let c = Counter(count: 0)
            let values = c.values().sorted()
            if (values.head()!.key != 3) || (c.count != 1) { panic("sort receiver") }
            c.mutable().sort()
            if c.count != 2 { panic("sort receiver twice") }
        }
    "#,
    );
}
