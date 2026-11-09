//! Top-level declaration ordering and dispatch.
use super::*;

impl Parser<'_> {
    pub(super) fn parse_module(&mut self) -> Result<Module, Diagnostic> {
        let mut module = Module {
            logical_name: "main".into(),
            intrinsic_attributes: Vec::new(),
            sources: Default::default(),
            scope: None,
            imports: Vec::new(),
            type_aliases: Vec::new(),
            records: Vec::new(),
            enums: Vec::new(),
            implementations: Vec::new(),
            traits: Vec::new(),
            constants: Vec::new(),
            items: Vec::new(),
        };
        let mut declarations_started = false;
        while self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            let start = self.peek_span().start();
            let mut attributes = Vec::new();
            while self.peek() == &TokenKind::Hash {
                let attr_start = self.peek_span();
                self.advance();
                let mut attribute = self.parse_attribute(0)?;
                attribute.span = self.span(attr_start.start(), self.last_span.end());
                attributes.push(attribute);
            }
            let public = self.peek() == &TokenKind::Pub;
            if public {
                self.advance();
            }
            if attributes.iter().any(|a| a.path == "test") && self.peek() != &TokenKind::Fn {
                return Err(self.error("#test requires a top-level function".into()));
            }
            if attributes.iter().any(|a| a.path == "every") {
                return Err(self.error(
                    "#every requires a method in an inherent impl for a #state type".into(),
                ));
            }
            if !attributes.is_empty()
                && self.peek() != &TokenKind::Struct
                && !(self.peek() == &TokenKind::Enum
                    && attributes.iter().all(|a| a.path == "derive"))
                && !(self.peek() == &TokenKind::Fn
                    && attributes
                        .iter()
                        .all(|a| matches!(a.path.as_str(), "intrinsic" | "test")))
            {
                if let Some(attr) = attributes.iter().find(|a| {
                    !matches!(
                        a.path.as_str(),
                        "repr" | "state" | "derive" | "test" | "every" | "intrinsic"
                    )
                }) {
                    let pos = self
                        .tokens
                        .partition_point(|t| t.span.start() < attr.span.start());
                    let mut error = Diagnostic::at(
                        self.tokens[pos].line,
                        attr.span,
                        format!("Unknown attribute #{}", attr.path),
                    );
                    error.code = crate::ErrorCode::UnknownName;
                    return Err(error);
                }
                return Err(self.error("Declaration attributes currently require a struct".into()));
            }
            if public
                && matches!(
                    self.peek(),
                    TokenKind::Import
                        | TokenKind::Impl
                        | TokenKind::Extern
                        | TokenKind::Unsafe
                        | TokenKind::Semi
                        | TokenKind::Eof
                )
            {
                return Err(self.error(
                    "pub requires a function, type, struct, enum, trait or constant declaration"
                        .into(),
                ));
            }
            if self.peek() == &TokenKind::Import {
                if declarations_started {
                    return Err(self.error("Imports must precede all declarations".into()));
                }
                module.imports.push(self.parse_import()?);
                continue;
            }
            declarations_started = true;
            match self.peek() {
                TokenKind::Fn => {
                    let is_test = attributes.iter().any(|a| a.path == "test");
                    let intrinsic = if is_test {
                        if attributes.len() != 1 || attributes[0].arguments.is_some() {
                            return Err(self.error(
                                "#test must appear once, without arguments or other attributes"
                                    .into(),
                            ));
                        }
                        None
                    } else {
                        self.parse_intrinsic_attribute(attributes)?
                    };
                    self.advance();
                    let (mut function, intrinsic) =
                        self.parse_fn_with_intrinsic(start, intrinsic)?;
                    if let Some(attribute) = intrinsic {
                        module
                            .intrinsic_attributes
                            .push((module.items.len(), attribute));
                    }
                    if is_test
                        && (function.name == "main"
                            || !function.type_params.is_empty()
                            || !function.params.is_empty()
                            || function.ret_ty != Type::Unit)
                    {
                        return Err(Diagnostic::at(self.peek_line(), function.span,
                            "#test requires a non-generic fn with no parameters and a Unit result, named other than main"));
                    }
                    if is_test {
                        function.kind = FunctionKind::Test;
                    }
                    function.public = public;
                    module.items.push(Item::FnDef(function));
                }
                TokenKind::Impl => module.implementations.push(self.parse_impl()?),
                TokenKind::Enum => {
                    let mut enumeration = self.parse_enum(public)?;
                    enumeration.attributes = attributes;
                    module.enums.push(enumeration);
                }
                TokenKind::Trait => module.traits.push(self.parse_trait(public)?),
                TokenKind::Struct => module.records.push(self.parse_record(public, attributes)?),
                TokenKind::Type => module.type_aliases.push(self.parse_type_alias(public)?),
                TokenKind::Const => module.constants.push(self.parse_const(public, start)?),
                _ => {
                    let mut item = self.parse_item()?;
                    if let Item::FnDef(function) = &mut item {
                        function.public = public;
                    }
                    module.items.push(item);
                }
            }
        }
        if self.durations && !module.imports.iter().any(|i| i.path == "joky/time") {
            module.imports.push(Import {
                path: "joky/time".into(),
                alias: Some("$joky_time".into()),
                members: vec![],
                span: self.last_span,
            });
        }
        if self.arrays && !module.imports.iter().any(|i| i.path == "joky/array") {
            module.imports.push(Import {
                path: "joky/array".into(),
                alias: Some("$joky_array".into()),
                members: vec![],
                span: self.last_span,
            });
        }
        if self.iterators {
            for (path, alias) in [
                ("joky/iter", "$joky_iter"),
                ("joky/option", "$joky_option"),
                ("joky/check", "$joky_check"),
            ] {
                if !module.imports.iter().any(|import| import.path == path) {
                    module.imports.push(Import {
                        path: path.into(),
                        alias: Some(alias.into()),
                        members: vec![],
                        span: self.last_span,
                    });
                }
            }
        }
        if self.sql_templates
            && !module
                .imports
                .iter()
                .any(|import| import.path == "joky/postgres")
        {
            module.imports.push(Import {
                path: "joky/postgres".into(),
                alias: Some("$joky_postgres".into()),
                members: vec![],
                span: self.last_span,
            });
        }
        if self.interpolation {
            for (path, alias) in [("joky/show", "$joky_show"), ("joky/string", "$joky_string")] {
                if module.imports.iter().any(|import| import.path == path) {
                    continue;
                }
                module.imports.push(Import {
                    path: path.into(),
                    alias: Some(alias.into()),
                    members: vec![],
                    span: self.last_span,
                });
            }
        }
        Ok(module)
    }

    fn parse_import(&mut self) -> Result<Import, Diagnostic> {
        let start = self.peek_span().start();
        self.advance();
        let mut path = self.expect_ident()?;
        while self.peek() == &TokenKind::Slash {
            self.format_role(format::Role::ImportSlash);
            self.advance();
            path.push('/');
            path.push_str(&self.expect_ident()?);
        }
        let mut alias = self.parse_import_alias()?;
        let mut members = Vec::new();
        if self.peek() == &TokenKind::Dot {
            self.advance();
            self.format_role(format::Role::ImportOpen);
            self.expect(&TokenKind::LBrace)?;
            while self.peek() != &TokenKind::RBrace {
                let member_start = self.peek_span().start();
                let kind = if self.peek() == &TokenKind::Type {
                    self.advance();
                    ImportMemberKind::Type
                } else {
                    ImportMemberKind::Value
                };
                let name = self.expect_ident()?;
                let member_alias = self.parse_import_alias()?;
                members.push(ImportMember {
                    name,
                    kind,
                    alias: member_alias,
                    span: self.span(member_start, self.last_span.end()),
                });
                if self.peek() == &TokenKind::Comma {
                    self.advance();
                } else if self.peek() != &TokenKind::RBrace {
                    return Err(self.error("Expected ',' or '}' in import member list".into()));
                }
            }
            self.format_role(format::Role::ImportClose);
            self.expect(&TokenKind::RBrace)?;
        }
        if alias.is_none() {
            alias = self.parse_import_alias()?;
        }
        Ok(Import {
            path,
            alias,
            members,
            span: self.span(start, self.last_span.end()),
        })
    }

    fn parse_import_alias(&mut self) -> Result<Option<String>, Diagnostic> {
        if matches!(self.peek(), TokenKind::Ident(name) if name == "as") {
            self.advance();
            Ok(Some(self.expect_ident()?))
        } else {
            Ok(None)
        }
    }
}
