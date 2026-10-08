use super::*;

#[cfg(test)]
mod generic_tests {
    use super::*;

    #[test]
    fn generic_parameters_have_function_local_identity_and_explicit_calls_keep_spans() {
        let source = "// 🙂\nfn id<T,>(x: T) -> T { x } fn next<T>(x: T) -> T { id::<T,>(x) } fn run() { (id::<(Int64, Bool),>((1, true))) 8 >> 1 1 < 2 }";
        let module = parse(source).unwrap();
        let Item::FnDef(first) = &module.items[0] else {
            panic!()
        };
        let Item::FnDef(second) = &module.items[1] else {
            panic!()
        };
        assert_ne!(first.params[0].ty, second.params[0].ty);
        assert_eq!(first.ret_ty, first.params[0].ty);
        assert_eq!(second.ret_ty, second.params[0].ty);
        let Item::FnDef(run) = &module.items[2] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &run.body else {
            panic!()
        };
        let Expr::Call {
            type_args, span, ..
        } = &exprs[0]
        else {
            panic!()
        };
        assert_eq!(type_args, &[Type::tuple(vec![Type::Int64, Type::Bool])]);
        assert_eq!(
            span.slice(source),
            Some("(id::<(Int64, Bool),>((1, true)))")
        );
        assert!(matches!(exprs[1], Expr::BinOp { op: BinOp::Shr, .. }));
        assert!(matches!(exprs[2], Expr::BinOp { op: BinOp::Lt, .. }));
    }

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
        let source = "// 🙂\nenum Option<T,> { None, Some(T) } fn f(x: Option<Option<Int32>>) { let a: Option<Option<Int32>>=x Option.None::<Option<Int32>> Option.Some::<Option<Int32>,>(x) 8 >> 1 8 >= 1 }";
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
            "enum E<T: Copy> { A(T) }",
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

#[cfg(test)]
mod generic_record_tests {
    use super::*;

    #[test]
    fn generic_record_parameters_and_explicit_constructors_preserve_scope_and_spans() {
        let source = "// 🙂\npub struct Pair<T, U,> { left: T, right: U } struct Plain { value: T } fn f() { let p: Pair<Int32, Pair<Bool, Unit>>=Pair::<Int32, Pair<Bool, Unit>> { left: 1, right: Pair { left: true, right: {} } } app.Pair::<Int64, Bool> { left: 42, right: true }.left 8 >> 1 }";
        let module = parse(source).unwrap();
        let record = &module.records[0];
        assert!(record.public);
        assert_eq!(record.type_params.len(), 2);
        assert_eq!(record.type_params[0].span.slice(source), Some("T"));
        assert!(matches!(&record.fields[0].ty, Type::Parameter(p) if p == &record.type_params[0]));
        assert!(matches!(&module.records[1].fields[0].ty, Type::Named(name) if name == "T"));
        let Item::FnDef(f) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &f.body else {
            panic!()
        };
        let Expr::Let { value, .. } = &exprs[0] else {
            panic!()
        };
        let Expr::Record {
            type_args, span, ..
        } = value.as_ref()
        else {
            panic!()
        };
        assert_eq!(type_args.len(), 2);
        assert_eq!(type_args[1].to_string(), "Pair<Bool, Unit>");
        assert!(span
            .slice(source)
            .unwrap()
            .starts_with("Pair::<Int32, Pair<Bool, Unit>> {"));
        assert!(
            matches!(&exprs[1], Expr::Field { base, .. } if matches!(base.as_ref(), Expr::Record { name, type_args, .. } if name == "app.Pair" && type_args.len() == 2))
        );
        assert!(matches!(exprs[2], Expr::BinOp { op: BinOp::Shr, .. }));
        for invalid in [
            "struct S<> {}",
            "struct S<T,T> {}",
            "struct S<_> {}",
            "struct S<T: Copy> { value: T }",
            "type Alias<T: Copy> = T",
            "impl<T: Copy +> Pair<T> {}",
            "impl Pair<Int32> {}",
        ] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
    }
}

#[cfg(test)]
mod generic_alias_tests {
    use super::*;

    #[test]
    fn generic_alias_parameters_keep_declaration_scope_and_source_spans() {
        let source = "// 🙂\npub type PairOf<T,> = (T, T) type Identity<T> = T type Plain = T fn f(x: PairOf<Identity<Int32>>) { let p: PairOf<Identity<Int32>>=x 8 >> 1 }";
        let module = parse(source).unwrap();
        let alias = &module.type_aliases[0];
        assert!(alias.public);
        assert_eq!(alias.type_params.len(), 1);
        assert_eq!(alias.name_span.slice(source), Some("PairOf"));
        assert_eq!(alias.target_span.slice(source), Some("(T, T)"));
        assert_eq!(alias.span.slice(source), Some("type PairOf<T,> = (T, T)"));
        assert_eq!(alias.type_params[0].span.slice(source), Some("T"));
        let Type::Tuple(tuple) = &alias.target else {
            panic!()
        };
        assert_eq!(
            tuple.elements,
            vec![Type::Parameter(alias.type_params[0].clone()); 2]
        );
        assert_ne!(alias.type_params[0], module.type_aliases[1].type_params[0]);
        assert_eq!(module.type_aliases[2].target, Type::Named("T".into()));
        let Item::FnDef(f) = &module.items[0] else {
            panic!()
        };
        assert_eq!(f.params[0].ty.to_string(), "PairOf<Identity<Int32>>");
        let Expr::Block { exprs, .. } = &f.body else {
            panic!()
        };
        assert!(matches!(exprs[1], Expr::BinOp { op: BinOp::Shr, .. }));
        for source in [
            "type X<> = Int32",
            "type X<T, T> = T",
            "type X<_> = Int32",
            "type X<T U> = T",
            "type X<T: Copy> = T",
            "type X<T = Int32> = T",
            "type X<T>",
            "type X<T> =",
            "type app.X<T> = T",
            "fn f() { type X<T> = T }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let params = (0..65)
            .map(|n| format!("T{n}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse(&format!("type X<{params}> = Unit"))
            .unwrap_err()
            .message
            .contains("64 type parameters"));
        let nested = format!("type X<T> = {}T{}", "X<".repeat(300), ">".repeat(300));
        assert!(parse(&nested).unwrap_err().message.contains("256"));
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
