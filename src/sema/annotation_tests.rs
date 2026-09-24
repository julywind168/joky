use super::*;

fn check(source: &str) -> Result<CheckedTypes, Diagnostic> {
    check_program(&crate::syntax::parse_program(source)?)
}

#[test]
fn annotations_evaluate_forward_type_functions_and_nested_aliases() {
    check(
        r#"
        struct Packet { let values: ListOf(Int64) }
        enum Reply { Ready(value: Wrapped(Int64)); Missing }
        eff Ask { fn read() -> ListOf(Int64) }
        fn first(T: type, values: ListOf(T)) -> Option(T) { values.head() }
        fn main() {
            let N = Int64
            let values: ListOf(N) = List(1, 2)
            let result: Option(N) = first(N, values)
            let legacy: Option(List(N)) = Some(List(1))
            let mixed: Result(ListOf(N), String) = Ok(List(1))
            let packet = Packet(values: List(1))
        }
        fn ListOf(T: type) -> type { List(T) }
        fn Wrapped(T: type) -> type { Option(ListOf(T)) }
    "#,
    )
    .unwrap();
}

#[test]
fn nested_function_and_tuple_annotations_are_structural() {
    check(
        r#"
        fn ListOf(T: type) -> type { List(T) }
        fn call(f: fn(fn(Int64) -> Int64) -> Int64) -> Int64 {
            f(fn(x: Int64) -> Int64 { x })
        }
        fn pair() -> (ListOf(Int64), String) { (List(42), "ok") }
        fn main() {
            let N = Int64
            let f: fn(N) -> ListOf(N) = fn(x: N) -> ListOf(N) { List(x) }
            let result = f(42)
            let nested: fn(fn(Int64) -> Int64) -> Int64 =
                fn(g: fn(Int64) -> Int64) -> Int64 { g(42) }
            let value = call(nested)
            let data = pair()
        }
    "#,
    )
    .unwrap();
}

#[test]
fn annotation_calls_support_labels_and_preserve_type_identity() {
    let program = crate::syntax::parse_program(
        r#"
        fn Either(A: type, B: type) -> type { Result(A, B) }
        fn main() {
            let a: Either(B: String, A: Int64) = Ok(42)
            let b: Result(Int64, String) = a
            let c: Result(Int64, String) = b
        }
    "#,
    )
    .unwrap();
    let types = check_program(&program).unwrap();
    let crate::syntax::ExprKind::Block(expressions) = &program.functions[1].body.kind else {
        panic!("block")
    };
    let resolved = expressions
        .iter()
        .map(|expr| {
            let crate::syntax::ExprKind::Let {
                annotation: Some(annotation),
                ..
            } = &expr.kind
            else {
                panic!("annotation")
            };
            types.checked_annotation(annotation, &[], None).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved[0], resolved[1]);
    assert_eq!(resolved[1], resolved[2]);
}

#[test]
fn bad_annotation_calls_fail_in_sema_with_source_spans() {
    for annotation in [
        "ListOf()",
        "ListOf(Int64, String)",
        "Unknown(Int64)",
        "ListOf(Unknown)",
        "ListOf(wrong: Int64)",
        "Option(Int64, String)",
        "Map(Float64, String)",
        "ListOf(Counter)",
        "Int64(String)",
    ] {
        let source = format!(
            "class Counter {{ var n: Int64 }}
             fn ListOf(T: type) -> type {{ List(T) }}
             fn bad(value: {annotation}) {{}}
             fn main() {{}}"
        );
        let error = check(&source).expect_err(&source);
        assert!(error.span().is_some(), "{error}");
    }
    assert!(check(
        r#"
        fn ListOf(T: type) -> type { List(T) }
        fn main() { let N = 1; let values: ListOf(N) = List(1) }
    "#
    )
    .is_err());
}

#[test]
fn associated_types_work_inside_type_function_applications() {
    check(
        r#"
        trait Source { type Item; fn next(&self) -> Option(Item) }
        struct Items { let value: Int64 }
        impl Source for Items {
            type Item = Int64
            fn next() -> Option(Self.Item) { Some(self.value) }
        }
        fn Maybe(T: type) -> type { Option(T) }
        fn next(I: type + Source, iter: I) -> Maybe(I.Item) { iter.next() }
        fn main() { let value = next(Items(value: 42))! }
    "#,
    )
    .unwrap();
}
