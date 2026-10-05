//! Top-level declaration ordering and dispatch.
use super::*;

impl Parser {
    pub(super) fn parse_module(&mut self) -> Result<Module, Diagnostic> {
        let mut module = Module {
            sources: Default::default(),
            scope: None,
            imports: Vec::new(),
            type_aliases: Vec::new(),
            records: Vec::new(),
            enums: Vec::new(),
            implementations: Vec::new(),
            traits: Vec::new(),
            items: Vec::new(),
        };
        let mut declarations_started = false;
        while self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            let mut attributes = Vec::new();
            while self.peek() == &TokenKind::Hash {
                let start = self.peek_span().start();
                self.advance();
                let mut attribute = self.parse_attribute(0)?;
                attribute.span = self.span(start, self.last_span.end());
                attributes.push(attribute);
            }
            let public = self.peek() == &TokenKind::Pub;
            if public {
                self.advance();
            }
            if !attributes.is_empty() && self.peek() != &TokenKind::Struct {
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
                    "pub requires a function, type, struct, enum or trait declaration".into(),
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
                TokenKind::Impl => module.implementations.push(self.parse_impl()?),
                TokenKind::Enum => module.enums.push(self.parse_enum(public)?),
                TokenKind::Trait => module.traits.push(self.parse_trait(public)?),
                TokenKind::Struct => module.records.push(self.parse_record(public, attributes)?),
                TokenKind::Type => module.type_aliases.push(self.parse_type_alias(public)?),
                _ => {
                    let mut item = self.parse_item()?;
                    if let Item::FnDef(function) = &mut item {
                        function.public = public;
                    }
                    module.items.push(item);
                }
            }
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
            self.advance();
            path.push('/');
            path.push_str(&self.expect_ident()?);
        }
        let mut alias = self.parse_import_alias()?;
        let mut members = Vec::new();
        if self.peek() == &TokenKind::Dot {
            self.advance();
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
