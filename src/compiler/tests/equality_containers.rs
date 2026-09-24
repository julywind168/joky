use super::*;

#[test]
fn container_equality_compares_tuples_and_active_variants() {
    run_program(
        r#"
        fn same(T: type + PartialEq, a: &T, b: &T) -> Bool { PartialEq.equals(a, b) }
        fn equivalent(T: type + Eq, a: &T, b: &T) -> Bool { a == b }
        fn expect(value: Bool) { if !value { panic("container equality") } }
        fn main() {
            let none: Option(String) = None
            let some = Some("hello")
            expect(some == Some("hel" + "lo"))
            expect(some != none)
            expect(None != Some(1))
            expect(Some(1) != None)
            expect(Some(None) != Some(Some(1)))
            expect(List#{} != List#{Some(1)})
            expect((None, 1) != (Some(1), 1))
            expect(equivalent(none, none))
            expect(same((1, some), (1, Some("hello"))))
            expect((1, "a") != (2, "a"))
            expect((1, "a") != (1, "b"))
            expect(equivalent(((), true), ((), true)))
            let ok: Result(String, Int32) = Ok("ok")
            let err: Result(String, Int32) = Err(7)
            let other: Result(String, Int32) = Err(8)
            expect(ok == Ok("o" + "k"))
            expect(err == Err(7))
            expect(err != other)
            expect(ok != err)
            expect(equivalent((some, ok), (Some("hello"), Ok("ok"))))
            expect(some == Some("hello"))
        }
    "#,
    );
}

#[test]
fn container_equality_compares_lists_iteratively() {
    run_program(
        r#"
        fn eq(T: type + Eq, a: &T, b: &T) -> Bool { a == b }
        fn expect(value: Bool) { if !value { panic("list equality") } }
        fn main() {
            let a = List#{"one", "two", "three"}
            let b = List#{"o" + "ne", "two", "three"}
            let empty: List(String) = List#{}
            expect(a == b)
            expect(eq(a, b))
            expect(a != List#{"one", "two"})
            expect(List#{"one", "two"} != a)
            expect(a != List#{"one", "two", "different"})
            expect(empty == empty)
            expect(a != empty)
            expect(empty != a)
            expect(a.head().unwrap_or("") == "one")
            expect(List#{Some((1, "hello")), None} == List#{Some((1, "hel" + "lo")), None})
            expect(List#{List#{1, 2}, List#{3}} == List#{List#{1, 2}, List#{3}})
        }
    "#,
    );
}

#[test]
fn container_equality_preserves_nan_even_for_identical_lists() {
    run_program(
        r#"
        fn expect(value: Bool) { if !value { panic("NaN equality") } }
        fn main() {
            let nan = 0.0 / 0.0
            let values = List#{nan}
            expect(values != values)
            expect(Some(nan) != Some(nan))
            expect((nan,) != (nan,))
            let result: Result(Float64, String) = Ok(nan)
            expect(result != result)
            expect(List#{0.0} == List#{-0.0})
        }
    "#,
    );
}

#[test]
fn container_equality_dispatches_custom_members_and_short_circuits() {
    run_program(include_str!(
        "../../../tests/fixtures/partial_eq_containers.jk"
    ));
}

#[test]
fn container_equality_requires_recursive_bounds() {
    for value in ["Some(0.0)", "(1, 0.0)", "List#{0.0}"] {
        let source = format!("fn eq(T: type + Eq, value: &T) {{}} fn main() {{ eq({value}) }}");
        let error = try_run_program(&source).unwrap_err().to_string();
        assert!(error.contains("Eq"), "{error}");
    }
    for source in [
        "fn compare(T: type, a: &Option(T), b: &Option(T)) -> Bool { a == b } fn main() {}",
        "fn eq(T: type + Eq, value: &T) {} fn main() { let value: Result(Int32, Float64) = Ok(1); eq(value) }",
        "struct Missing {} fn main() { let a: Option(Missing) = None; a == a }",
        "fn main() { let a = Map.empty(Int32, String); a == a }",
        "fn main() { Map.empty(Option(Int32), String) }",
        "fn main() { Map.empty(List(Int32), String) }",
        "fn main() { Set.empty(Option(Int32)) }",
        "fn main() { MutMap.empty(List(Int32), String) }",
        "fn main() { MutSet.empty(Option(Int32)) }",
    ] {
        assert!(try_run_program(source).is_err(), "{source}");
    }
}

#[test]
fn container_equality_handles_long_lists_without_comparison_recursion() {
    // Recursive fixture construction needs more stack than a test worker provides.
    // Equality itself traverses the two lists with MIR loop cursors.
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            run_program(
                r#"
        fn build(count: Int32) -> List(Int32) {
            if count == 0 { List#{} } else { build(count - 1).push_front(count) }
        }
        fn main() {
            let a = build(4000)
            let b = build(4000)
            if a != b { panic("long list equality") }
        }
    "#,
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn container_equality_evaluates_operands_once_in_order_and_cleans_up_in_loops() {
    run_program(
        r#"
        class Counter {
            var count: Int32
            fn next(&self) -> Int32 { self.count = self.count + 1; self.count }
            fn result(&self) -> Result(Int32, String) {
                if self.count != 1 { panic("operand evaluation order") }
                Ok(self.next())
            }
            fn verify(&self) {
                loop {
                    if self.count == 20 { break }
                    let a = List#{Some("hello"), None}
                    let b = List#{Some("hel" + "lo"), None}
                    if a != b { panic("loop comparison") }
                    self.count = self.count + 1
                }
            }
        }
        fn same(T: type + Eq + PartialEq, a: &T, b: &T) -> Bool { a.equals(b) }
        fn expect(value: Bool) { if !value { panic("comparison evaluation") } }
        fn main() {
            let counter = Counter(count: 0)
            expect(Ok(counter.next()) != counter.result())
            expect(counter.next() == 3)
            expect((counter.next(), 1) == (4, 1))
            expect(List#{counter.next()} == List#{5})
            expect(same(List#{()}, List#{()}))
            expect(same(((),), ((),)))
            counter.verify()
        }
    "#,
    );
}
