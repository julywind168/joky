use super::*;

#[test]
fn ordering_containers_dispatch_short_circuit_borrow_and_sort() {
    run_program(include_str!(
        "../../../tests/fixtures/ordering_containers.jk"
    ));
}

#[test]
fn ordering_containers_require_bounds_on_every_member() {
    for source in [
        "fn f(T: type, a: List(T), b: List(T)) -> Bool { a < b } fn main() {}",
        "struct Plain {} fn main() { Some(Plain()) < Some(Plain()) }",
        "struct Plain {} fn main() { (1, Plain()) < (2, Plain()) }",
        "struct Plain {} fn main() { let a: Result(Int32, Plain) = Ok(1); a < a }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("PartialOrd"), "{source}\n{error}");
    }
    for source in [
        "fn main() { Ord.compare(Some(1.0), Some(2.0)) }",
        "fn main() { Ord.compare((1, 1.0), (1, 2.0)) }",
        "fn main() { let a: Result(Int32, Float64) = Ok(1); Ord.compare(a, a) }",
        "fn main() { List#{Some(1.0)}.sorted() }",
        "fn main() { MutList#{List#{1.0}}.sort() }",
        "struct P {} impl PartialEq for P { fn equals(&self, other: &Self) -> Bool { true } } impl PartialOrd for P { fn partial_compare(&self, other: &Self) -> Option(Ordering) { None } } fn main() { List#{Some(P())}.sorted() }",
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains("Ord"), "{source}\n{error}");
    }
}

#[test]
fn ordering_lists_iterate_over_long_equal_prefixes() {
    // Keep long-list destruction off the small test-worker stack.
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            run_program(
                r#"
        class Builder {
            var values: List(Int32)
            var count: Int32
            fn build(&self) -> List(Int32) {
                while self.count < 4000 {
                    self.values = self.values.push_front(self.count)
                    self.count = self.count + 1
                }
                self.values
            }
        }
        fn main() {
            let a = Builder(values: List#{0}, count: 0).build()
            let b = Builder(values: List#{1}, count: 0).build()
            if Ord.compare(a, a) != Ordering.Equal { panic("long equal list") }
            if Ord.compare(a, b) != Ordering.Less { panic("long prefix") }
            if !(a < b) { panic("long partial prefix") }
        }
    "#,
            )
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn ordering_builtin_custom_and_generic_dispatch() {
    run_program(include_str!("../../../tests/fixtures/ordering.jk"));
}

#[test]
fn ordering_all_numeric_widths_and_ieee_semantics() {
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
            fn expect(b: Bool) {{ if !b {{ panic("integer ordering") }} else {{}} }}
            fn main() {{
                let low: {ty} = {low}
                let high: {ty} = {high}
                expect(low < high)
                expect(high > low)
                expect(low <= low)
                expect(high >= high)
                expect(Ord.compare(low, high) == Ordering.Less)
                expect(Ord.compare(high, low) == Ordering.Greater)
                expect(PartialOrd.partial_compare(low, low) == Some(Ordering.Equal))
            }}
        "#
        ));
    }
    for ty in ["Float32", "Float64"] {
        run_program(&format!(
            r#"
            fn expect(b: Bool) {{ if !b {{ panic("IEEE ordering") }} else {{}} }}
            fn unordered(a: {ty}, b: {ty}) {{
                expect(!(a < b)); expect(!(a <= b)); expect(!(a > b)); expect(!(a >= b))
                expect(PartialOrd.partial_compare(a, b).is_none())
            }}
            fn main() {{
                let zero: {ty} = 0.0
                let one: {ty} = 1.0
                let nan = zero / zero
                let inf = one / zero
                let neginf: {ty} = -inf
                unordered(nan, one); unordered(one, nan); unordered(nan, nan)
                expect(PartialOrd.partial_compare(zero, -zero) == Some(Ordering.Equal))
                expect(PartialOrd.partial_compare(inf, one) == Some(Ordering.Greater))
                expect(PartialOrd.partial_compare(neginf, one) == Some(Ordering.Less))
                expect(PartialOrd.partial_compare(inf, inf) == Some(Ordering.Equal))
            }}
        "#
        ));
    }
}

#[test]
fn ordering_borrows_and_evaluates_once_in_order() {
    run_program(
        r#"
        class Value { let key: Int32 }
        impl PartialEq for Value { fn equals(&self, other: &Self) -> Bool { self.key == other.key } }
        impl PartialOrd for Value {
            fn partial_compare(&self, other: &Self) -> Option(Ordering) {
                if (self.key == 0) || (other.key == 0) { None }
                else { PartialOrd.partial_compare(self.key, other.key) }
            }
        }
        class Counter {
            var value: Int32
            fn next(&self, expected: Int32) -> Value {
                if self.value != expected { panic("operand order") }
                self.value = self.value + 1
                Value(key: self.value)
            }
        }
        fn expect(b: Bool) { if !b { panic("borrowed ordering") } }
        fn main() {
            let a = Value(key: 1)
            let b = Value(key: 2)
            let none = Value(key: 0)
            expect(a < b); expect(a <= b); expect(b > a); expect(b >= a)
            expect(PartialOrd.partial_compare(a, b) == Some(Ordering.Less))
            expect(a.key == 1); expect(b.key == 2)
            expect(!(none < a)); expect(!(none <= a)); expect(!(none > a)); expect(!(none >= a))
            expect(!(a < none)); expect(!(a <= none)); expect(!(a > none)); expect(!(a >= none))
            let c = Counter(value: 0)
            expect(c.next(0) < c.next(1))
            expect(c.value == 2)
        }
    "#,
    );
}

#[test]
fn ordering_rejects_invalid_bounds_and_implementations() {
    for (source, message) in [
        ("fn f(T: type + Ord, x: T) {} fn main() { f(1.0) }", "Ord"),
        ("fn f(T: type, x: &T, y: &T) -> Bool { x < y } fn main() {}", "PartialOrd"),
        ("fn main() { 1 < true }", "PartialOrd"),
        ("fn main() { Ord.compare(1, true) }", "Int32"),
        ("class V {} impl PartialOrd for V { fn partial_compare(&self, other: &Self) -> Option(Ordering) { None } } fn main() {}", "PartialEq"),
        ("class V {} impl Ord for V { fn compare(&self, other: &Self) -> Ordering { Ordering.Equal } } fn main() {}", "Eq"),
        ("struct V {} impl PartialEq for V { fn equals(&self, other: &Self) -> Bool { true } } impl Eq for V {} impl Ord for V { fn compare(&self, other: &Self) -> Ordering { Ordering.Equal } } fn main() {}", "PartialOrd"),
        ("struct V {} impl PartialEq for V { fn equals(&self, other: &Self) -> Bool { true } } impl PartialOrd for V { fn partial_compare(&self, other: Self) -> Option(Ordering) { None } } fn main() {}", "PartialOrd"),
        ("eff Log { fn write() -> Unit } struct V {} impl PartialOrd for V { fn partial_compare(&self, other: &Self) -> Option(Ordering) effects { Log } { Log.write(); None } } fn main() {}", "PartialOrd"),
        ("trait Ord {} fn main() {}", "Ord"),
        ("enum Ordering { Wrong } fn main() {}", "Ordering"),
        ("fn main() { MutList#{1} < MutList#{2} }", "PartialOrd"),
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains(message), "{source}\n{error}");
    }
}

#[test]
fn ordering_rejects_transitive_suspension() {
    for (method, result, name) in [
        ("partial_compare", "Option(Ordering)", "PartialOrd"),
        ("compare", "Ordering", "Ord"),
    ] {
        let implementations = if name == "Ord" {
            "impl Eq for V {} impl PartialOrd for V { fn partial_compare(&self, other: &Self) -> Option(Ordering) { None } }"
        } else {
            ""
        };
        let value = if name == "Ord" {
            "Ordering.Equal"
        } else {
            "None"
        };
        let source = format!(
            r#"
            fn background() {{ branch {{}} }}
            class V {{}}
            impl PartialEq for V {{ fn equals(&self, other: &Self) -> Bool {{ true }} }}
            {implementations}
            impl {name} for V {{ fn {method}(&self, other: &Self) -> {result} {{ background(); {value} }} }}
            fn main() {{ let a = V(); let b = V(); {name}.{method}(a, b) }}
        "#
        );
        let error = try_run_program(&source).unwrap_err().to_string();
        assert!(
            error.contains(&format!("{name}.{method} must be synchronous")),
            "{error}"
        );
    }
}
