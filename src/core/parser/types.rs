//! Types syntax.
use super::*;

impl Parser {
    pub(super) fn parse_type(&mut self) -> Result<Type, Diagnostic> {
        if self.type_depth >= 256 {
            return Err(self.error("Type syntax nesting exceeds 256 levels".into()));
        }
        self.type_depth += 1;
        let result = self.parse_type_inner(true);
        self.type_depth -= 1;
        result
    }

    pub(super) fn parse_type_inner(&mut self, projections: bool) -> Result<Type, Diagnostic> {
        let start = self.peek_span();
        let mut is_self = matches!(self.peek(), TokenKind::Ident(name) if name == "Self");
        let mut ty = match self.peek().clone() {
            TokenKind::Dyn => {
                self.advance();
                let name = self.parse_trait_name()?;
                let equalities = self.parse_associated_equalities()?;
                Ok(Type::DynTrait(std::sync::Arc::new(DynTraitType {
                    identity: None,
                    interface: None,
                    name,
                    equalities,
                    span: start,
                })))
            }
            TokenKind::Lt | TokenKind::Shl => {
                self.expect_type_open()?;
                let base = self.parse_type()?;
                if self.expect_ident()? != "as" {
                    return Err(self.error("Expected as in qualified projection".into()));
                }
                let trait_name = self.parse_trait_name()?;
                self.expect_type_close()?;
                self.expect(&TokenKind::ColonColon)?;
                let name = self.expect_ident()?;
                Ok(Type::Projection(std::sync::Arc::new(TypeProjection {
                    base,
                    trait_name: Some(trait_name),
                    name,
                    identity: None,
                    span: start,
                })))
            }
            TokenKind::Fn => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let mut params = Vec::new();
                while self.peek() != &TokenKind::RParen {
                    params.push(self.parse_type()?);
                    self.expect_list_separator()?;
                }
                self.expect(&TokenKind::RParen)?;
                let (suspends, noescape, names) = self.parse_fn_attrs()?;
                if !names.is_empty() {
                    return Err(
                        self.error("Function types use @noescape without parameter names".into())
                    );
                }
                self.expect(&TokenKind::Arrow)?;
                Ok(Type::function_with_contract(
                    params,
                    self.parse_type()?,
                    suspends,
                    noescape,
                ))
            }
            TokenKind::Amp => {
                self.advance();
                let mutable = self.peek() == &TokenKind::Mut;
                if mutable {
                    self.advance();
                }
                Ok(Type::reference(self.parse_type()?, mutable))
            }
            TokenKind::AndAnd => {
                Err(self.error("References to references are not supported".into()))
            }
            TokenKind::LParen => {
                self.advance();
                let first = self.parse_type()?;
                if self.peek() != &TokenKind::Comma {
                    self.expect(&TokenKind::RParen)?;
                    Ok(first)
                } else {
                    self.advance();
                    let mut elements = vec![first];
                    while self.peek() != &TokenKind::RParen {
                        elements.push(self.parse_type()?);
                        self.expect_list_separator()?;
                    }
                    self.expect(&TokenKind::RParen)?;
                    Ok(Type::tuple(elements))
                }
            }
            TokenKind::TyFloat32 => {
                self.advance();
                Ok(Type::Float32)
            }
            TokenKind::TyFloat64 => {
                self.advance();
                Ok(Type::Float64)
            }
            TokenKind::TyInt8 => {
                self.advance();
                Ok(Type::Int8)
            }
            TokenKind::TyInt16 => {
                self.advance();
                Ok(Type::Int16)
            }
            TokenKind::TyUInt8 => {
                self.advance();
                Ok(Type::UInt8)
            }
            TokenKind::TyUInt16 => {
                self.advance();
                Ok(Type::UInt16)
            }
            TokenKind::TyUInt32 => {
                self.advance();
                Ok(Type::UInt32)
            }
            TokenKind::TyRune => {
                self.advance();
                Ok(Type::Rune)
            }
            TokenKind::TyUInt64 => {
                self.advance();
                Ok(Type::UInt64)
            }
            TokenKind::TyInt64 => {
                self.advance();
                Ok(Type::Int64)
            }
            TokenKind::TyInt32 => {
                self.advance();
                Ok(Type::Int32)
            }
            TokenKind::TyString => {
                self.advance();
                Ok(Type::String)
            }
            TokenKind::TyPtr => {
                self.advance();
                Ok(Type::Ptr)
            }
            TokenKind::TyBool => {
                self.advance();
                Ok(Type::Bool)
            }
            TokenKind::TyUnit => {
                self.advance();
                Ok(Type::Unit)
            }
            TokenKind::Ident(name) => {
                self.advance();
                let mut name = name;
                if self
                    .type_params
                    .iter()
                    .any(|p| p.name == name && p.kind == ParameterKind::Const)
                {
                    return Err(self.error("Const parameter cannot be used as a type".into()));
                }
                if name == "Self" || self.type_params.iter().any(|p| p.name == name) {
                    if matches!(self.peek(), TokenKind::Lt | TokenKind::Shl) {
                        return Ok(Type::Applied(std::sync::Arc::new(TypeApplication {
                            name,
                            arguments: self.parse_type_arguments()?,
                        })));
                    }
                    let base = if name == "Self" {
                        self.impl_target
                            .clone()
                            .unwrap_or(Type::Named(name.clone()))
                    } else {
                        Type::Parameter(
                            self.type_params
                                .iter()
                                .find(|p| p.name == name)
                                .unwrap()
                                .clone(),
                        )
                    };
                    Ok(base)
                } else {
                    // Module paths are split from member suffixes during name binding.
                    name = self.parse_dotted_name(name)?;
                    if matches!(self.peek(), TokenKind::Lt | TokenKind::Shl) {
                        Ok(Type::Applied(std::sync::Arc::new(TypeApplication {
                            name,
                            arguments: self.parse_type_arguments()?,
                        })))
                    } else {
                        Ok(Type::Named(name))
                    }
                }
            }
            other => Err(self.error(format!("Expected a type, got {:?}", other))),
        }?;
        while projections && self.peek() == &TokenKind::Dot {
            self.advance();
            let name = self.expect_ident()?;
            let trait_name = if is_self {
                self.impl_trait.clone()
            } else {
                None
            };
            is_self = false;
            ty = Type::Projection(std::sync::Arc::new(TypeProjection {
                base: ty,
                trait_name,
                name,
                identity: None,
                span: start,
            }));
        }
        Ok(ty)
    }

    fn parse_associated_equalities(&mut self) -> Result<Vec<AssociatedEquality>, Diagnostic> {
        let mut equalities = Vec::new();
        if self.peek() == &TokenKind::Lt {
            self.advance();
            loop {
                let span = self.peek_span();
                let name = self.expect_ident()?;
                self.expect(&TokenKind::Eq)?;
                let ty = self.parse_type()?;
                equalities.push(AssociatedEquality { name, ty, span });
                if self.peek() != &TokenKind::Comma {
                    break;
                }
                self.advance();
                if self.peek() == &TokenKind::Gt {
                    break;
                }
            }
            self.expect_type_close()?;
        }
        Ok(equalities)
    }

    pub(super) fn parse_type_parameters(
        &mut self,
        allow_bounds: bool,
    ) -> Result<Vec<std::sync::Arc<TypeParameter>>, Diagnostic> {
        let mut type_params = Vec::<std::sync::Arc<TypeParameter>>::new();
        if self.peek() == &TokenKind::Lt {
            self.advance();
            loop {
                let span = self.peek_span();
                let constant = self.peek() == &TokenKind::Const;
                if constant {
                    self.advance();
                }
                let parameter = self.expect_ident()?;
                if matches!(parameter.as_str(), "_" | "Copy" | "Fn" | "FnMut" | "FnOnce")
                    || type_params.iter().any(|p| p.name == parameter)
                {
                    return Err(Diagnostic::at(
                        self.peek_line(),
                        span,
                        "Duplicate or reserved type parameter",
                    ));
                }
                if type_params.len() == 64 {
                    return Err(
                        self.error("A declaration may have at most 64 type parameters".into())
                    );
                }
                let mut parameter = TypeParameter::new(parameter, span);
                if constant {
                    parameter.kind = ParameterKind::Const;
                    self.expect(&TokenKind::Colon)?;
                    self.expect(&TokenKind::TyUInt64)?;
                }
                if !constant && self.peek() == &TokenKind::Colon {
                    self.advance();
                    loop {
                        if matches!(self.peek(), TokenKind::Ident(name) if name == "Copy") {
                            self.advance();
                            if parameter.copy {
                                return Err(self.error("Duplicate Copy bound".into()));
                            }
                            parameter.copy = true;
                        } else if !allow_bounds {
                            return Err(self.error(
                                "Only Copy bounds are supported on type declarations".into(),
                            ));
                        } else if matches!(self.peek(), TokenKind::Ident(name) if matches!(name.as_str(), "Fn" | "FnMut" | "FnOnce"))
                        {
                            let name = self.expect_ident()?;
                            self.expect(&TokenKind::LParen)?;
                            let mut params = Vec::new();
                            while self.peek() != &TokenKind::RParen {
                                params.push(self.parse_type()?);
                                self.expect_list_separator()?;
                            }
                            self.expect(&TokenKind::RParen)?;
                            let (suspends, noescape, names) = self.parse_fn_attrs()?;
                            if !names.is_empty() {
                                return Err(
                                    self.error("Callable bounds require unnamed @noescape".into())
                                );
                            }
                            if suspends && name != "FnOnce" {
                                return Err(
                                    self.error("Suspending callable bounds require FnOnce".into())
                                );
                            }
                            if parameter
                                .callable
                                .as_ref()
                                .is_some_and(|b| b.suspends != suspends)
                            {
                                return Err(self.error(
                                    "Callable bounds must have the same suspension effect".into(),
                                ));
                            }
                            self.expect(&TokenKind::Arrow)?;
                            let ret = self.parse_type()?;
                            let mode = match name.as_str() {
                                "Fn" => CallableKind::Fn,
                                "FnMut" => CallableKind::FnMut,
                                _ => CallableKind::FnOnce,
                            };
                            let signature = crate::FunctionSignature::new(params, ret);
                            let mut additional_signatures = parameter
                                .callable
                                .as_ref()
                                .map_or_else(Vec::new, |b| b.additional_signatures.clone());
                            if let Some(bound) = &parameter.callable {
                                additional_signatures.push(bound.signature.clone());
                            }
                            let mode = parameter
                                .callable
                                .as_ref()
                                .map_or(mode, |b| b.mode.min(mode));
                            parameter.callable = Some(std::sync::Arc::new(CallableBound {
                                suspends,
                                mode,
                                noescape: noescape
                                    || parameter.callable.as_ref().is_some_and(|b| b.noescape),
                                signature,
                                additional_signatures,
                            }));
                        } else {
                            let span = self.peek_span();
                            let name = self.parse_trait_name()?;
                            let equalities = self.parse_associated_equalities()?;
                            let bound = TraitBound {
                                name,
                                equalities,
                                span,
                            };
                            if parameter
                                .bounds
                                .iter()
                                .any(|b: &TraitBound| b.name == bound.name)
                            {
                                return Err(self.error("Duplicate trait bound".into()));
                            }
                            parameter.bounds.push(bound);
                        }
                        if self.peek() != &TokenKind::Plus {
                            break;
                        }
                        self.advance();
                    }
                }
                type_params.push(std::sync::Arc::new(parameter));
                if self.peek() != &TokenKind::Comma {
                    break;
                }
                self.advance();
                if self.peek() == &TokenKind::Gt {
                    break;
                }
            }
            self.expect_type_close()?;
        }
        let parameters = self
            .type_params
            .iter()
            .chain(&type_params)
            .cloned()
            .collect::<Vec<_>>();
        fn bind(ty: &mut Type, parameters: &[std::sync::Arc<TypeParameter>]) {
            match ty {
                Type::Named(name) => {
                    if let Some(p) = parameters.iter().find(|p| p.name == *name) {
                        *ty = Type::Parameter(p.clone());
                    } else if let Some((base, members)) = name.split_once('.') {
                        if let Some(p) = parameters.iter().find(|p| p.name == base) {
                            *ty = members.split('.').fold(
                                Type::Parameter(p.clone()),
                                |base, member| {
                                    Type::Projection(std::sync::Arc::new(TypeProjection {
                                        base,
                                        trait_name: None,
                                        name: member.to_owned(),
                                        identity: None,
                                        span: p.span,
                                    }))
                                },
                            );
                        }
                    }
                }
                Type::DynTrait(d) => {
                    let d = std::sync::Arc::make_mut(d);
                    for e in &mut d.equalities {
                        bind(&mut e.ty, parameters);
                    }
                    d.interface = None;
                }
                Type::Projection(p) => bind(&mut std::sync::Arc::make_mut(p).base, parameters),
                Type::Reference(r) => {
                    let r = std::sync::Arc::make_mut(r);
                    bind(&mut r.target, parameters);
                }
                Type::Applied(a) => {
                    for ty in &mut std::sync::Arc::make_mut(a).arguments {
                        if let GenericArgument::Type(Type::Named(name)) = ty {
                            if let Some(p) = parameters
                                .iter()
                                .find(|p| p.name == *name && p.kind == ParameterKind::Const)
                            {
                                *ty = p.argument();
                                continue;
                            }
                        }
                        if let Some(ty) = ty.as_type_mut() {
                            bind(ty, parameters);
                        }
                    }
                }
                Type::Function(f) => {
                    let f = std::sync::Arc::make_mut(f);
                    for ty in &mut f.signature.params {
                        bind(ty, parameters);
                    }
                    bind(&mut f.signature.ret, parameters);
                }
                Type::Tuple(t) => {
                    let mut elements = t.elements.clone();
                    for ty in &mut elements {
                        bind(ty, parameters);
                    }
                    *ty = Type::tuple(elements);
                }
                _ => {}
            }
        }
        for parameter in &mut type_params {
            for bound in &mut std::sync::Arc::make_mut(parameter).bounds {
                for equality in &mut bound.equalities {
                    bind(&mut equality.ty, &parameters);
                }
            }
            if let Some(bound) = &mut std::sync::Arc::make_mut(parameter).callable {
                let bound = std::sync::Arc::make_mut(bound);
                for signature in
                    std::iter::once(&mut bound.signature).chain(&mut bound.additional_signatures)
                {
                    for ty in &mut signature.params {
                        bind(ty, &parameters);
                    }
                    bind(&mut signature.ret, &parameters);
                }
            }
        }
        Ok(type_params)
    }

    // Split angle brackets only while parsing type syntax.
    fn expect_type_open(&mut self) -> Result<(), Diagnostic> {
        if self.peek() == &TokenKind::Shl {
            let token = &mut self.tokens[self.pos];
            let start = token.span.start();
            self.last_span = Span::new(start, start + 1).with_source(self.source);
            token.kind = TokenKind::Lt;
            token.span = Span::new(start + 1, token.span.end()).with_source(self.source);
            Ok(())
        } else {
            self.expect(&TokenKind::Lt)
        }
    }

    fn expect_type_close(&mut self) -> Result<(), Diagnostic> {
        if matches!(
            self.peek(),
            TokenKind::Shr | TokenKind::Ge | TokenKind::CompoundAssign(BinOp::Shr)
        ) {
            let remainder = match self.peek() {
                TokenKind::Shr => TokenKind::Gt,
                TokenKind::CompoundAssign(BinOp::Shr) => TokenKind::Ge,
                _ => TokenKind::Eq,
            };
            let token = &mut self.tokens[self.pos];
            let start = token.span.start();
            self.last_span = Span::new(start, start + 1).with_source(self.source);
            token.kind = remainder;
            token.span = Span::new(start + 1, token.span.end()).with_source(self.source);
            Ok(())
        } else {
            self.expect(&TokenKind::Gt)
        }
    }

    fn parse_generic_argument(&mut self) -> Result<GenericArgument, Diagnostic> {
        if let TokenKind::IntLit(n) = self.peek().clone() {
            self.advance();
            return Ok(GenericArgument::Const(ConstArgument::Value(n)));
        }
        if let TokenKind::Ident(name) = self.peek() {
            if let Some(p) = self
                .type_params
                .iter()
                .find(|p| p.name == *name && p.kind == ParameterKind::Const)
                .cloned()
            {
                self.advance();
                return Ok(GenericArgument::Const(ConstArgument::Parameter(p)));
            }
        }
        Ok(GenericArgument::Type(self.parse_type()?))
    }

    pub(super) fn parse_type_arguments(&mut self) -> Result<Vec<GenericArgument>, Diagnostic> {
        self.expect_type_open()?;
        let mut args = vec![self.parse_generic_argument()?];
        while self.peek() == &TokenKind::Comma {
            self.advance();
            if matches!(
                self.peek(),
                TokenKind::Gt
                    | TokenKind::Shr
                    | TokenKind::Ge
                    | TokenKind::CompoundAssign(BinOp::Shr)
            ) {
                break;
            }
            args.push(self.parse_generic_argument()?);
        }
        self.expect_type_close()?;
        Ok(args)
    }
}
