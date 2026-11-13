//! Source occurrences recorded by the semantic checker for editor queries.
use super::parser::{
    ast::{Item, Module, Param},
    lexer::{InterpolationPart, Lexer, Token, TokenKind},
};
use crate::{FunctionSignature, SourceId, SourceMap, Span, Type};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

pub(crate) type Recorder = Rc<RefCell<SemanticRecorder>>;

/// Resolved identifiers and their source text for one checked module graph.
#[derive(Default)]
pub struct SemanticIndex {
    sources: SourceMap,
    symbols: BTreeMap<(SourceId, usize), Symbol>,
}

/// A declaration or use of a source-level binding or function.
pub struct Symbol {
    pub occurrence: Span,
    pub definition: Option<Span>,
    pub display: String,
}

impl SemanticIndex {
    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }

    pub fn symbol_at(&self, source: SourceId, offset: usize) -> Option<&Symbol> {
        let (_, symbol) = self.symbols.range(..=(source, offset)).next_back()?;
        (symbol.occurrence.source_id() == source && symbol.occurrence.contains(offset))
            .then_some(symbol)
    }
}

#[derive(Default)]
pub(crate) struct SemanticRecorder {
    index: SemanticIndex,
    tokens: HashMap<SourceId, Vec<Token>>,
    functions: HashMap<String, Function>,
    fields: HashMap<u64, HashMap<String, Span>>,
    pub(crate) completion: Option<super::completion::CompletionQuery>,
}

struct Function {
    definition: Option<Span>,
    name: String,
    parameters: Vec<String>,
}

impl SemanticRecorder {
    pub(crate) fn new(module: &Module) -> Recorder {
        let mut recorder = Self {
            index: SemanticIndex {
                sources: module.sources().clone(),
                symbols: BTreeMap::new(),
            },
            ..Self::default()
        };
        for record in &module.records {
            for field in &record.fields {
                let tokens = recorder.tokens(field.span);
                let Some(name) = tokens.windows(2).find_map(|pair| {
                    (pair[1].kind == TokenKind::Colon)
                        .then(|| ident(&pair[0], &field.name))
                        .flatten()
                }) else {
                    continue;
                };
                let definition = name.with_source(field.span.source_id());
                recorder
                    .fields
                    .entry(record.id)
                    .or_default()
                    .insert(field.name.clone(), definition);
                recorder.add(
                    definition,
                    Some(definition),
                    format!("{}: {}", field.name, field.ty),
                );
            }
        }
        Rc::new(RefCell::new(recorder))
    }

    pub(crate) fn finish(&mut self) -> SemanticIndex {
        std::mem::take(self).index
    }

    pub(crate) fn for_completion(query: super::completion::CompletionQuery) -> Recorder {
        Rc::new(RefCell::new(Self {
            completion: Some(query),
            ..Self::default()
        }))
    }

    fn tokens(&mut self, span: Span) -> &[Token] {
        let source = span.source_id();
        let tokens = self.tokens.entry(source).or_insert_with(|| {
            let Some(file) = self
                .index
                .sources
                .get(source)
                .filter(|file| file.builtin_module().is_none())
            else {
                return Vec::new();
            };
            let mut tokens = Vec::new();
            flatten_tokens(
                Lexer::tokenize(file.text()).unwrap_or_default(),
                &mut tokens,
            );
            tokens
        });
        let start = tokens.partition_point(|token| token.span.start() < span.start());
        let end = tokens.partition_point(|token| token.span.start() < span.end());
        &tokens[start..end]
    }

    fn add(&mut self, occurrence: Span, definition: Option<Span>, display: String) {
        // Rigid generic bodies are checked before their concrete instances.
        self.index
            .symbols
            .entry((occurrence.source_id(), occurrence.start()))
            .or_insert(Symbol {
                occurrence,
                definition,
                display,
            });
    }

    pub(crate) fn binding(&mut self, span: Span, name: &str, ty: &Type) -> Option<Span> {
        if self.completion.is_some() {
            return None;
        }
        let token = self.tokens(span).iter().find(|token| {
            !matches!(
                token.kind,
                TokenKind::Let | TokenKind::Var | TokenKind::Amp | TokenKind::Mut
            )
        })?;
        let span = ident(token, name)?.with_source(span.source_id());
        self.add(span, Some(span), format!("{name}: {ty}"));
        Some(span)
    }

    pub(crate) fn local(&mut self, span: Span, name: &str, definition: Span) {
        if let Some(occurrence) = self.path_name(span, name) {
            if let Some(symbol) = self
                .index
                .symbol_at(definition.source_id(), definition.start())
            {
                self.add(occurrence, Some(definition), symbol.display.clone());
            }
        }
    }

    pub(crate) fn declare_function(&mut self, item: &Item, signature: &FunctionSignature) {
        if self.completion.is_some() {
            return;
        }
        let (canonical, params, suspends, span, type_params) = match item {
            Item::FnDef(f) => (
                &f.name,
                &f.params,
                f.suspends,
                f.span,
                f.type_params.as_slice(),
            ),
            Item::ExternFn(f) => (&f.name, &f.params, f.suspends, f.span, &[][..]),
        };
        let name = canonical.rsplit(['.', '/']).next().unwrap();
        let definition = self.function_name(span, name);
        if definition.is_none()
            && !self
                .index
                .sources
                .get(span.source_id())
                .is_some_and(|file| file.builtin_module().is_some())
        {
            return;
        }
        let function = Function {
            definition,
            name: name.to_owned(),
            parameters: params.iter().map(|p| p.name.clone()).collect(),
        };
        if let Some(definition) = definition {
            let generic_names = type_params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>();
            self.add(
                definition,
                Some(definition),
                function.display(signature, suspends, &generic_names),
            );
            for (param, ty) in params.iter().zip(&signature.params) {
                let Param { span, name, .. } = param;
                self.binding(*span, name, ty);
            }
        }
        self.functions.insert(canonical.clone(), function);
    }

    fn function_name(&mut self, span: Span, name: &str) -> Option<Span> {
        let mut tokens = self.tokens(span).iter().skip_while(|t| {
            matches!(
                t.kind,
                TokenKind::Unsafe | TokenKind::Extern | TokenKind::StrLit(_) | TokenKind::Pub
            )
        });
        if tokens.next()?.kind != TokenKind::Fn {
            return None;
        }
        Some(ident(tokens.next()?, name)?.with_source(span.source_id()))
    }

    pub(crate) fn function(
        &mut self,
        span: Span,
        written: &str,
        canonical: &str,
        signature: &FunctionSignature,
        suspends: bool,
    ) {
        if self.completion.is_some() {
            return;
        }
        if written.starts_with("joky/member/") {
            self.method(span, canonical, signature, suspends);
            return;
        }
        let Some(occurrence) = self.path_name(span, written) else {
            return;
        };
        if let Some(function) = self.functions.get(canonical) {
            self.add(
                occurrence,
                function.definition,
                function.display(signature, suspends, &[]),
            );
        }
    }

    pub(crate) fn method(
        &mut self,
        span: Span,
        canonical: &str,
        signature: &FunctionSignature,
        suspends: bool,
    ) {
        let Some(name) = self
            .functions
            .get(canonical)
            .map(|function| function.name.clone())
        else {
            return;
        };
        if let Some(occurrence) = self.method_name(span, &name) {
            let function = &self.functions[canonical];
            self.add(
                occurrence,
                function.definition,
                function.display(signature, suspends, &[]),
            );
        }
    }

    pub(crate) fn field(&mut self, span: Span, record: u64, name: &str, ty: &Type) {
        let Some(definition) = self
            .fields
            .get(&record)
            .and_then(|fields| fields.get(name))
            .copied()
        else {
            return;
        };
        let tokens = self.tokens(span);
        let occurrence = match tokens {
            [first, second, ..] if second.kind == TokenKind::Colon => ident(first, name),
            _ => {
                let mut tokens = tokens
                    .iter()
                    .rev()
                    .skip_while(|t| t.kind == TokenKind::RParen);
                let occurrence = tokens.next().and_then(|token| ident(token, name));
                occurrence.filter(|_| tokens.next().is_some_and(|t| t.kind == TokenKind::Dot))
            }
        };
        if let Some(occurrence) = occurrence {
            self.add(
                occurrence.with_source(span.source_id()),
                Some(definition),
                format!("{name}: {ty}"),
            );
        }
    }

    fn method_name(&mut self, span: Span, name: &str) -> Option<Span> {
        let mut tokens = self.tokens(span);
        loop {
            let end = tokens
                .iter()
                .rposition(|token| token.kind == TokenKind::RParen)?;
            if end + 1 != tokens.len() {
                return None;
            }
            let mut depth = 0;
            let open = tokens[..end].iter().rposition(|token| match token.kind {
                TokenKind::RParen => {
                    depth += 1;
                    false
                }
                TokenKind::LParen if depth == 0 => true,
                TokenKind::LParen => {
                    depth -= 1;
                    false
                }
                _ => false,
            })?;
            if open == 0 {
                tokens = &tokens[1..end];
                continue;
            }
            let mut head = &tokens[..open];
            // Explicit method type arguments follow a double colon.
            if matches!(head.last()?.kind, TokenKind::Gt | TokenKind::Shr) {
                let mut depth = 0;
                let types = head.iter().rposition(|token| match token.kind {
                    TokenKind::Gt => {
                        depth += 1;
                        false
                    }
                    TokenKind::Shr => {
                        depth += 2;
                        false
                    }
                    TokenKind::Lt => {
                        depth -= 1;
                        depth == 0
                    }
                    _ => false,
                })?;
                let colon = types.checked_sub(1)?;
                if head[colon].kind != TokenKind::ColonColon {
                    return None;
                }
                head = &head[..colon];
            }
            let [.., dot, member] = head else { return None };
            if dot.kind != TokenKind::Dot {
                return None;
            }
            return Some(ident(member, name)?.with_source(span.source_id()));
        }
    }

    fn path_name(&mut self, span: Span, path: &str) -> Option<Span> {
        let mut tokens = self
            .tokens(span)
            .iter()
            .skip_while(|t| t.kind == TokenKind::LParen);
        let mut names = path.split('.');
        let mut name = ident(tokens.next()?, names.next()?)?;
        for segment in names {
            if tokens.next()?.kind != TokenKind::Dot {
                return None;
            }
            name = ident(tokens.next()?, segment)?;
        }
        Some(name.with_source(span.source_id()))
    }
}

pub(crate) fn function_display(item: &Item) -> String {
    let (name, params, ret, suspends, type_params) = match item {
        Item::FnDef(f) => (
            &f.name,
            &f.params,
            &f.ret_ty,
            f.suspends,
            f.type_params.as_slice(),
        ),
        Item::ExternFn(f) => (&f.name, &f.params, &f.ret_ty, f.suspends, &[][..]),
    };
    let function = Function {
        definition: None,
        name: name.rsplit('.').next().unwrap().to_owned(),
        parameters: params.iter().map(|p| p.name.clone()).collect(),
    };
    let signature =
        FunctionSignature::new(params.iter().map(|p| p.ty.clone()).collect(), ret.clone());
    let generic_names = type_params
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>();
    function.display(&signature, suspends, &generic_names)
}

impl Function {
    fn display(&self, signature: &FunctionSignature, suspends: bool, generics: &[&str]) -> String {
        let parameters = self
            .parameters
            .iter()
            .zip(&signature.params)
            .map(|(name, ty)| format!("{name}: {ty}"))
            .collect::<Vec<_>>();
        signature_display(&self.name, &parameters, suspends, &signature.ret, generics)
    }
}

pub(crate) fn signature_display(
    name: &str,
    parameters: &[String],
    suspends: bool,
    ret: &Type,
    generics: &[&str],
) -> String {
    let parameters = parameters.join(", ");
    let generics = if generics.is_empty() {
        String::new()
    } else {
        format!("<{}>", generics.join(", "))
    };
    let effect = if suspends { " @suspends" } else { "" };
    let ret = if *ret == Type::Unit {
        String::new()
    } else {
        format!(" -> {ret}")
    };
    format!("fn {name}{generics}({parameters}){effect}{ret}")
}

fn ident(token: &Token, name: &str) -> Option<Span> {
    matches!(&token.kind, TokenKind::Ident(written) if written == name).then_some(token.span)
}

fn flatten_tokens(tokens: Vec<Token>, out: &mut Vec<Token>) {
    for mut token in tokens {
        let nested = match &mut token.kind {
            TokenKind::Interpolated(parts) | TokenKind::SqlTemplate(parts) => std::mem::take(parts),
            _ => Vec::new(),
        };
        out.push(token);
        for part in nested {
            if let InterpolationPart::Expression(tokens) | InterpolationPart::Parameter(tokens) =
                part
            {
                flatten_tokens(tokens, out);
            }
        }
    }
}
