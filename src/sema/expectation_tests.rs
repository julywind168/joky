//! TypeExpectation usage examples and tests

#[cfg(test)]
mod tests {
    use crate::sema::{check_program, Type};
    use crate::syntax;

    #[test]
    fn hint_allows_integer_literal_inference() {
        // Hint: integer literals can infer different integer types from context
        let program = syntax::parse_program("fn main() { let x: Int64 = 42; x }").unwrap();
        let types = check_program(&program).unwrap();

        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected a block");
        };
        let crate::syntax::ExprKind::Let { value, .. } = &expressions[0].kind else {
            panic!("expected a let expression");
        };

        // The literal 42 is inferred as Int64 (not the default Int32)
        assert_eq!(types.get(value), Type::I64);
    }

    #[test]
    fn require_enforces_exact_type_match() {
        // Require: the type annotation requires the value to match the bound type
        let program = syntax::parse_program("fn main() { let x: Int32 = 100; x }").unwrap();
        assert!(check_program(&program).is_ok());

        // A type mismatch reports an error
        let program = syntax::parse_program("fn main() { let x: Int32 = \"text\" }").unwrap();
        let result = check_program(&program);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "expected Int32, found String"
        );
    }

    #[test]
    fn none_allows_free_inference() {
        let program =
            syntax::parse_program("fn main() { let a = 42; let b = 3.14; let c = \"hello\"; c }")
                .unwrap();
        let types = check_program(&program).unwrap();

        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected a block");
        };
        let crate::syntax::ExprKind::Let { value: a, .. } = &expressions[0].kind else {
            panic!("expected a let expression");
        };
        let crate::syntax::ExprKind::Let { value: b, .. } = &expressions[1].kind else {
            panic!("expected a let expression");
        };
        let crate::syntax::ExprKind::Let { value: c, .. } = &expressions[2].kind else {
            panic!("expected a let expression");
        };

        assert_eq!(types.get(a), Type::I32);
        assert_eq!(types.get(b), Type::F64);
        assert_eq!(types.get(c), Type::String);
    }

    #[test]
    fn binary_ops_use_hint_for_literals() {
        // Binary operations: literals are hint-inferred from the other side's type
        let program =
            syntax::parse_program("fn main() { let x: Int64 = 100; let y = 42 + x; y }").unwrap();
        let types = check_program(&program).unwrap();

        let crate::syntax::ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("expected a block");
        };
        let crate::syntax::ExprKind::Let { value, .. } = &expressions[1].kind else {
            panic!("expected a let expression");
        };

        // The whole expression is Int64
        assert_eq!(types.get(value), Type::I64);
    }

    #[test]
    fn if_branches_must_match() {
        // if expressions: the then branch determines the type, the else branch must match
        let program = syntax::parse_program("fn main() { if true 42 else 100 }").unwrap();
        assert!(check_program(&program).is_ok());

        // A type mismatch reports an error
        let program = syntax::parse_program("fn main() { if true 42 else \"text\" }").unwrap();
        assert!(check_program(&program).is_err());
    }
}
