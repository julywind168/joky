use super::*;

fn check(source: &str) -> Result<CheckedTypes, Diagnostic> {
    check_program(&crate::syntax::parse_program(source)?)
}

#[test]
fn type_functions_compose_with_forward_references_and_local_bindings() {
    check(
        r#"
        fn Nested(T: type) -> type {
            let Inner: type = Wrap(T)
            List(Inner)
        }
        fn Wrap(T: type) -> type { Result(T, String) }
        fn Number() -> type { Int64 }
        fn identity(T: type, value: T) -> T { value }
        fn main() {
            let N: type = Number()
            let n: N = 42
            let values = identity(Nested(N), List(Ok(n)))
        }
    "#,
    )
    .unwrap();
}

#[test]
fn type_functions_can_create_cached_anonymous_struct_types() {
    check(
        r#"
        fn Box(T: type) -> type {
            struct {
                let value: T
            }
        }
        fn take(value: Box(Int64)) -> Unit { let copy = value; }
        fn main() {
            let A: type = Box(Int64)
            let B: type = Box(Int64)
            let C: type = Box(String)
            let value: Box(Int64) = Box(Int64)(value: 1)
            take(value)
        }
    "#,
    )
    .unwrap();
}

#[test]
fn anonymous_struct_types_support_nesting_and_ownership_checks() {
    check(
        r#"
        fn Pair(T: type) -> type {
            struct {
                let first: T
                let second: T
            }
        }
        fn Box(T: type) -> type {
            struct { let value: T }
        }
        fn main() {
            let value: Pair(Box(Int64)) = Pair(Box(Int64))(first: Box(Int64)(value: 1), second: Box(Int64)(value: 2))
        }
    "#,
    )
    .unwrap();

    let source = r#"
        class Counter { var value: Int64 }
        fn Box(T: type) -> type { struct { let value: T } }
        fn main() {
            let invalid: List(Box(Counter)) = List(Box(Counter)(value: Counter(value: 1)))
        }
    "#;
    assert!(check(source).is_err());
}

#[test]
fn type_functions_support_reordered_labels_and_builtin_constructors() {
    check(
        r#"
        fn Either(A: type, B: type) -> type { Result(A, B) }
        fn main() {
            let R = Either(B: String, A: Int64)
            let value: R = Ok(42)
            let L = MutList(Int64)
            let M = MutMap(String, Int64)
            let S = MutSet(String)
            let I = Set(Int64)
            let O = Option(R)
        }
    "#,
    )
    .unwrap();
}

#[test]
fn malformed_type_functions_are_rejected_even_when_unused() {
    for definition in [
        "fn Bad(T: type) -> type { 42 }",
        "fn Bad(T: type) -> type {}",
        "fn Bad(T: type) -> type { missing }",
        "fn Bad(T: type) -> type { println(\"ignored\"); T }",
        "fn Bad(T: type) -> type { let X = 42; T }",
        "fn Bad(T: type) -> type { let X: Int64 = T; T }",
        "fn Bad(T: type) -> type { List(T, T) }",
        "fn Bad(T: type) -> type { List() }",
        "fn Bad(T: type) -> type { List(element: T) }",
        "fn Bad(T: type) -> type { Bad(T) }",
        "fn Bad(T: type) -> type { Other(T) } fn Other(T: type) -> type { Bad(T) }",
        "fn Bad(T: type, T: type) -> type { T }",
        "fn Bad(value: Int64) -> type { Int64 }",
        "fn Bad(T: type + Show) -> type { T }",
        "eff E { fn op() -> Int64 } fn Bad() -> type effects { E } { Int64 }",
        "fn Bad() -> type { Int64 } fn Bad() -> Int64 { 1 }",
        "fn Bad() -> Int64 { 1 } fn Bad() -> type { Int64 }",
        "fn Bad() -> type { Int64 } fn Bad() -> type { String }",
        "const Bad = 1; fn Bad() -> type { Int64 }",
        "fn List() -> type { Int64 }",
        "fn Bad() -> type { List(Counter) } class Counter { var n: Int64 }",
    ] {
        let source = format!("{definition}\nfn main() {{}}");
        let error = check(&source).expect_err(&source);
        assert!(error.span().is_some(), "{source}: {error}");
    }
    assert!(check("fn main() -> type { Int64 }").is_err());
}

#[test]
fn type_function_calls_reject_runtime_values_and_bad_arguments() {
    for body in [
        "let X = Wrap()",
        "let X = Wrap(Int64, String)",
        "let X = Wrap(42)",
        "let X = Wrap(Unknown)",
        "let X = Wrap(wrong: Int64)",
        "let X: Int64 = Wrap(Int64)",
        "println(Wrap(Int64))",
        "Wrap(Int64)",
        "let X = Wrap(Int64); println(X)",
        "let X = Pair(A: Int64, A: String)",
    ] {
        let source = format!(
            "fn Wrap(T: type) -> type {{ List(T) }}
             fn Pair(A: type, B: type) -> type {{ Result(A, B) }}
             fn main() {{ {body} }}"
        );
        assert!(check(&source).is_err(), "{source}");
    }
}

#[test]
fn type_function_evaluation_is_lexical_not_dynamically_scoped() {
    assert!(check(
        r#"
        fn Hidden() -> type { Local }
        fn main() { let Local = Int64; let X = Hidden() }
    "#
    )
    .is_err());
    assert!(check(
        r#"
        fn Hidden() -> type { T }
        fn Caller(T: type) -> type { Hidden() }
        fn main() { let X = Caller(Int64) }
    "#
    )
    .is_err());
    check(
        r#"
        fn Number() -> type { Int64 }
        fn main() {
            let Int64 = String
            let N = Number()
            let n: N = 42
        }
    "#,
    )
    .unwrap();
}

#[test]
fn type_bindings_follow_value_shadowing_and_block_scope() {
    check(
        r#"
        fn identity(T: type, value: T) -> T { value }
        fn main() {
            let N = Int64
            { let N = String; let text: N = "ok" }
            let n: N = 42
            let N = 7
            let value = N + 1
            let N = String
            let text = identity(N, "ok")
        }
    "#,
    )
    .unwrap();
    for body in [
        "{ let N = Int64 }; let value = identity(N, 1)",
        "let N = Int64; let N = 7; let value = identity(N, 1)",
        "let Int64 = 7; let value = identity(Int64, 1)",
        "let N = Int64; let f = fn (N: Int64) -> Int64 { identity(N, 1) }",
    ] {
        let source =
            format!("fn identity(T: type, value: T) -> T {{ value }} fn main() {{ {body} }}");
        assert!(check(&source).is_err(), "{source}");
    }
}

#[test]
fn type_construction_validates_container_constraints() {
    for expression in [
        "List(Counter)",
        "Option(List(Counter))",
        "Map(Float64, String)",
        "Wrap(Counter)",
    ] {
        let source = format!(
            "class Counter {{ var n: Int64 }}
             fn Wrap(T: type) -> type {{ List(T) }}
             fn main() {{ let Invalid = {expression} }}"
        );
        assert!(check(&source).is_err(), "{source}");
    }
}

#[test]
fn type_evaluation_depth_is_bounded() {
    let mut source = String::new();
    for index in 0..70 {
        source.push_str(&format!(
            "fn F{index}(T: type) -> type {{ F{}(T) }}\n",
            index + 1
        ));
    }
    source.push_str("fn F70(T: type) -> type { T } fn main() {}");
    let error = check(&source).unwrap_err();
    assert!(error.to_string().contains("depth"), "{error}");
}

#[test]
fn generated_struct_type_functions_reject_recursive_instances() {
    for definition in [
        "fn Node(T: type) -> type { struct { let next: Option(Node(T)) } }",
        "fn A(T: type) -> type { B(T) } fn B(T: type) -> type { A(T) }",
        "fn A(T: type) -> type { struct { let next: Option(B(T)) } } fn B(T: type) -> type { A(T) }",
    ] {
        let source = format!("{definition}\nfn main() {{ let N = Node(Int64) }}");
        assert!(check(&source).is_err(), "{source}");
    }
}

#[test]
fn type_functions_can_create_anonymous_enums() {
    check(
        r#"
        fn Maybe(T: type) -> type {
            enum {
                Some(value: T)
                None
            }
        }
        fn main() {
            let M: type = Maybe(Int64)
            let value: M = Maybe(Int64).Some(value: 42)
        }
    "#,
    )
    .unwrap();
}
