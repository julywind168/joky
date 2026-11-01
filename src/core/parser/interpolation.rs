//! Expand interpolation into ordinary builder operations with hygienic locals.
use super::*;
use lexer::InterpolationPart;

impl Parser {
    pub(super) fn parse_interpolation(
        &mut self,
        parts: Vec<InterpolationPart>,
        span: Span,
    ) -> Result<Expr, Diagnostic> {
        self.interpolation = true;
        let builder = format!("$interpolation_builder_{}", span.start());
        let ident = |name: &str, span| Expr::Ident {
            name: name.into(),
            span,
        };
        let mut exprs = vec![Expr::Var {
            name: builder.clone(),
            annotation: None,
            value: Box::new(Expr::Call {
                callee: "$joky_string.StringBuilder.new".into(),
                type_args: vec![],
                args: vec![],
                span,
            }),
            span,
        }];
        for part in parts {
            let value = match part {
                InterpolationPart::Text(text, text_span) => {
                    if text.is_empty() {
                        continue;
                    }
                    Expr::Literal {
                        literal: Literal::String(text),
                        span: text_span.with_source(self.source),
                    }
                }
                InterpolationPart::Expression(mut tokens) => {
                    for token in &mut tokens {
                        token.span = token.span.with_source(self.source);
                    }
                    let mut parser = Parser::new(tokens);
                    parser.type_params = self.type_params.clone();
                    parser.impl_target = self.impl_target.clone();
                    parser.impl_trait = self.impl_trait.clone();
                    if parser.peek() == &TokenKind::Eof {
                        return Err(parser.error("Interpolation requires an expression".into()));
                    }
                    let value = parser.parse_expr()?;
                    self.durations |= parser.durations;
                    self.arrays |= parser.arrays;
                    self.iterators |= parser.iterators;
                    if parser.peek() != &TokenKind::Eof {
                        return Err(parser.error("Expected end of interpolation expression; format specifiers are not supported".into()));
                    }
                    value
                }
            };
            let value_span = value.span();
            exprs.push(Expr::Call {
                callee: "$interpolation_append".into(),
                type_args: vec![],
                args: vec![
                    Expr::Unary {
                        op: UnaryOp::Borrow,
                        operand: Box::new(value),
                        span: value_span,
                    },
                    Expr::Unary {
                        op: UnaryOp::BorrowMut,
                        operand: Box::new(ident(&builder, span)),
                        span,
                    },
                ],
                span: value_span,
            });
        }
        exprs.push(Expr::MethodCall(Box::new(MethodCall {
            receiver: ident(&builder, span),
            name: "finish".into(),
            type_args: vec![],
            args: vec![],
            span,
        })));
        Ok(Expr::Block { exprs, span })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_lexes_nested_braces_strings_comments_and_utf8_spans() {
        let source = "fn text() -> String { \"界 {({ let x = \\\"quoted\\\"; x })}\" }";
        // Ordinary quotes inside an expression need no outer-string escaping.
        let source = source.replace("\\\"", "\"");
        let id = crate::SourceId::from_raw(7);
        let module = super::super::parse_with_source(&source, id).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!("Expected function")
        };
        assert_eq!(function.body.span().source_id(), id);
        assert!(module
            .imports
            .iter()
            .any(|import| import.alias.as_deref() == Some("$joky_show")));
        assert!(super::super::parse(
            "fn text() -> String { \"{({ let x = 42 // } in comment\n x })}\" }"
        )
        .is_ok());
        assert!(super::super::parse("fn text() -> String { \"{ \"{42}\" }\" }").is_ok());
    }

    #[test]
    fn interpolation_bounds_nested_strings_and_rejects_bad_delimiters() {
        for source in [
            "fn text() -> String { \"}\" }",
            "fn text() -> String { \"{}\" }",
            "fn text() -> String { \"{42:02}\" }",
            "fn text() -> String { \"{42",
            "fn text() -> String { f\"{42}\" }",
        ] {
            assert!(super::super::parse(source).is_err(), "{source}");
        }
        let mut value = "42".to_owned();
        for _ in 0..65 {
            value = format!("\"{{{value}}}\"");
        }
        let error = super::super::parse(&format!("fn text() -> String {{ {value} }}")).unwrap_err();
        assert!(error.message.contains("64 levels"));
    }
}
