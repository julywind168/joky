use super::*;

#[cfg(test)]
mod generic_tests {
    use super::*;

    #[test]
    fn generic_syntax_rejects_empty_duplicate_unseparated_and_unsupported_parameters() {
        for source in [
            "fn f<>() {}",
            "fn f<T,T>() {}",
            "fn f<_>() {}",
            "fn f<T U>() {}",
            "fn f<T: Eq +>(x: T) {}",
            "extern fn f<T>(x: T)",
            "struct R<T U> {}",
            "type X<T U> = T",
            "fn f() { id::<>() }",
            "fn f() { id::<Int32 Bool>(1) }",
            "fn f() { id::<Int32, ,>(1) }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let parameters = (0..65)
            .map(|i| format!("T{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse(&format!("fn f<{parameters}>() {{}}"))
            .unwrap_err()
            .message
            .contains("64 type parameters"));
    }
}

#[cfg(test)]
mod generic_enum_tests {
    use super::*;

    #[test]
    fn generic_enum_type_applications_split_closers_without_changing_shift_tokens() {
        let source = "// 🙂\nenum Option<T,> { None, Some(T) } fn f(x: Option<Option<Int32>>) { let a: Option<Option<Int32>>=x\n Option.None::<Option<Int32>>\n Option.Some::<Option<Int32>,>(x)\n 8 >> 1\n 8 >= 1 }";
        let module = parse(source).unwrap();
        let definition = &module.enums[0];
        assert_eq!(definition.type_params.len(), 1);
        assert_eq!(definition.type_params[0].span.slice(source), Some("T"));
        assert!(
            matches!(&definition.variants[1].payload, Some(Type::Parameter(p)) if p == &definition.type_params[0])
        );
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        assert_eq!(function.params[0].ty.to_string(), "Option<Option<Int32>>");
        assert!(matches!(&exprs[1], Expr::Field { type_args, .. } if type_args.len() == 1));
        assert_eq!(
            exprs[1].span().slice(source),
            Some("Option.None::<Option<Int32>>")
        );
        assert!(matches!(exprs[3], Expr::BinOp { op: BinOp::Shr, .. }));
        assert!(matches!(exprs[4], Expr::BinOp { op: BinOp::Ge, .. }));
        for invalid in [
            "enum E<> { A }",
            "enum E<T,T> { A }",
            "enum E<_> { A }",
            "enum E<T: Show> { A(T) }",
            "fn f(x: E<>) {}",
            "fn f(x: E<Int32 Bool>) {}",
            "fn f(x: E<E<Int32>) {}",
            "fn f() { E.None::< > }",
            "struct S<> {}",
            "type Alias<> = Int32",
        ] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
    }
}

#[test]
fn associated_equalities_reject_malformed_bounds_and_dyn_types() {
    for equalities in [
        "<>",
        "<Item>",
        "<Item =>",
        "<Item = Int32 Other = Bool>",
        "<Item = Int32,,>",
    ] {
        for source in [
            format!("fn f<T: Read{equalities}>(x: T) {{}}"),
            format!("fn f(x: &dyn Read{equalities}) {{}}"),
        ] {
            assert!(parse(&source).is_err(), "{source}");
        }
    }
}
