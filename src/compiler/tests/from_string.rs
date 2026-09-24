use super::*;

#[test]
fn from_string_builtin_ranges_syntax_and_errors() {
    run_program(&format!(
        "{}\nfn main() {{ check() }}",
        include_str!("../../../tests/fixtures/from_string_builtins.jk")
    ));
}

#[test]
fn from_string_dispatches_builtin_custom_and_generic_targets() {
    run_program(
        r#"
        struct Count {
            let value: Int32
            fn from_string(value: &String) -> Result(Count, String) { Err("inherent") }
        }
        impl FromString for Count {
            fn from_string(value: &String) -> Result(Self, String) {
                Ok(Count(value: value.parse(Int32)?))
            }
        }
        class Label { let value: String }
        class Counter {
            var calls: Int32
            fn text(&self) -> String {
                self.calls = self.calls + 1
                "42".concat("")
            }
        }
        impl FromString for Label {
            fn from_string(value: &String) -> Result(Self, String) {
                if value.is_empty() { Err("empty") } else { Ok(Label(value: value)) }
            }
        }
        fn parse(T: type + FromString, value: String) -> Result(T, String) { value.parse(T) }
        fn explicit(T: type + FromString, value: String) -> Result(T, String) {
            FromString.from_string(T, value)
        }
        fn main() {
            let counter = Counter(calls: 0)
            if counter.text().parse(Count)!.value != 42 { panic("evaluated receiver") }
            if counter.calls != 1 { panic("receiver evaluated more than once") }
            if "42".parse(Int32)! != 42 { panic("int") }
            if "1.5".parse(Float64)! != 1.5 { panic("float") }
            if !"2147483648".parse(Int32).is_err() { panic("overflow") }
            if !"invalid".parse(Float64).is_err() { panic("invalid") }
            if FromString.from_string(Int32, value: "7")! != 7 { panic("qualified builtin") }
            if FromString.from_string(Float64, "2.5")! != 2.5 { panic("qualified float") }
            if "42".parse(Count)!.value != 42 { panic("custom") }
            if parse(Count, "43")!.value != 43 { panic("generic custom") }
            if parse(Int32, "44")! != 44 { panic("generic builtin") }
            if explicit(Count, "45")!.value != 45 { panic("generic qualified") }
            if explicit(Float64, "4.5")! != 4.5 { panic("generic qualified builtin") }
            if FromString.from_string(Count, "46")!.value != 46 { panic("qualified") }
            let label = "hello".parse(Label)!
            if label.value != "hello" { panic("class") }
            if !"".parse(Label).is_err() { panic("custom error") }
            if !"invalid".parse(Count).is_err() { panic("propagated error") }
        }
    "#,
    );
}

#[test]
fn static_trait_methods_have_no_instance_receiver() {
    run_program(
        r#"
        trait Factory { fn create(value: Int32) -> Self }
        struct Left { let value: Int32 }
        struct Right { let value: Int32 }
        impl Factory for Left { fn create(value: Int32) -> Self { Left(value: value) } }
        impl Factory for Right { fn create(value: Int32) -> Self { Right(value: value + 1) } }
        trait Default { fn default() -> Self }
        impl Default for Left { fn default() -> Self { Left(value: 42) } }
        trait CopyValue { fn copy(value: &Self) -> Int32 }
        class Value { let number: Int32 }
        impl CopyValue for Value { fn copy(value: &Self) -> Int32 { value.number } }
        fn make(T: type + Factory, value: Int32) -> T { Factory.create(T, value) }
        fn main() {
            if make(Left, 2).value != 2 { panic("left") }
            if Factory.create(Right, 2).value != 3 { panic("right") }
            if Default.default(Left).value != 42 { panic("no arguments") }
            let value = Value(number: 7)
            if CopyValue.copy(Value, value) != 7 { panic("borrow") }
            if CopyValue.copy(Value, value) != 7 { panic("borrow again") }
        }
    "#,
    );
}

#[test]
fn from_string_rejects_invalid_bounds_signatures_and_dispatch() {
    for (source, expected) in [
        ("fn main() { \"1\".parse(Bytes) }", "FromString"),
        ("struct Value {} fn main() { \"1\".parse(Value) }", "FromString"),
        ("fn parse(T: type, value: String) -> Result(T, String) { value.parse(T) } fn main() {}", "FromString"),
        ("fn main() { \"1\".parse() }", "expects exactly"),
        ("fn main() { \"1\".parse(Int32, 2) }", "expects exactly"),
        ("fn main() { \"1\".parse(1) }", "compile-time type"),
        ("fn main() { \"1\".parse(T: Int32) }", "compile-time type"),
        ("fn main() { FromString.from_string(1, \"1\") }", "compile-time target"),
        ("fn main() { FromString.from_string(Bytes, \"1\") }", "FromString"),
        ("fn main() { FromString.from_string(Int32, 1) }", "expected String"),
        ("fn use(value: Dyn(FromString)) {} fn main() {}", "not object safe"),
        ("struct Value {} impl FromString for Value { fn from_string(&self, value: &String) -> Result(Self, String) { Ok(Value()) } } fn main() {}", "FromString"),
        ("struct Value {} impl FromString for Value { fn from_string(value: String) -> Result(Self, String) { Ok(Value()) } } fn main() {}", "FromString"),
        ("struct Value {} impl FromString for Value { fn from_string(value: &String) -> Result(Self, String) { self } } fn main() {}", "self"),
        ("struct Value {} impl FromString for Value { fn from_string(value: &String) -> Result(Self, String) { Ok(Value()) } } fn main() { Value().from_string(\"1\") }", "unknown function"),
        ("trait Factory { fn create() -> Int32 } fn use(value: Dyn(Factory)) {} fn main() {}", "not object safe"),
        ("@intrinsic struct String { fn parse(&self, T: type) -> Result(T, String) } fn main() {}", "requires T: FromString"),
        ("@intrinsic struct String { fn parse(&self, T: type + Unknown) -> Result(T, String) } fn main() {}", "Unknown"),
        ("eff Log { fn write() -> Unit } struct Value {} impl FromString for Value { fn from_string(value: &String) -> Result(Self, String) effects { Log } { Log.write(); Ok(Value()) } } fn main() {}", "FromString"),
        ("eff Log { fn write() -> Unit } struct Value {} impl FromString for Value { fn from_string(value: &String) -> Result(Self, String) { Log.write(); Ok(Value()) } } fn main() {}", "Log"),
        ("trait FromString { fn from_string(value: String) -> Result(Self, String) } fn main() {}", "builtin definition"),
        ("trait Factory { fn create() -> Int32 } eff Log { fn write() -> Unit } struct Value {} impl Factory for Value { fn create() -> Int32 effects { Log } { Log.write(); 1 } } fn main() {}", "Factory"),
        ("@intrinsic struct String { fn parse(&self, T: type + FromString, T: type) -> Result(T, String) } fn main() {}", "duplicate intrinsic type parameter"),
    ] {
        let error = try_run_program(source).unwrap_err().to_string();
        assert!(error.contains(expected), "{source}\n{error}");
    }
}

#[test]
fn intrinsic_parse_checks_method_where_bounds() {
    run_program(
        r#"
        @intrinsic struct String { fn parse(&self, Target: type) -> Result(Target, String) where Target: FromString + Ord }
        fn parse(T: type + FromString + Ord, text: String) -> Result(T, String) { text.parse(T) }
        fn main() { if parse(Int32, "42")! != 42 { panic("where") } }
    "#,
    );
    let error = try_run_program(r#"
        @intrinsic struct String { fn parse(&self, T: type + FromString) -> Result(T, String) where T: Ord }
        fn main() { "1.5".parse(Float64) }
    "#).unwrap_err().to_string();
    assert!(error.contains("Ord"), "{error}");
}
