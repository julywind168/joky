//! Visitor usage examples

#[cfg(test)]
mod tests {
    use crate::syntax::{parse_program, walk_expr, ExprVisitor};

    /// Expression counter - counts the number of expressions in the AST
    struct ExprCounter {
        count: usize,
    }

    impl ExprVisitor for ExprCounter {
        type Output = ();

        fn visit_expr(&mut self, expr: &crate::syntax::Expr) {
            self.count += 1;
            walk_expr(self, expr);
        }

        fn default_output(&self) -> Self::Output {}
    }

    #[test]
    fn visitor_counts_expressions() {
        let program = parse_program("fn main() { let x = 1 + 2; x }").unwrap();
        let mut counter = ExprCounter { count: 0 };

        for function in &program.functions {
            counter.visit_expr(&function.body);
        }

        // block(1) + let(1) + binary(1) + integers(2) + name(1) = 6
        assert_eq!(counter.count, 6);
    }

    /// Literal collector - collects all integer literals
    struct IntegerCollector {
        integers: Vec<u64>,
    }

    impl ExprVisitor for IntegerCollector {
        type Output = ();

        fn visit_expr(&mut self, expr: &crate::syntax::Expr) {
            walk_expr(self, expr);
        }

        fn visit_integer(&mut self, value: u64, _expr: &crate::syntax::Expr) {
            self.integers.push(value);
        }

        fn default_output(&self) -> Self::Output {}
    }

    #[test]
    fn visitor_collects_integers() {
        let program = parse_program("fn main() { let x = 1 + 2 * 3; x }").unwrap();
        let mut collector = IntegerCollector {
            integers: Vec::new(),
        };

        for function in &program.functions {
            collector.visit_expr(&function.body);
        }

        assert_eq!(collector.integers, vec![1, 2, 3]);
    }
}
