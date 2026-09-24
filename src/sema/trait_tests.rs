use super::*;

fn check(source: &str) -> Result<CheckedTypes, Diagnostic> {
    check_program(&crate::syntax::parse_program(source)?)
}

const TRAITS: &str = r#"
    trait First { fn read(&self) -> Int32 }
    trait Second { fn read(&self) -> String }
    struct Value { let value: Int32 }
    impl First for Value { fn read(&self) -> Int32 { self.value } }
    impl Second for Value { fn read(&self) -> String { "second" } }
"#;

#[test]
fn qualified_trait_calls_select_signatures_and_check_bounds() {
    check(&format!("{TRAITS}\nfn main() {{ let v = Value(value: 1); let n: Int32 = First.read(v); let s: String = Second.read(v) }}")).unwrap();
    for (source, message) in [
        (format!("{TRAITS}\nfn main() {{ let n: Int32 = Second.read(Value(value: 1)) }}"), "expected Int32, found String"),
        (format!("{TRAITS}\nfn main() {{ First.read(1) }}"), "does not implement trait 'First'"),
        (format!("{TRAITS}\nfn f(T: type + Second, value: T) -> Int32 {{ First.read(value) }} fn main() {{}}"), "does not implement trait 'First'"),
        (format!("{TRAITS}\nfn main() {{ First.missing(Value(value: 1)) }}"), "unknown function 'First.missing'"),
        (format!("{TRAITS}\nfn main() {{ First.read() }}"), "expects exactly 1 argument"),
    ] {
        let error = check(&source).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn trait_method_ambiguity_is_rejected_for_concrete_and_generic_receivers() {
    for body in [
        "fn main() { Value(value: 1).read() }",
        "fn f(T: type + First + Second, value: T) -> Int32 { value.read() } fn main() {}",
        "fn f(T: type + Second + First, value: T) -> String { value.read() } fn main() {}",
    ] {
        let error = check(&format!("{TRAITS}\n{body}")).unwrap_err().to_string();
        assert!(error.contains("ambiguous method 'read'"), "{error}");
        assert!(
            error.contains("First") && error.contains("Second"),
            "{error}"
        );
    }
}

#[test]
fn qualified_trait_calls_resolve_associated_types_and_self_parameters() {
    check(r#"
        trait Source { type Item; fn get(&self) -> Item }
        trait Compare { fn same(&self, other: Self) -> Bool }
        struct Value { let value: Int32 }
        impl Source for Value { type Item = Int32; fn get(&self) -> Int32 { self.value } }
        impl Compare for Value { fn same(&self, other: Value) -> Bool { self.value == other.value } }
        fn get(T: type + Source, value: T) -> T.Item { Source.get(value) }
        fn same(T: type + Compare, value: T, other: T) -> Bool { Compare.same(value, other: other) }
        fn main() { let value = Value(value: 1); let n: Int32 = get(value); same(value, value) }
    "#).unwrap();
}

#[test]
fn trait_names_can_be_shadowed_by_values() {
    check(
        r#"
        trait Read { fn read(&self) -> Int32 }
        struct Value { fn read(&self, value: Int32) -> Int32 { value } }
        fn main() { let Read = Value(); Read.read(42) }
    "#,
    )
    .unwrap();
}

#[test]
fn qualified_trait_calls_enforce_effect_and_drop_contracts() {
    let error = check(
        r#"
        eff Read { fn value() -> Int32 }
        trait Source { fn get(&self) -> Int32 effects { Read } }
        fn get(T: type + Source, value: T) -> Int32 { Source.get(value) }
        fn main() {}
    "#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("Read"), "{error}");
    let error = check(
        r#"
        class C {}
        impl Drop for C { fn drop(&self) {} }
        fn main() { Drop.drop(C()) }
    "#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("cannot be called directly"), "{error}");
}

#[test]
fn builtin_trait_methods_participate_in_ambiguity_checks() {
    let definitions = r#"
        class Value {
            fn show(&self) -> String { "inherent" }
            fn debug(&self) -> String { "inherent" }
            fn drop(&self) {}
        }
        impl Show for Value { fn show(&self) -> String { "Show" } }
        impl Debug for Value { fn debug(&self) -> String { "Debug" } }
        impl Drop for Value { fn drop(&self) {} }
    "#;
    for name in ["show", "debug", "drop"] {
        let error = check(&format!("{definitions}\nfn main() {{ Value().{name}() }}"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("ambiguous method '{name}'")),
            "{error}"
        );
    }
    let error = check(&format!(
        "{definitions}\nfn main() {{ Drop.drop(Value()) }}"
    ))
    .unwrap_err()
    .to_string();
    assert!(error.contains("cannot be called directly"), "{error}");
}

#[test]
fn generic_trait_bounds_reject_extra_declared_effects() {
    for implementation_body in ["Read.value()", "42"] {
        for generic_body in ["value.read()", "Source.read(value)", "42"] {
            let source = format!(
                r#"
                eff Read {{ fn value() -> Int32 }}
                trait Source {{ fn read(&self) -> Int32 }}
                struct Value {{}}
                impl Source for Value {{
                    fn read(&self) -> Int32 effects {{ Read }} {{ {implementation_body} }}
                }}
                fn read(T: type + Source, value: T) -> Int32 {{ {generic_body} }}
                fn main() {{ read(Value()) }}
                "#
            );
            let error = check(&source).expect_err(&source).to_string();
            for expected in ["Source", "read", "Read"] {
                assert!(error.contains(expected), "{source}\n{error}");
            }
        }
    }
}

#[test]
fn generic_trait_bounds_check_every_instance_method() {
    let error = check(
        r#"
        eff Write { fn record() -> Unit }
        trait Source {
            fn read(&self) -> Int32
            fn record(&self) -> Unit
        }
        struct Value {}
        impl Source for Value {
            fn read(&self) -> Int32 { 42 }
            fn record(&self) effects { Write } {}
        }
        fn read(T: type + Source, value: T) -> Int32 { value.read() }
        fn main() { read(Value()) }
        "#,
    )
    .unwrap_err()
    .to_string();
    for expected in ["Source", "record", "Write"] {
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn generic_trait_bounds_check_forward_calls_from_type_methods() {
    for declaration in ["struct", "class"] {
        let source = format!(
            r#"
            eff Read {{ fn value() -> Int32 }}
            trait Source {{ fn read(&self) -> Int32 }}
            {declaration} Caller {{
                fn invoke(&self) -> Int32 {{ read(Value()) }}
            }}
            struct Value {{}}
            impl Source for Value {{
                fn read(&self) -> Int32 effects {{ Read }} {{ 42 }}
            }}
            fn read(T: type + Source, value: T) -> Int32 {{ value.read() }}
            fn main() {{ Caller().invoke() }}
            "#
        );
        let error = check(&source).expect_err(&source).to_string();
        for expected in ["Source", "read", "Read"] {
            assert!(error.contains(expected), "{source}\n{error}");
        }
    }
}

#[test]
fn generic_trait_bounds_check_nested_forwarding() {
    let source = r#"
        eff Read { fn value() -> Int32 }
        trait Source { fn read(&self) -> Int32 }
        struct Value {}
        impl Source for Value {
            fn read(&self) -> Int32 effects { Read } { 42 }
        }
        fn read(T: type + Source, value: T) -> Int32 { value.read() }
        fn forward(T: type + Source, value: T) -> Int32 { read(value) }
        fn outer(T: type + Source, value: T) -> Int32 { forward(value) }
        fn main() { outer(Value()) }
    "#;
    let error = check(source).unwrap_err().to_string();
    for expected in ["Source", "read", "Read"] {
        assert!(error.contains(expected), "{error}");
    }
    check(&source.replace("outer(Value())", "")).unwrap();
    check(&source.replace("effects { Read } { 42 }", "{ 42 }")).unwrap();
}

#[test]
fn generic_trait_bounds_accept_effect_subsets() {
    for implementation in [
        "fn read(&self) -> Int32 { 42 }",
        "fn read(&self) -> Int32 effects { Read } { Read.value() }",
        "fn read(&self) -> Int32 effects { Read, Write } { Write.record(); Read.value() }",
    ] {
        check(&format!(
            r#"
            eff Read {{ fn value() -> Int32 }}
            eff Write {{ fn record() -> Unit }}
            trait Source {{ fn read(&self) -> Int32 effects {{ Read, Write }} }}
            struct Value {{}}
            impl Source for Value {{ {implementation} }}
            fn read(T: type + Source, value: T) -> Int32 effects {{ Read, Write }} {{
                value.read()
            }}
            fn main() effects {{ Read, Write }} {{ read(Value()) }}
            "#
        ))
        .unwrap();
    }
}

#[test]
fn generic_trait_bounds_allow_internal_task_waits_without_external_effects() {
    check(
        r#"
        trait Source { fn read(&self) -> Int32 }
        struct Value {}
        impl Source for Value {
            fn read(&self) -> Int32 {
                let result = parallel {
                    | 42
                }
                result.0
            }
        }
        fn read(T: type + Source, value: T) -> Int32 { value.read() }
        fn main() { read(Value()) }
        "#,
    )
    .unwrap();
}

#[test]
fn concrete_trait_calls_still_use_the_implementation_effects() {
    for call in ["Value().read()", "Source.read(Value())"] {
        let source = format!(
            r#"
            eff Read {{ fn value() -> Int32 }}
            trait Source {{ fn read(&self) -> Int32 }}
            struct Value {{}}
            impl Source for Value {{
                fn read(&self) -> Int32 effects {{ Read }} {{ Read.value() }}
            }}
            fn main() effects {{ Read }} {{ {call} }}
            "#
        );
        check(&source).unwrap();
        let missing_effect = source.replace("fn main() effects { Read }", "fn main()");
        let error = check(&missing_effect).unwrap_err().to_string();
        assert!(error.contains("Read"), "{error}");
    }
}
