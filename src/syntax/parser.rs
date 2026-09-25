use crate::diagnostic::ParseError;
use crate::{Diagnostic, Span};

use super::ast::{
    BinaryOp, CallArgument, Class, ClassField, CollectionLiteral, Constant, Effect, EffectMode,
    EffectOperation, Enum, EnumVariant, Expr, ExprKind, FieldAccess, ForeignFunction, Function,
    HandlerArm, Impl, ImplType, Import, IntrinsicMethod, IntrinsicType, IntrinsicTypeKind,
    MapLiteralEntry, MatchArm, NodeIdGenerator, Parameter, Pattern, PatternField, Program,
    ReceiverMode, Struct, StructField, Trait, TraitMethod, TraitType, TypeAnnotation, TypeArgument,
    TypeExpr, TypeParameter, UnaryOp, Visibility, WherePredicate,
};
use super::lexer::lex;
use super::token::{StringLiteral, Token, TokenKind};

mod compound;
mod control;
mod expression;
mod handlers;
mod patterns;
mod tokens;
mod types;

pub fn parse_program(source: &str) -> Result<Program, Diagnostic> {
    parse_program_named(source, "<source>")
}

pub(crate) fn parse_program_named(source: &str, path: &str) -> Result<Program, Diagnostic> {
    Parser::new(lex(source).map_err(Diagnostic::from)?, source, path)
        .parse_program()
        .map_err(Diagnostic::from)
}

pub(crate) fn parse_program_at(source: &str, offset: u32) -> Result<Program, Diagnostic> {
    let mut tokens = lex(source).map_err(Diagnostic::from)?;
    for token in &mut tokens {
        token.span = Span::new(
            token.span.start() + offset as usize,
            token.span.end() + offset as usize,
        );
        // String offset tables record source positions like spans, so they must
        // shift together
        if let TokenKind::String(literal) = &mut token.kind {
            for source_offset in &mut literal.source_offsets {
                *source_offset += offset as usize;
            }
        }
    }
    let mut parser = Parser::new(tokens, source, "<prelude>");
    parser.source_offset = offset as usize;
    parser.id_gen = NodeIdGenerator::starting_at(offset);
    parser.parse_program().map_err(Diagnostic::from)
}

struct Parser {
    source: String,
    source_path: String,
    source_offset: usize,
    tokens: Vec<Token>,
    position: usize,
    id_gen: NodeIdGenerator,
    /// Depth of expression positions where a following `{` is a structural
    /// block (if/while/match/for), not a `||`-omitted trailing closure.
    suppress_trailing_closure: usize,
    /// The current concurrent arm ends at a bare `|`. Nested expressions,
    /// including parenthesized bitwise or, still accept the operator.
    stop_at_arm_pipe: bool,
    /// Recursive `parse_expression` depth, used to diagnose pathological nesting.
    expr_depth: usize,
    pending_foreign: Vec<ForeignFunction>,
}

impl Parser {
    fn new(tokens: Vec<Token>, source: &str, path: &str) -> Self {
        Self {
            source: source.to_owned(),
            source_path: path.to_owned(),
            source_offset: 0,
            tokens,
            position: 0,
            id_gen: NodeIdGenerator::new(),
            suppress_trailing_closure: 0,
            stop_at_arm_pipe: false,
            expr_depth: 0,
            pending_foreign: Vec::new(),
        }
    }

    /// Parse one expression with trailing closures disabled. `if cond { .. }`,
    /// `while cond { .. }`, `for x in xs { .. }`, and `match value { .. }`
    /// place a structural `{` right after the parsed expression.
    fn parse_expression_structural(&mut self) -> Result<Expr, ParseError> {
        self.suppress_trailing_closure += 1;
        let expression = self.parse_expression(0);
        self.suppress_trailing_closure -= 1;
        expression
    }

    fn parse_program(mut self) -> Result<Program, ParseError> {
        if self.tokens.is_empty() {
            return Err(ParseError::EmptyProgram);
        }

        let mut imports = Vec::new();
        let mut constants = Vec::new();
        let mut traits = Vec::new();
        let mut impls = Vec::new();
        let mut structs = Vec::new();
        let mut classes = Vec::new();
        let mut enums = Vec::new();
        let mut effects = Vec::new();
        let mut intrinsic_types = Vec::new();
        let mut functions = Vec::new();
        while self.peek().is_some() {
            self.skip_newlines();
            if self.peek().is_none() {
                break;
            }
            match self.peek().map(|token| &token.kind) {
                Some(TokenKind::At) if self.is_extern_annotation() => {
                    while self.is_extern_annotation() {
                        self.parse_extern_annotation()?;
                        self.skip_newlines();
                    }
                    let visibility = if self.match_token(TokenKind::Pub) {
                        Visibility::Public
                    } else {
                        Visibility::Private
                    };
                    functions.push(self.parse_function(visibility)?);
                }
                Some(TokenKind::At) if self.is_repr_c_annotation() => {
                    self.parse_repr_c_annotation()?;
                    structs.push(self.parse_struct_with_repr(true)?);
                }
                Some(TokenKind::At) => intrinsic_types.push(self.parse_intrinsic_declaration()?),
                Some(TokenKind::Import) => imports.push(self.parse_import()?),
                Some(TokenKind::Const) => constants.push(self.parse_constant(Visibility::Private)?),
                Some(TokenKind::Trait) => traits.push(self.parse_trait()?),
                Some(TokenKind::Impl) => impls.push(self.parse_impl()?),
                Some(TokenKind::Struct) => structs.push(self.parse_struct()?),
                Some(TokenKind::Class) => classes.push(self.parse_class()?),
                Some(TokenKind::Enum) => enums.push(self.parse_enum()?),
                Some(TokenKind::Eff) => effects.push(self.parse_effect()?),
                Some(TokenKind::Identifier(name)) if name == "type" => {
                    self.parse_type_declaration()?
                }
                Some(TokenKind::Pub) => {
                    self.advance();
                    if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Const)) {
                        constants.push(self.parse_constant(Visibility::Public)?);
                    } else if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Identifier(name)) if name == "type")
                    {
                        self.parse_type_declaration()?;
                    } else {
                        functions.push(self.parse_function(Visibility::Public)?);
                    }
                }
                _ => functions.push(self.parse_function(Visibility::Private)?),
            }
        }
        Ok(Program {
            imports,
            constants,
            traits,
            impls,
            structs,
            classes,
            enums,
            effects,
            intrinsic_types,
            functions,
        })
    }

    fn parse_constant(&mut self, visibility: Visibility) -> Result<Constant, ParseError> {
        let start = self
            .expect_simple(TokenKind::Const, "expected 'const'")?
            .span;
        let name = match self.advance() {
            Some(Token {
                kind: TokenKind::Identifier(name),
                ..
            }) => name,
            Some(token) => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected a constant name".to_owned(),
                    span: Some(token.span),
                })
            }
            None => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected a constant name".to_owned(),
                    span: Some(start),
                })
            }
        };
        let annotation = if self.match_token(TokenKind::Colon) {
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Equal, "expected '=' after constant name")?;
        let value = self.parse_expression(0)?;
        Ok(Constant {
            name,
            visibility,
            annotation,
            span: start.merge(value.span),
            value,
        })
    }

    fn parse_effect(&mut self) -> Result<Effect, ParseError> {
        let start = self.expect_simple(TokenKind::Eff, "expected 'eff'")?.span;
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected effect name".to_owned(),
            span: Some(start),
        })?;
        let TokenKind::Identifier(name) = name_token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected effect name".to_owned(),
                span: Some(name_token.span),
            });
        };
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after effect name")?;
        let mut operations = Vec::new();
        loop {
            self.skip_newlines();
            if self.match_token(TokenKind::RightBrace) {
                break;
            }
            operations.push(self.parse_effect_operation()?);
            self.consume_optional_member_separator();
        }
        let end = self.previous_span().unwrap_or(name_token.span);
        Ok(Effect {
            name,
            operations,
            span: start.merge(end),
        })
    }

    fn parse_effect_operation(&mut self) -> Result<EffectOperation, ParseError> {
        let (mode, suspends) = self.parse_effect_mode_annotations()?;
        let start = self
            .expect_simple(TokenKind::Fn, "expected 'fn' in effect")?
            .span;
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected effect operation name".to_owned(),
            span: Some(start),
        })?;
        let TokenKind::Identifier(name) = name_token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected effect operation name".to_owned(),
                span: Some(name_token.span),
            });
        };
        self.expect_simple(
            TokenKind::LeftParen,
            "expected '(' after effect operation name",
        )?;
        let parameters = self.parse_parameters()?;
        self.expect_simple(
            TokenKind::RightParen,
            "expected ')' after effect parameters",
        )?;
        self.expect_simple(TokenKind::Arrow, "expected '->' after effect parameters")?;
        let return_type = self.parse_return_type()?;
        let end = self.previous_span().unwrap_or(return_type.span);
        Ok(EffectOperation {
            name,
            parameters,
            return_type,
            mode,
            suspends,
            span: start.merge(end),
        })
    }

    fn parse_effect_mode_annotations(&mut self) -> Result<(EffectMode, bool), ParseError> {
        let mut mode = EffectMode::Normal;
        let mut suspends = false;
        while self.match_token(TokenKind::At) {
            let annotation = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected effect operation annotation".to_owned(),
                span: self.previous_span(),
            })?;
            let TokenKind::Identifier(name) = annotation.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected effect operation annotation".to_owned(),
                    span: Some(annotation.span),
                });
            };
            match name.as_str() {
                "suspends" if !suspends => suspends = true,
                "resumable" if mode == EffectMode::Normal => mode = EffectMode::Resumable,
                "aborts" if mode == EffectMode::Normal => mode = EffectMode::Aborts,
                "suspends" | "resumable" | "aborts" => {
                    return Err(ParseError::ExpectedToken {
                        expected: "effect operation annotations must be unique and have at most one of @resumable or @aborts".to_owned(),
                        span: Some(annotation.span),
                    })
                }
                _ => {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected @suspends, @resumable, or @aborts".to_owned(),
                        span: Some(annotation.span),
                    })
                }
            }
        }
        if mode == EffectMode::Aborts && suspends {
            return Err(ParseError::ExpectedToken {
                expected: "@suspends can only combine with the normal or resumable control mode"
                    .to_owned(),
                span: self.previous_span(),
            });
        }
        Ok((mode, suspends))
    }

    fn parse_type_declaration(&mut self) -> Result<(), ParseError> {
        let type_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected 'type'".to_owned(),
            span: self.previous_span(),
        })?;
        if !matches!(type_token.kind, TokenKind::Identifier(name) if name == "type") {
            return Err(ParseError::ExpectedToken {
                expected: "expected 'type'".to_owned(),
                span: Some(type_token.span),
            });
        }
        let name = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected type name".to_owned(),
            span: self.previous_span(),
        })?;
        if !matches!(name.kind, TokenKind::Identifier(_)) {
            return Err(ParseError::ExpectedToken {
                expected: "expected type name".to_owned(),
                span: Some(name.span),
            });
        }
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::LeftParen)
        ) {
            self.parse_inline_type_parameters()?;
        }
        Ok(())
    }

    fn parse_inline_type_parameters(&mut self) -> Result<Vec<TypeParameter>, ParseError> {
        self.expect_simple(TokenKind::LeftParen, "expected '('")?;
        let mut parameters = Vec::new();
        if self.match_token(TokenKind::RightParen) {
            return Ok(parameters);
        }
        loop {
            let parameter = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected type parameter name".to_owned(),
                span: self.previous_span(),
            })?;
            let span = parameter.span;
            let TokenKind::Identifier(name) = parameter.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected type parameter name".to_owned(),
                    span: Some(span),
                });
            };
            self.expect_simple(TokenKind::Colon, "expected ':' after type parameter name")?;
            let type_marker = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected 'type' after type parameter name".to_owned(),
                span: Some(span),
            })?;
            if !matches!(type_marker.kind, TokenKind::Identifier(marker) if marker == "type") {
                return Err(ParseError::ExpectedToken {
                    expected: "expected 'type' after type parameter name".to_owned(),
                    span: Some(type_marker.span),
                });
            }
            let mut bounds = Vec::new();
            while self.match_token(TokenKind::Plus) {
                bounds.push(self.parse_type_annotation()?);
            }
            parameters.push(TypeParameter { name, bounds, span });
            if !self.match_token(TokenKind::Comma) {
                break;
            }
        }
        self.expect_simple(TokenKind::RightParen, "expected ')' after type parameters")?;
        Ok(parameters)
    }

    /// Parse a compiler-provided type declaration.
    ///
    /// Intrinsic declarations describe an ABI-backed type to the source
    /// language, but are not ordinary user-defined structs or classes. They are consumed
    /// here and resolved by the corresponding compiler builtin.
    fn parse_intrinsic_declaration(&mut self) -> Result<IntrinsicType, ParseError> {
        self.expect_simple(TokenKind::At, "expected '@'")?;
        let annotation = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected intrinsic annotation name".to_owned(),
            span: self.previous_span(),
        })?;
        if !matches!(annotation.kind, TokenKind::Identifier(name) if name == "intrinsic") {
            return Err(ParseError::ExpectedToken {
                expected: "expected '@intrinsic' annotation".to_owned(),
                span: Some(annotation.span),
            });
        }
        let effect = if self.match_token(TokenKind::LeftParen) {
            let key = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected 'effect' intrinsic binding".into(),
                span: self.previous_span(),
            })?;
            if !matches!(key.kind, TokenKind::Identifier(ref name) if name == "effect") {
                return Err(ParseError::ExpectedToken {
                    expected: "expected 'effect' intrinsic binding".into(),
                    span: Some(key.span),
                });
            }
            self.expect_simple(TokenKind::Equal, "expected '=' after 'effect'")?;
            let value = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected effect name".into(),
                span: self.previous_span(),
            })?;
            let TokenKind::Identifier(name) = value.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected effect name".into(),
                    span: Some(value.span),
                });
            };
            self.expect_simple(TokenKind::RightParen, "expected ')' after effect binding")?;
            Some(name)
        } else {
            None
        };
        self.match_token(TokenKind::Pub);
        self.parse_intrinsic_type(effect)
    }

    fn parse_intrinsic_type(
        &mut self,
        effect: Option<String>,
    ) -> Result<IntrinsicType, ParseError> {
        let start = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected 'struct' or 'class' after '@intrinsic'".into(),
            span: self.previous_span(),
        })?;
        let kind = match start.kind {
            TokenKind::Struct => IntrinsicTypeKind::Struct,
            TokenKind::Class => IntrinsicTypeKind::Class,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected 'struct' or 'class' after '@intrinsic'".into(),
                    span: Some(start.span),
                })
            }
        };
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected intrinsic type name".to_owned(),
            span: Some(start.span),
        })?;
        let name = match name_token.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected intrinsic type name".to_owned(),
                    span: Some(name_token.span),
                })
            }
        };
        let type_parameters = if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::LeftParen)
        ) {
            self.parse_inline_type_parameters()?
        } else {
            Vec::new()
        };
        self.expect_simple(
            TokenKind::LeftBrace,
            "expected '{' after intrinsic type name",
        )?;
        self.skip_newlines();
        let mut methods = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            methods.push(self.parse_intrinsic_method()?);
            self.consume_optional_member_separator();
            self.skip_newlines();
        }
        self.expect_simple(
            TokenKind::RightBrace,
            "expected '}' after intrinsic type body",
        )?;
        Ok(IntrinsicType {
            name,
            kind,
            effect,
            type_parameters,
            methods,
            span: start.span.merge(self.previous_span().unwrap_or(start.span)),
        })
    }

    fn parse_intrinsic_method(&mut self) -> Result<IntrinsicMethod, ParseError> {
        let start = self
            .expect_simple(TokenKind::Fn, "expected 'fn' in intrinsic type")?
            .span;
        let method = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected intrinsic method name".to_owned(),
            span: self.previous_span(),
        })?;
        let method_name = match method.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected intrinsic method name".to_owned(),
                    span: Some(method.span),
                })
            }
        };
        self.expect_simple(
            TokenKind::LeftParen,
            "expected '(' after intrinsic method name",
        )?;
        let receiver_mode = if self.match_token(TokenKind::Ampersand) {
            ReceiverMode::Borrowed
        } else {
            ReceiverMode::Owned
        };
        let receiver = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected 'self' intrinsic receiver".to_owned(),
            span: self.previous_span(),
        })?;
        if !matches!(receiver.kind, TokenKind::Identifier(ref name) if name == "self") {
            return Err(ParseError::ExpectedToken {
                expected: "expected 'self' intrinsic receiver".to_owned(),
                span: Some(receiver.span),
            });
        }
        let (parameters, type_parameters) = if self.match_token(TokenKind::Comma) {
            self.parse_function_parameters()?
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect_simple(
            TokenKind::RightParen,
            "expected ')' after intrinsic parameters",
        )?;
        self.expect_simple(TokenKind::Arrow, "expected '->' after intrinsic receiver")?;
        let return_type = self.parse_return_type()?;
        let where_predicates = self.parse_where_predicates()?;
        let end = where_predicates.last().map_or(return_type.span, |p| p.span);
        let operation = if matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Identifier(word)) if word == "as")
        {
            self.advance();
            let token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected effect operation alias".into(),
                span: Some(end),
            })?;
            let TokenKind::Identifier(name) = token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected effect operation alias".into(),
                    span: Some(token.span),
                });
            };
            Some(name)
        } else {
            None
        };
        Ok(IntrinsicMethod {
            name: method_name,
            operation,
            receiver_mode,
            type_parameters,
            parameters,
            return_type,
            where_predicates,
            span: start.merge(end),
        })
    }

    fn parse_where_predicates(&mut self) -> Result<Vec<WherePredicate>, ParseError> {
        let saved = self.position;
        self.skip_newlines();
        if !matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Identifier(word)) if word == "where")
        {
            self.position = saved;
            return Ok(Vec::new());
        }
        self.advance();
        let mut predicates = Vec::new();
        loop {
            self.skip_newlines();
            let token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected type parameter after 'where' or ','".into(),
                span: self.previous_span(),
            })?;
            let TokenKind::Identifier(parameter) = token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected type parameter in where clause".into(),
                    span: Some(token.span),
                });
            };
            let associated = if self.match_token(TokenKind::Dot) {
                let associated = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected associated type in where clause".into(),
                    span: self.previous_span(),
                })?;
                let TokenKind::Identifier(name) = associated.kind else {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected associated type in where clause".into(),
                        span: Some(associated.span),
                    });
                };
                Some(name)
            } else {
                None
            };
            self.expect_simple(TokenKind::Colon, "expected ':' in where clause")?;
            self.skip_newlines();
            let mut bounds = vec![self.parse_type_annotation()?];
            while self.match_token(TokenKind::Plus) {
                self.skip_newlines();
                bounds.push(self.parse_type_annotation()?);
            }
            let span = token.span.merge(bounds.last().unwrap().span);
            predicates.push(WherePredicate {
                parameter,
                associated,
                bounds,
                span,
            });
            if !self.match_token(TokenKind::Comma) {
                break;
            }
        }
        Ok(predicates)
    }

    fn parse_trait(&mut self) -> Result<Trait, ParseError> {
        let start = self
            .expect_simple(TokenKind::Trait, "expected 'trait'")?
            .span;
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected trait name".to_owned(),
            span: Some(start),
        })?;
        let TokenKind::Identifier(name) = name_token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected trait name".to_owned(),
                span: Some(name_token.span),
            });
        };
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after trait name")?;
        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            if self.match_token(TokenKind::RightBrace) {
                break;
            }
            if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Identifier(name)) if name == "type")
            {
                let type_start = self.advance().expect("peeked type").span;
                let type_token = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected associated type name".to_owned(),
                    span: Some(type_start),
                })?;
                let TokenKind::Identifier(type_name) = type_token.kind else {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected associated type name".to_owned(),
                        span: Some(type_token.span),
                    });
                };
                associated_types.push(TraitType {
                    name: type_name,
                    span: type_start.merge(type_token.span),
                });
                self.consume_optional_member_separator();
                continue;
            }
            let method_start = self
                .expect_simple(TokenKind::Fn, "expected 'fn' in trait")?
                .span;
            let method_token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected trait method name".to_owned(),
                span: Some(method_start),
            })?;
            let TokenKind::Identifier(method_name) = method_token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected trait method name".to_owned(),
                    span: Some(method_token.span),
                });
            };
            self.expect_simple(TokenKind::LeftParen, "expected '(' after trait method name")?;
            let has_receiver = matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Ampersand))
                || matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Identifier(name)) if name == "self");
            let (receiver_mode, parameters) = if has_receiver {
                let receiver_mode = if self.match_token(TokenKind::Ampersand) {
                    ReceiverMode::Borrowed
                } else {
                    ReceiverMode::Owned
                };
                let receiver = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected 'self' trait receiver".to_owned(),
                    span: self.previous_span(),
                })?;
                if !matches!(receiver.kind, TokenKind::Identifier(ref value) if value == "self") {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected 'self' trait receiver".to_owned(),
                        span: Some(receiver.span),
                    });
                }
                if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Colon)) {
                    return Err(ParseError::ExpectedToken {
                        expected: "the 'self' receiver cannot have a type annotation".to_owned(),
                        span: Some(receiver.span),
                    });
                }
                let parameters = if self.match_token(TokenKind::Comma) {
                    self.parse_parameters()?
                } else {
                    Vec::new()
                };
                (receiver_mode, parameters)
            } else {
                (ReceiverMode::Static, self.parse_parameters()?)
            };
            self.expect_simple(TokenKind::RightParen, "expected ')' after trait parameters")?;
            self.expect_simple(TokenKind::Arrow, "expected '->' after trait receiver")?;
            let return_type = self.parse_return_type()?;
            let effect_names = self.parse_effect_names()?;
            let end = self.previous_span().unwrap_or(return_type.span);
            methods.push(TraitMethod {
                name: method_name,
                receiver_mode,
                parameters,
                return_type,
                effect_names,
                span: method_start.merge(end),
            });
            self.consume_optional_member_separator();
        }
        let end = self.previous_span().unwrap_or(start);
        Ok(Trait {
            name,
            associated_types,
            methods,
            span: start.merge(end),
        })
    }

    fn parse_impl(&mut self) -> Result<Impl, ParseError> {
        let start = self.expect_simple(TokenKind::Impl, "expected 'impl'")?.span;
        let trait_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected trait name after 'impl'".to_owned(),
            span: Some(start),
        })?;
        let TokenKind::Identifier(mut trait_name) = trait_token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected trait name after 'impl'".to_owned(),
                span: Some(trait_token.span),
            });
        };
        if self.match_token(TokenKind::Dot) {
            let member = self.advance().ok_or_else(|| ParseError::ExpectedToken {
                expected: "expected imported trait name".into(),
                span: self.previous_span(),
            })?;
            let TokenKind::Identifier(name) = member.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected imported trait name".into(),
                    span: Some(member.span),
                });
            };
            trait_name = format!("{trait_name}.{name}");
        }
        self.expect_simple(TokenKind::For, "expected 'for' in impl")?;
        let type_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected type name after 'for'".to_owned(),
            span: self.previous_span(),
        })?;
        let TokenKind::Identifier(type_name) = type_token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected type name after 'for'".to_owned(),
                span: Some(type_token.span),
            });
        };
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after impl target")?;
        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            if self.match_token(TokenKind::RightBrace) {
                break;
            }
            if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Identifier(name)) if name == "type")
            {
                let type_start = self.advance().expect("peeked type").span;
                let type_token = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected associated type name".to_owned(),
                    span: Some(type_start),
                })?;
                let TokenKind::Identifier(type_name) = type_token.kind else {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected associated type name".to_owned(),
                        span: Some(type_token.span),
                    });
                };
                self.expect_simple(TokenKind::Equal, "expected '=' after associated type name")?;
                let ty = self.parse_type_annotation()?;
                associated_types.push(ImplType {
                    name: type_name,
                    span: type_start.merge(ty.span),
                    ty,
                });
                self.consume_optional_member_separator();
                continue;
            }
            methods.push(self.parse_function(Visibility::Private)?);
        }
        let end = self.previous_span().unwrap_or(type_token.span);
        Ok(Impl {
            trait_name,
            type_name,
            associated_types,
            methods,
            span: start.merge(end),
        })
    }

    fn parse_import(&mut self) -> Result<Import, ParseError> {
        let start = self
            .expect_simple(TokenKind::Import, "expected 'import'")?
            .span;
        let mut path = Vec::new();
        loop {
            let token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected module path component".to_owned(),
                span: Some(start),
            })?;
            let TokenKind::Identifier(component) = token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected module path component".to_owned(),
                    span: Some(token.span),
                });
            };
            path.push(component);
            if !self.match_token(TokenKind::Slash) {
                let end = self.previous_span().unwrap_or(token.span);
                self.consume_optional_member_separator();
                return Ok(Import {
                    path,
                    span: start.merge(end),
                });
            }
        }
    }

    fn parse_enum(&mut self) -> Result<Enum, ParseError> {
        let start = self.expect_simple(TokenKind::Enum, "expected 'enum'")?.span;
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected enum name".to_owned(),
            span: Some(start),
        })?;
        let TokenKind::Identifier(name) = name_token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected enum name".to_owned(),
                span: Some(name_token.span),
            });
        };
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after enum name")?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            let variant_token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected enum variant".to_owned(),
                span: self.previous_span(),
            })?;
            let TokenKind::Identifier(variant_name) = variant_token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected enum variant".to_owned(),
                    span: Some(variant_token.span),
                });
            };
            let fields = if self.match_token(TokenKind::LeftParen) {
                let fields = self.parse_parameters()?;
                self.expect_simple(TokenKind::RightParen, "expected ')' after variant fields")?;
                fields
            } else {
                Vec::new()
            };
            variants.push(EnumVariant {
                name: variant_name,
                fields,
                span: variant_token.span,
            });
            self.consume_member_separator()?;
            self.skip_newlines();
        }
        let end = self.expect_simple(TokenKind::RightBrace, "expected '}' after enum body")?;
        Ok(Enum {
            name,
            variants,
            span: start.merge(end.span),
        })
    }

    fn parse_class(&mut self) -> Result<Class, ParseError> {
        let start = self
            .expect_simple(TokenKind::Class, "expected 'class'")?
            .span;
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected class name".to_owned(),
            span: Some(start),
        })?;
        let name = match name_token.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected class name".to_owned(),
                    span: Some(name_token.span),
                });
            }
        };
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after class name")?;
        self.skip_newlines();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            match self.peek().map(|token| &token.kind) {
                Some(TokenKind::Fn) => {
                    methods.push(self.parse_function(Visibility::Private)?);
                    self.consume_optional_member_separator();
                }
                Some(TokenKind::Let | TokenKind::Var) => {
                    let field_token = self.advance().expect("class field token checked");
                    let mutable = matches!(field_token.kind, TokenKind::Var);
                    let name_token = self.advance().ok_or(ParseError::ExpectedToken {
                        expected: "expected class field name".to_owned(),
                        span: Some(field_token.span),
                    })?;
                    let field_name = match name_token.kind {
                        TokenKind::Identifier(name) => name,
                        _ => {
                            return Err(ParseError::ExpectedToken {
                                expected: "expected class field name".to_owned(),
                                span: Some(name_token.span),
                            });
                        }
                    };
                    self.expect_simple(TokenKind::Colon, "expected ':' after class field name")?;
                    let ty = self.parse_type_annotation()?;
                    let default = self.parse_optional_field_default()?;
                    fields.push(ClassField {
                        name: field_name,
                        mutable,
                        ty,
                        default,
                        span: field_token.span,
                    });
                    self.consume_member_separator()?;
                }
                _ => {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected a class field or method".to_owned(),
                        span: self.peek().map(|token| token.span),
                    });
                }
            }
            self.skip_newlines();
        }
        let end = self.expect_simple(TokenKind::RightBrace, "expected '}' after class body")?;
        Ok(Class {
            name,
            fields,
            methods,
            span: start.merge(end.span),
        })
    }

    fn parse_struct(&mut self) -> Result<Struct, ParseError> {
        self.parse_struct_with_repr(false)
    }

    fn parse_struct_with_repr(&mut self, repr_c: bool) -> Result<Struct, ParseError> {
        let start = self
            .expect_simple(TokenKind::Struct, "expected 'struct'")?
            .span;
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected struct name".to_owned(),
            span: Some(start),
        })?;
        let name = match name_token.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected struct name".to_owned(),
                    span: Some(name_token.span),
                });
            }
        };
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after struct name")?;
        self.skip_newlines();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Fn)) {
                methods.push(self.parse_function(Visibility::Private)?);
                self.consume_optional_member_separator();
                self.skip_newlines();
                continue;
            }
            let field_token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected 'let' before struct field".to_owned(),
                span: self.previous_span(),
            })?;
            if !matches!(field_token.kind, TokenKind::Let) {
                return Err(ParseError::ExpectedToken {
                    expected: "expected 'let' before struct field".to_owned(),
                    span: Some(field_token.span),
                });
            }
            let name_token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected struct field name".to_owned(),
                span: Some(field_token.span),
            })?;
            let TokenKind::Identifier(field_name) = name_token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected struct field name".to_owned(),
                    span: Some(name_token.span),
                });
            };
            self.expect_simple(TokenKind::Colon, "expected ':' after struct field name")?;
            let ty = self.parse_type_annotation()?;
            let default = self.parse_optional_field_default()?;
            fields.push(StructField {
                name: field_name,
                ty,
                default,
                span: field_token.span,
            });
            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::Comma | TokenKind::Semicolon)
            ) {
                self.advance();
                self.skip_newlines();
            } else if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::Newline)
            ) {
                self.skip_newlines();
            } else if !matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::RightBrace)
            ) {
                return Err(ParseError::ExpectedToken {
                    expected: "expected a newline, ',', ';', or '}' after struct field".to_owned(),
                    span: self.previous_span(),
                });
            }
        }
        let end = self.expect_simple(TokenKind::RightBrace, "expected '}' after struct fields")?;
        Ok(Struct {
            name,
            repr_c,
            fields,
            methods,
            span: start.merge(end.span),
        })
    }

    fn parse_optional_field_default(&mut self) -> Result<Option<Expr>, ParseError> {
        if !matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Equal)) {
            return Ok(None);
        }
        self.advance();
        Ok(Some(self.parse_expression(0)?))
    }

    fn parse_function(&mut self, visibility: Visibility) -> Result<Function, ParseError> {
        let mut foreign_bindings = std::mem::take(&mut self.pending_foreign).into_iter();
        let foreign = foreign_bindings.next().map(|mut first| {
            first.alternatives = foreign_bindings.collect();
            first
        });
        let start = self.expect_simple(TokenKind::Fn, "expected 'fn'")?.span;
        let name_token = self
            .advance()
            .ok_or(ParseError::ExpectedFunctionName { span: start })?;
        let name = match name_token.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedFunctionName {
                    span: name_token.span,
                });
            }
        };

        // Parse the parameter list
        self.expect_simple(TokenKind::LeftParen, "expected '('")?;
        self.skip_newlines();
        let receiver_mode = if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Ampersand)
        ) || matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Identifier(name)) if name == "self")
        {
            let mode = if self.match_token(TokenKind::Ampersand) {
                ReceiverMode::Borrowed
            } else {
                ReceiverMode::Owned
            };
            let token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected 'self' receiver".to_owned(),
                span: self.previous_span(),
            })?;
            if !matches!(token.kind, TokenKind::Identifier(ref name) if name == "self") {
                return Err(ParseError::ExpectedToken {
                    expected: "expected 'self' receiver".to_owned(),
                    span: Some(token.span),
                });
            }
            self.skip_newlines();
            if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Colon)) {
                return Err(ParseError::ExpectedToken {
                    expected: "the 'self' receiver cannot have a type annotation".to_owned(),
                    span: Some(token.span),
                });
            }
            if !matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::RightParen)
            ) {
                self.expect_simple(TokenKind::Comma, "expected ',' after receiver")?;
            }
            Some(mode)
        } else {
            None
        };
        let (parameters, inline_type_parameters) = self.parse_function_parameters()?;
        self.expect_simple(TokenKind::RightParen, "expected ')'")?;

        // Parse the optional return type
        let return_type = if self.match_token(TokenKind::Arrow) {
            Some(self.parse_return_type()?)
        } else {
            None
        };
        let effect_names = self.parse_effect_names()?;
        let where_predicates = self.parse_where_predicates()?;

        // `T: type` is the value-level spelling of a type parameter.  Keep
        // the AST canonical by moving these parameters into the existing
        // generic-parameter list; calls can then use `f(Int32, value)`.
        let type_parameters = if !inline_type_parameters.is_empty() {
            inline_type_parameters
        } else {
            infer_type_parameters(&parameters, return_type.as_ref())
        };

        let (foreign, body) = if let Some(mut foreign) = foreign {
            if foreign.library.is_empty() {
                self.expect_contextual_word("from")?;
                foreign.library =
                    self.parse_foreign_string("expected dynamic library name after 'from'")?;
                foreign.symbol = if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Identifier(word)) if word == "as")
                {
                    self.advance();
                    self.parse_foreign_string("expected C symbol name after 'as'")?
                } else {
                    name.clone()
                };
            }
            let end = self
                .expect_simple(
                    TokenKind::Semicolon,
                    "expected ';' after extern declaration",
                )?
                .span;
            (
                Some(foreign),
                Expr {
                    id: self.id_gen.next(),
                    span: end,
                    kind: ExprKind::Block(Vec::new()),
                },
            )
        } else {
            (None, self.parse_block()?)
        };

        Ok(Function {
            name,
            foreign,
            receiver_mode,
            visibility,
            type_parameters,
            parameters,
            return_type,
            effect_names,
            where_predicates,
            span: start.merge(body.span),
            body,
        })
    }

    fn is_extern_annotation(&self) -> bool {
        matches!(
            (self.tokens.get(self.position), self.tokens.get(self.position + 1)),
            (Some(Token { kind: TokenKind::At, .. }), Some(Token { kind: TokenKind::Identifier(name), .. })) if name == "extern"
        )
    }

    fn is_repr_c_annotation(&self) -> bool {
        matches!(
            (
                self.tokens.get(self.position),
                self.tokens.get(self.position + 1),
                self.tokens.get(self.position + 2),
                self.tokens.get(self.position + 3),
                self.tokens.get(self.position + 4)
            ),
            (
                Some(Token { kind: TokenKind::At, .. }),
                Some(Token { kind: TokenKind::Identifier(name), .. }),
                Some(Token { kind: TokenKind::LeftParen, .. }),
                Some(Token { kind: TokenKind::Identifier(abi), .. }),
                Some(Token { kind: TokenKind::RightParen, .. })
            ) if name == "repr" && abi == "c"
        )
    }

    fn parse_repr_c_annotation(&mut self) -> Result<(), ParseError> {
        self.expect_simple(TokenKind::At, "expected '@'")?;
        self.expect_contextual_word("repr")?;
        self.expect_simple(TokenKind::LeftParen, "expected '(' after '@repr'")?;
        self.expect_contextual_word("c")?;
        self.expect_simple(TokenKind::RightParen, "expected ')' after '@repr(c)'")?;
        Ok(())
    }

    fn parse_extern_annotation(&mut self) -> Result<(), ParseError> {
        self.expect_simple(TokenKind::At, "expected '@'")?;
        let name = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected extern".into(),
            span: self.previous_span(),
        })?;
        if !matches!(name.kind, TokenKind::Identifier(value) if value == "extern") {
            return Err(ParseError::ExpectedToken {
                expected: "expected '@extern'".into(),
                span: Some(name.span),
            });
        }
        self.expect_simple(TokenKind::LeftParen, "expected '(' after '@extern'")?;
        let abi = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected C ABI".into(),
            span: self.previous_span(),
        })?;
        if !matches!(abi.kind, TokenKind::Identifier(value) if value == "c") {
            return Err(ParseError::ExpectedToken {
                expected: "only '@extern(c, ...)'' is supported".into(),
                span: Some(abi.span),
            });
        }
        self.expect_simple(TokenKind::Comma, "expected ',' after C ABI")?;
        let library = self.parse_foreign_string("expected dynamic library name")?;
        self.expect_simple(TokenKind::Comma, "expected ',' after library name")?;
        let symbol = self.parse_foreign_string("expected C symbol name")?;
        let target_os = if self.match_token(TokenKind::Comma) {
            let key = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected 'os'".into(),
                span: self.previous_span(),
            })?;
            if !matches!(key.kind, TokenKind::Identifier(ref value) if value == "os") {
                return Err(ParseError::ExpectedToken {
                    expected: "expected 'os'".into(),
                    span: Some(key.span),
                });
            }
            self.expect_simple(TokenKind::Equal, "expected '=' after 'os'")?;
            Some(self.parse_foreign_string("expected target operating system")?)
        } else {
            None
        };
        self.expect_simple(TokenKind::RightParen, "expected ')' after '@extern'")?;
        self.pending_foreign.push(ForeignFunction {
            library,
            symbol,
            target_os,
            alternatives: Vec::new(),
        });
        Ok(())
    }

    fn expect_contextual_word(&mut self, word: &str) -> Result<(), ParseError> {
        let token = self.advance().ok_or_else(|| ParseError::ExpectedToken {
            expected: format!("expected '{word}'"),
            span: self.previous_span(),
        })?;
        if matches!(token.kind, TokenKind::Identifier(ref name) if name == word) {
            Ok(())
        } else {
            Err(ParseError::ExpectedToken {
                expected: format!("expected '{word}'"),
                span: Some(token.span),
            })
        }
    }

    fn parse_foreign_string(&mut self, expected: &str) -> Result<String, ParseError> {
        let token = self.advance().ok_or_else(|| ParseError::ExpectedToken {
            expected: expected.into(),
            span: self.previous_span(),
        })?;
        match token.kind {
            TokenKind::String(literal)
                if !literal.value.is_empty() && !literal.value.contains('\0') =>
            {
                Ok(literal.value)
            }
            _ => Err(ParseError::ExpectedToken {
                expected: expected.into(),
                span: Some(token.span),
            }),
        }
    }

    fn parse_effect_names(&mut self) -> Result<Vec<TypeAnnotation>, ParseError> {
        if !self.match_token(TokenKind::Effects) {
            return Ok(Vec::new());
        }
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after 'effects'")?;
        let mut names = Vec::new();
        self.skip_newlines();
        if self.match_token(TokenKind::RightBrace) {
            return Ok(names);
        }
        loop {
            names.push(self.parse_type_annotation()?);
            if !self.match_token(TokenKind::Comma) {
                break;
            }
        }
        self.expect_simple(TokenKind::RightBrace, "expected '}' after effects")?;
        Ok(names)
    }

    fn parse_parameters(&mut self) -> Result<Vec<Parameter>, ParseError> {
        let mut parameters = Vec::new();

        // Empty parameter list
        if matches!(self.peek().map(|t| &t.kind), Some(TokenKind::RightParen)) {
            return Ok(parameters);
        }

        loop {
            let name_token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected parameter name".to_owned(),
                span: self.previous_span(),
            })?;
            let name = match &name_token.kind {
                TokenKind::Identifier(name) => name.clone(),
                _ => {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected parameter name".to_owned(),
                        span: Some(name_token.span),
                    });
                }
            };

            self.expect_simple(TokenKind::Colon, "expected ':' after parameter name")?;
            let borrowed = self.match_token(TokenKind::Ampersand);
            let ty = self.parse_type_annotation()?;

            parameters.push(Parameter {
                name,
                borrowed,
                ty,
                span: name_token.span,
            });

            // Check whether there are more parameters
            if !self.match_token(TokenKind::Comma) {
                break;
            }
        }

        Ok(parameters)
    }

    fn parse_function_parameters(
        &mut self,
    ) -> Result<(Vec<Parameter>, Vec<TypeParameter>), ParseError> {
        let mut parameters = Vec::new();
        let mut type_parameters = Vec::new();
        let mut runtime_parameter_seen = false;

        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightParen)
        ) {
            return Ok((parameters, type_parameters));
        }

        loop {
            let name_token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected parameter name".to_owned(),
                span: self.previous_span(),
            })?;
            let span = name_token.span;
            let TokenKind::Identifier(name) = name_token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected parameter name".to_owned(),
                    span: Some(span),
                });
            };
            self.expect_simple(TokenKind::Colon, "expected ':' after parameter name")?;
            let borrowed = self.match_token(TokenKind::Ampersand);
            let ty = self.parse_type_annotation()?;
            if ty.is_name("type") {
                if runtime_parameter_seen {
                    return Err(ParseError::ExpectedToken {
                        expected: "type parameters must precede runtime parameters".to_owned(),
                        span: Some(span),
                    });
                }
                let mut bounds = Vec::new();
                while self.match_token(TokenKind::Plus) {
                    bounds.push(self.parse_type_annotation()?);
                }
                type_parameters.push(TypeParameter { name, bounds, span });
            } else {
                runtime_parameter_seen = true;
                parameters.push(Parameter {
                    name,
                    borrowed,
                    ty,
                    span,
                });
            }
            if !self.match_token(TokenKind::Comma) {
                break;
            }
        }
        Ok((parameters, type_parameters))
    }

    fn consume_member_separator(&mut self) -> Result<(), ParseError> {
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Comma | TokenKind::Semicolon)
        ) {
            self.advance();
            return Ok(());
        }
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Newline | TokenKind::RightBrace)
        ) {
            return Ok(());
        }
        Err(ParseError::ExpectedToken {
            expected: "expected a newline, ',', ';', or '}' after class field".to_owned(),
            span: self.previous_span(),
        })
    }

    fn consume_optional_member_separator(&mut self) {
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Comma | TokenKind::Semicolon)
        ) {
            self.advance();
        }
    }

    fn parse_call(&mut self, callee: Expr) -> Result<Expr, ParseError> {
        self.expect_simple(TokenKind::LeftParen, "expected '('")?;
        let mut arguments = Vec::new();

        if !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightParen)
        ) {
            loop {
                let label = if let Some(Token {
                    kind: TokenKind::Identifier(name),
                    ..
                }) = self.peek()
                {
                    let next_is_colon = self
                        .tokens
                        .get(self.position + 1)
                        .is_some_and(|token| matches!(token.kind, TokenKind::Colon));
                    if next_is_colon {
                        let name = name.clone();
                        self.advance();
                        self.advance();
                        Some(name)
                    } else {
                        None
                    }
                } else {
                    None
                };
                arguments.push(CallArgument {
                    label,
                    value: self.parse_expression(0)?,
                });
                if !matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Comma)) {
                    break;
                }
                self.advance();
            }
        }

        let right = self.expect_simple(TokenKind::RightParen, "expected ')' after arguments")?;
        let span = callee.span.merge(right.span);
        Ok(self.make_expr(
            ExprKind::Call {
                callee: Box::new(callee),
                arguments,
            },
            span,
        ))
    }

    fn starts_legacy_type_arguments(&self) -> bool {
        if !matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Less)) {
            return false;
        }
        let mut depth = 0usize;
        for (offset, token) in self.tokens[self.position..].iter().enumerate() {
            match token.kind {
                TokenKind::Less => depth += 1,
                TokenKind::Greater => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return matches!(
                            self.tokens
                                .get(self.position + offset + 1)
                                .map(|token| &token.kind),
                            Some(TokenKind::LeftParen)
                        );
                    }
                }
                TokenKind::Newline => return false,
                _ => {}
            }
        }
        false
    }

    fn parse_block(&mut self) -> Result<Expr, ParseError> {
        let left = self.expect_simple(TokenKind::LeftBrace, "expected '{'")?;
        self.parse_block_after_left_brace(left.span)
    }

    fn parse_block_after_left_brace(&mut self, left_span: Span) -> Result<Expr, ParseError> {
        // A block is a fresh context: trailing closures work inside it even
        // when the enclosing position (e.g. an if condition) suppresses them.
        let outer_suppression = std::mem::replace(&mut self.suppress_trailing_closure, 0);
        let result = self.parse_block_statements_after_left_brace(left_span);
        self.suppress_trailing_closure = outer_suppression;
        result
    }

    fn parse_block_statements_after_left_brace(
        &mut self,
        left_span: Span,
    ) -> Result<Expr, ParseError> {
        let mut expressions = Vec::new();

        loop {
            self.skip_newlines();
            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::RightBrace)
            ) {
                let right = self.advance().expect("right brace checked above");
                return Ok(
                    self.make_expr(ExprKind::Block(expressions), left_span.merge(right.span))
                );
            }
            if self.peek().is_none() {
                return Err(ParseError::ExpectedToken {
                    expected: "missing '}'".to_owned(),
                    span: Some(left_span),
                });
            }

            expressions.push(self.parse_expression(0)?);
            match self.peek().map(|token| &token.kind) {
                Some(TokenKind::Semicolon) => {
                    self.advance();
                }
                Some(TokenKind::Newline) => {
                    self.skip_newlines();
                }
                Some(TokenKind::RightBrace) => {}
                Some(_) => {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected a newline, ';', or '}' after expression".to_owned(),
                        span: self.peek().map(|token| token.span),
                    });
                }
                None => {
                    return Err(ParseError::ExpectedToken {
                        expected: "missing '}'".to_owned(),
                        span: Some(left_span),
                    })
                }
            }
        }
    }

    /// Creates a new expression node, allocating a NodeId
    fn make_expr(&mut self, kind: ExprKind, span: Span) -> Expr {
        Expr::new(self.id_gen.next(), kind, span)
    }
}

fn infer_type_parameters(
    parameters: &[Parameter],
    return_type: Option<&TypeAnnotation>,
) -> Vec<TypeParameter> {
    let mut names = Vec::new();
    let mut collect = |annotation: &TypeAnnotation| {
        collect_implicit_type_names(annotation, &mut names, false);
    };
    for parameter in parameters {
        collect(&parameter.ty);
    }
    if let Some(return_type) = return_type {
        collect(return_type);
    }
    names
        .into_iter()
        .map(|name| TypeParameter {
            name,
            bounds: Vec::new(),
            span: parameters
                .first()
                .map(|parameter| parameter.ty.span)
                .or_else(|| return_type.map(|annotation| annotation.span))
                .expect("inferred type parameter has a source annotation"),
        })
        .collect()
}

fn collect_implicit_type_names(
    annotation: &TypeAnnotation,
    names: &mut Vec<String>,
    allow_leaf: bool,
) {
    match &annotation.kind {
        TypeExpr::Name(name)
            if allow_leaf
                && name.len() == 1
                && name.as_bytes()[0].is_ascii_uppercase()
                && !names.contains(name) =>
        {
            names.push(name.clone());
        }
        TypeExpr::Tuple(elements) => {
            for element in elements {
                collect_implicit_type_names(element, names, allow_leaf);
            }
        }
        TypeExpr::Member { base, .. } => collect_implicit_type_names(base, names, allow_leaf),
        TypeExpr::Apply { callee, arguments } => {
            // Dyn's first argument names an interface, never an implicit type parameter.
            for argument in arguments.iter().skip(usize::from(callee.is_name("Dyn"))) {
                collect_implicit_type_names(&argument.value, names, true);
            }
        }
        TypeExpr::Function { parameters, result } => {
            for parameter in parameters {
                collect_implicit_type_names(&parameter.value, names, allow_leaf);
            }
            collect_implicit_type_names(result, names, allow_leaf);
        }
        _ => {}
    }
}

/// Higher numbers bind tighter. Ranges sit between addition and `&&`; range
/// endpoints do not swallow bitwise operators
const PREC_BIT_OR: u8 = 3;
const PREC_ADD: u8 = 7;
const PREC_MUL: u8 = 8;

pub(super) fn compound_operator(token: &TokenKind) -> Option<BinaryOp> {
    match token {
        TokenKind::Arithmetic(op, mode, true) => Some(BinaryOp::Arithmetic(*op, *mode)),
        TokenKind::PlusEqual => Some(BinaryOp::Add),
        TokenKind::MinusEqual => Some(BinaryOp::Subtract),
        TokenKind::StarEqual => Some(BinaryOp::Multiply),
        TokenKind::SlashEqual => Some(BinaryOp::Divide),
        TokenKind::PercentEqual => Some(BinaryOp::Remainder),
        TokenKind::ShlEqual => Some(BinaryOp::ShiftLeft),
        TokenKind::ShrEqual => Some(BinaryOp::ShiftRight),
        TokenKind::AmpEqual => Some(BinaryOp::BitAnd),
        TokenKind::CaretEqual => Some(BinaryOp::BitXor),
        TokenKind::PipeEqual => Some(BinaryOp::BitOr),
        _ => None,
    }
}

fn binary_operator(token: &TokenKind) -> Option<(BinaryOp, u8)> {
    match token {
        TokenKind::Arithmetic(op, mode, false) => Some((
            BinaryOp::Arithmetic(*op, *mode),
            if matches!(op, super::ArithmeticOp::Add | super::ArithmeticOp::Subtract) {
                PREC_ADD
            } else {
                PREC_MUL
            },
        )),
        TokenKind::EqualEqual => Some((BinaryOp::Equal, 0)),
        TokenKind::NotEqual => Some((BinaryOp::NotEqual, 0)),
        TokenKind::Less => Some((BinaryOp::Less, 0)),
        TokenKind::LessEqual => Some((BinaryOp::LessEqual, 0)),
        TokenKind::Greater => Some((BinaryOp::Greater, 0)),
        TokenKind::GreaterEqual => Some((BinaryOp::GreaterEqual, 0)),
        TokenKind::AndAnd => Some((BinaryOp::And, 1)),
        TokenKind::OrOr => Some((BinaryOp::Or, 0)),
        TokenKind::Pipe => Some((BinaryOp::BitOr, PREC_BIT_OR)),
        TokenKind::Caret => Some((BinaryOp::BitXor, 4)),
        TokenKind::Ampersand => Some((BinaryOp::BitAnd, 5)),
        TokenKind::Shl => Some((BinaryOp::ShiftLeft, 6)),
        TokenKind::Shr => Some((BinaryOp::ShiftRight, 6)),
        TokenKind::Plus => Some((BinaryOp::Add, PREC_ADD)),
        TokenKind::Minus => Some((BinaryOp::Subtract, PREC_ADD)),
        TokenKind::Star => Some((BinaryOp::Multiply, PREC_MUL)),
        TokenKind::Slash => Some((BinaryOp::Divide, PREC_MUL)),
        TokenKind::Percent => Some((BinaryOp::Remainder, PREC_MUL)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[path = "concurrent.rs"]
    mod concurrent_tests;
    #[path = "control_flow.rs"]
    mod control_flow_tests;
    #[path = "declarations.rs"]
    mod declaration_tests;
    #[path = "errors.rs"]
    mod error_tests;
    #[path = "expressions.rs"]
    mod expression_tests;
}
