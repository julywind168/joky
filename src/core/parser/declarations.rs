//! Declarations syntax.
use super::*;

impl Parser {
    pub(super) fn parse_params(&mut self) -> Result<Vec<Param>, Diagnostic> {
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        while self.peek() != &TokenKind::RParen {
            let start = self.peek_span().start();
            let borrowed = if self.peek() == &TokenKind::Amp {
                self.advance();
                let mutable = self.peek() == &TokenKind::Mut;
                if mutable {
                    self.advance();
                }
                Some(mutable)
            } else {
                None
            };
            let name = self.expect_ident()?;
            self.check_binding_name(&name)?;
            if borrowed.is_some() && (name != "self" || self.impl_target.is_none()) {
                return Err(
                    self.error("Only a trait or impl receiver may use &self or &mut self".into())
                );
            }
            let ty = if name == "self" && self.impl_target.is_some() {
                if !params.is_empty() || self.peek() == &TokenKind::Colon {
                    return Err(self.error("self must be the first, unannotated parameter".into()));
                }
                let target = self.impl_target.clone().unwrap();
                borrowed.map_or_else(
                    || target.clone(),
                    |mutable| Type::reference(target.clone(), mutable),
                )
            } else {
                self.expect(&TokenKind::Colon)?;
                self.parse_type()?
            };
            params.push(Param {
                name,
                ty,
                span: self.span(start, self.last_span.end()),
            });
            self.expect_list_separator()?;
        }
        self.expect(&TokenKind::RParen)?;
        Ok(params)
    }

    /// Parse the optional `@suspends` attribute; anything else is an error.
    pub(super) fn parse_suspends_attr(&mut self) -> Result<bool, Diagnostic> {
        if self.peek() != &TokenKind::At {
            return Ok(false);
        }
        self.advance();
        let attr = self.expect_ident()?;
        if attr != "suspends" {
            return Err(self.error(format!("Unknown attribute @{}", attr)));
        }
        Ok(true)
    }

    /// Language functions may promise not to retain or return argument loans.
    fn parse_fn_attrs(&mut self) -> Result<(bool, bool, Vec<String>), Diagnostic> {
        let (mut suspends, mut noescape) = (false, false);
        let mut names = Vec::new();
        while self.peek() == &TokenKind::At {
            self.advance();
            match self.expect_ident()?.as_str() {
                "suspends" if !suspends => suspends = true,
                "noescape" if !noescape && names.is_empty() => {
                    noescape = true;
                    if self.peek() == &TokenKind::LParen {
                        self.advance();
                        loop {
                            names.push(self.expect_ident()?);
                            if self.peek() != &TokenKind::Comma {
                                break;
                            }
                            self.advance();
                        }
                        self.expect(&TokenKind::RParen)?;
                        noescape = false;
                    }
                }
                attr => return Err(self.error(format!("Unknown or duplicate attribute @{attr}"))),
            }
        }
        if suspends && (noescape || !names.is_empty()) {
            return Err(self.error("@noescape functions must be synchronous".into()));
        }
        Ok((suspends, noescape, names))
    }

    /// Parse the optional `-> RetType`; a missing arrow means Unit.
    pub(super) fn parse_ret_ty(&mut self) -> Result<Type, Diagnostic> {
        if self.peek() == &TokenKind::Arrow {
            self.advance();
            Ok(self.parse_type()?)
        } else {
            Ok(Type::Unit)
        }
    }

    pub(super) fn parse_fn_def(&mut self, start: usize) -> Result<FnDef, Diagnostic> {
        self.parse_fn_with_intrinsic(start, None)
            .map(|(function, _)| function)
    }

    pub(super) fn parse_fn_with_intrinsic(
        &mut self,
        start: usize,
        mut intrinsic: Option<Box<IntrinsicAttribute>>,
    ) -> Result<(FnDef, Option<Box<IntrinsicAttribute>>), Diagnostic> {
        // The `fn` token has been consumed.
        let name = self.expect_ident()?;
        let type_params = self.parse_type_parameters(true)?;
        let enclosing_parameters = self.type_params.clone();
        if let Some(target) = &self.impl_target {
            let target_name = match target {
                Type::Applied(application) => application.name.clone(),
                _ => target.to_string(),
            };
            if let Some(parameter) = type_params.iter().find(|p| {
                p.name == target_name
                    || enclosing_parameters
                        .iter()
                        .any(|outer| outer.name == p.name)
            }) {
                return Err(Diagnostic::at(
                    self.peek_line(),
                    parameter.span,
                    "Method type parameters cannot shadow impl parameters or the impl target",
                ));
            }
            if enclosing_parameters.len() + type_params.len() > 64 {
                return Err(self.error(
                    "An impl and method may have at most 64 type parameters in total".into(),
                ));
            }
            self.type_params.extend(type_params.iter().cloned());
        } else {
            self.type_params.clone_from(&type_params);
        }
        let params = self.parse_params()?;
        let (suspends, noescape, noescape_params) = self.parse_fn_attrs()?;
        let ret_ty = self.parse_ret_ty()?;
        let body = if let Some(attribute) = &mut intrinsic {
            attribute.has_body = self.peek() == &TokenKind::LBrace;
            if attribute.has_body {
                self.parse_block()?
            } else {
                let span = self.peek_span();
                self.expect(&TokenKind::Semi)?;
                Expr::Block {
                    exprs: Vec::new(),
                    span,
                }
            }
        } else {
            self.parse_block()?
        };
        self.type_params = enclosing_parameters;
        Ok((
            FnDef {
                json_encode: false,
                noescape: (noescape || !noescape_params.is_empty()).then(|| {
                    Box::new(NoEscape {
                        all: noescape,
                        params: noescape_params,
                    })
                }),
                public: false,
                buffer_intrinsic: None,
                compiler_intrinsic: None,
                buffer_element: None,
                scope: None,
                name,
                type_params,
                params,
                ret_ty,
                suspends,
                body,
                span: self.span(start, self.last_span.end()),
            },
            intrinsic,
        ))
    }

    fn parse_extern_fn(&mut self, start: usize, c_abi: bool) -> Result<ExternFnDecl, Diagnostic> {
        // The `extern fn` tokens have been consumed.
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let suspends = self.parse_suspends_attr()?;
        let ret_ty = self.parse_ret_ty()?;
        if c_abi && suspends {
            return Err(self.error("C imports cannot be @suspends".into()));
        }
        Ok(ExternFnDecl {
            c_abi,
            name,
            params,
            ret_ty,
            suspends,
            span: self.span(start, self.last_span.end()),
        })
    }

    pub(super) fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        match self.peek().clone() {
            TokenKind::Fn => {
                let start = self.peek_span().start();
                self.advance();
                Ok(Item::FnDef(self.parse_fn_def(start)?))
            }
            TokenKind::Unsafe => {
                let start = self.peek_span().start();
                self.advance();
                self.expect(&TokenKind::Extern)?;
                self.expect(&TokenKind::StrLit("C".into()))?;
                self.expect(&TokenKind::Fn)?;
                Ok(Item::ExternFn(self.parse_extern_fn(start, true)?))
            }
            TokenKind::Extern => {
                let start = self.peek_span().start();
                self.advance();
                self.expect(&TokenKind::Fn)?;
                Ok(Item::ExternFn(self.parse_extern_fn(start, false)?))
            }
            other => Err(self.error(format!("Expected fn or extern fn, got {:?}", other))),
        }
    }

    fn expect_associated_separator(&mut self) -> Result<(), Diagnostic> {
        if self.peek() == &TokenKind::Semi {
            self.advance();
            return Ok(());
        }
        if self.peek() == &TokenKind::RBrace || self.peek_line() > self.last_line {
            return Ok(());
        }
        Err(self.error("Expected a semicolon or newline after associated type".into()))
    }

    pub(super) fn parse_trait_name(&mut self) -> Result<String, Diagnostic> {
        let name = self.expect_ident()?;
        self.parse_dotted_name(name)
    }

    pub(super) fn parse_trait(&mut self, public: bool) -> Result<TraitDef, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::Trait)?;
        let name = self.expect_ident()?;
        if matches!(name.as_str(), "Fn" | "FnMut" | "FnOnce") {
            return Err(self.error("Callable bound names are reserved".into()));
        }
        let mut parameter = TypeParameter::new("Self".into(), self.peek_span());
        parameter.bounds.push(TraitBound {
            name: name.clone(),
            equalities: Vec::new(),
            span: self.peek_span(),
        });
        let self_parameter = std::sync::Arc::new(parameter);
        self.type_params = vec![self_parameter.clone()];
        self.impl_target = Some(Type::Parameter(self_parameter.clone()));
        self.impl_trait = Some(name.clone());
        self.expect(&TokenKind::LBrace)?;
        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            if self.peek() == &TokenKind::Type {
                self.advance();
                let span = self.peek_span();
                let name = self.expect_ident()?;
                let mut bounds = Vec::new();
                if self.peek() == &TokenKind::Colon {
                    self.advance();
                    loop {
                        let span = self.peek_span();
                        bounds.push(TraitBound {
                            name: self.parse_trait_name()?,
                            equalities: Vec::new(),
                            span,
                        });
                        if self.peek() != &TokenKind::Plus {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect_associated_separator()?;
                associated_types.push(AssociatedTypeDecl { name, bounds, span });
                continue;
            }
            let start = self.peek_span().start();
            self.expect(&TokenKind::Fn)?;
            let name_span = self.peek_span();
            let name = self.expect_ident()?;
            let params = self.parse_params()?;
            let (suspends, noescape, noescape_params) = self.parse_fn_attrs()?;
            let ret_ty = self.parse_ret_ty()?;
            let signature_span = self.span(start, self.last_span.end());
            let body = if self.peek() == &TokenKind::LBrace {
                Some(self.parse_block()?)
            } else {
                None
            };
            let span = self.span(start, self.last_span.end());
            methods.push(TraitMethod {
                noescape: (noescape || !noescape_params.is_empty()).then(|| {
                    Box::new(NoEscape {
                        all: noescape,
                        params: noescape_params,
                    })
                }),
                name,
                name_span,
                signature_span,
                params,
                ret_ty,
                suspends,
                body,
                span,
            });
        }
        self.expect(&TokenKind::RBrace)?;
        self.type_params.clear();
        self.impl_target = None;
        self.impl_trait = None;
        Ok(TraitDef {
            is_drop: false,
            public,
            name,
            self_parameter,
            associated_types,
            methods,
            span: self.span(start, self.last_span.end()),
            scope: None,
        })
    }

    pub(super) fn parse_impl(&mut self) -> Result<ImplDef, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::Impl)?;
        let type_params = self.parse_type_parameters(true)?;
        if let Some(parameter) = type_params.iter().find(|p| p.name == "Self") {
            return Err(Diagnostic::at(
                self.peek_line(),
                parameter.span,
                "Self is reserved inside impl",
            ));
        }
        self.type_params.clone_from(&type_params);
        let first = self.parse_type()?;
        let (trait_name, target) = if self.peek() == &TokenKind::For {
            self.advance();
            let Type::Named(name) = first else {
                return Err(self.error("Expected a non-generic trait name before for".into()));
            };
            (Some(name), self.parse_type()?)
        } else {
            (None, first)
        };
        if trait_name.is_some() && type_params.iter().any(|p| p.kind == ParameterKind::Const) {
            return Err(
                self.error("Const parameters are not supported in trait implementations".into())
            );
        }
        let target_name = match &target {
            Type::Applied(application) => application.name.clone(),
            _ => target.to_string(),
        };
        if matches!(target, Type::Tuple(_))
            || (type_params.is_empty() && matches!(target, Type::Applied(_)))
        {
            return Err(self
                .error("impl requires a named type; generic targets need impl parameters".into()));
        }
        self.impl_target = Some(target.clone());
        self.impl_trait = trait_name.clone();
        self.expect(&TokenKind::LBrace)?;
        let mut associated_types = Vec::new();
        let mut methods = Vec::new();
        let mut constants = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            let start = self.peek_span().start();
            let public = self.peek() == &TokenKind::Pub;
            if public {
                self.advance();
            }
            match self.peek() {
                TokenKind::Fn => {
                    self.advance();
                    let mut method = self.parse_fn_def(start)?;
                    method.public = public;
                    methods.push(method);
                }
                TokenKind::Type if !public && trait_name.is_some() => {
                    self.advance();
                    let span = self.peek_span();
                    let name = self.expect_ident()?;
                    self.expect(&TokenKind::Eq)?;
                    let target_start = self.peek_span().start();
                    let ty = self.parse_type()?;
                    let target_span = self.span(target_start, self.last_span.end());
                    self.expect_associated_separator()?;
                    associated_types.push(AssociatedTypeDef {
                        name,
                        ty,
                        span,
                        target_span,
                    });
                }
                TokenKind::Const => {
                    let declaration = self.parse_const(public, start)?;
                    constants.push(AssociatedConst {
                        public: declaration.public,
                        scope: declaration.scope,
                        name: declaration.name,
                        ty: declaration.ty,
                        value: declaration.value,
                        span: declaration.span,
                    });
                }
                TokenKind::Semi if !public => {
                    self.advance();
                }
                _ => return Err(self.error("Expected fn or const in impl".into())),
            }
        }
        self.expect(&TokenKind::RBrace)?;
        self.impl_target = None;
        self.impl_trait = None;
        self.type_params.clear();
        Ok(ImplDef {
            json_derive: None,
            trait_name,
            origin: None,
            scope: None,
            type_params,
            target,
            target_name,
            associated_types,
            methods,
            constants,
            span: self.span(start, self.last_span.end()),
        })
    }

    pub(super) fn parse_enum(&mut self, public: bool) -> Result<EnumDef, Diagnostic> {
        let start = self.peek_span().start();
        self.advance();
        let name_span = self.peek_span();
        let name = self.expect_ident()?;
        let type_params = self.parse_type_parameters(false)?;
        if type_params.iter().any(|p| p.kind == ParameterKind::Const) {
            return Err(
                self.error("Const parameters are not supported on enum declarations".into())
            );
        }
        self.type_params.clone_from(&type_params);
        self.expect(&TokenKind::LBrace)?;
        let mut variants = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            if self.peek() == &TokenKind::Comma {
                return Err(self.error("Enum variants cannot be empty".into()));
            }
            let start = self.peek_span().start();
            let name = self.expect_ident()?;
            let payload = if self.peek() == &TokenKind::LParen {
                self.advance();
                let ty = self.parse_type()?;
                if self.peek() == &TokenKind::Comma {
                    self.advance();
                }
                self.expect(&TokenKind::RParen)?;
                Some(ty)
            } else {
                None
            };
            variants.push(VariantDef {
                name,
                payload,
                span: self.span(start, self.last_span.end()),
            });
            self.expect_braced_separator("enum variants")?;
        }
        self.expect(&TokenKind::RBrace)?;
        self.type_params.clear();
        let mut declaration = EnumDef::new(
            name,
            variants,
            name_span,
            self.span(start, self.last_span.end()),
        );
        declaration.public = public;
        declaration.type_params = type_params;
        Ok(declaration)
    }

    /// Parse `const NAME: Ty = value`, shared by module-level and associated
    /// constants. Module constants end at the initializer because the module
    /// has no closing delimiter; the caller keeps `start` for the full span.
    pub(super) fn parse_const(
        &mut self,
        public: bool,
        start: usize,
    ) -> Result<ModuleConst, Diagnostic> {
        self.expect(&TokenKind::Const)?;
        let name = self.expect_ident()?;
        self.expect(&TokenKind::Colon)?;
        let ty = self.parse_type()?;
        self.expect(&TokenKind::Eq)?;
        let value = self.parse_assignment()?;
        Ok(ModuleConst {
            public,
            scope: None,
            name,
            path: None,
            ty,
            value,
            span: self.span(start, self.last_span.end()),
        })
    }

    pub(super) fn parse_record(
        &mut self,
        public: bool,
        attributes: Vec<Attribute>,
    ) -> Result<RecordDef, Diagnostic> {
        let start = self.peek_span().start();
        self.advance();
        let name_span = self.peek_span();
        let name = self.expect_ident()?;
        let type_params = self.parse_type_parameters(false)?;
        self.type_params.clone_from(&type_params);
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        let mut public_fields = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            if self.peek() == &TokenKind::Comma {
                return Err(self.error("Record fields cannot be empty".into()));
            }
            let start = self.peek_span().start();
            let field_public = self.peek() == &TokenKind::Pub;
            if field_public {
                self.advance();
            }
            if matches!(self.peek(), TokenKind::Ident(modifier) if modifier == "private")
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| matches!(t.kind, TokenKind::Ident(_)))
            {
                return Err(self.error(
                    "The 'private' field modifier is no longer supported; fields are private by default — write 'field: T', or add 'pub' to make the field public".into(),
                ));
            }
            let name = self.expect_ident()?;
            if field_public {
                public_fields.push(name.clone());
            }
            self.expect(&TokenKind::Colon)?;
            let ty = self.parse_type()?;
            fields.push(Param {
                name,
                ty,
                span: self.span(start, self.last_span.end()),
            });
            self.expect_braced_separator("record fields")?;
        }
        self.expect(&TokenKind::RBrace)?;
        self.type_params.clear();
        let mut record = RecordDef::new(
            name,
            fields,
            name_span,
            self.span(start, self.last_span.end()),
        );
        record.public_fields = public_fields;
        record.attributes = attributes;
        record.public = public;
        record.type_params = type_params;
        Ok(record)
    }

    pub(super) fn parse_type_alias(&mut self, public: bool) -> Result<TypeAlias, Diagnostic> {
        let start = self.peek_span().start();
        self.advance();
        let name_span = self.peek_span();
        let name = self.expect_ident()?;
        let type_params = self.parse_type_parameters(false)?;
        self.type_params = type_params.clone();
        self.expect(&TokenKind::Eq)?;
        let target_start = self.peek_span().start();
        let target = self.parse_type()?;
        self.type_params.clear();
        Ok(TypeAlias {
            public,
            name,
            type_params,
            target,
            name_span,
            target_span: self.span(target_start, self.last_span.end()),
            span: self.span(start, self.last_span.end()),
        })
    }
}
