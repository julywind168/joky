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
}

struct Function {
    definition: Option<Span>,
    name: String,
    parameters: Vec<String>,
}

impl SemanticRecorder {
    pub(crate) fn new(module: &Module) -> Recorder {
        Rc::new(RefCell::new(Self {
            index: SemanticIndex {
                sources: module.sources().clone(),
                symbols: BTreeMap::new(),
            },
            tokens: HashMap::new(),
            functions: HashMap::new(),
        }))
    }

    pub(crate) fn finish(&mut self) -> SemanticIndex {
        std::mem::take(self).index
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
        let name = canonical.rsplit('.').next().unwrap();
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

impl Function {
    fn display(&self, signature: &FunctionSignature, suspends: bool, generics: &[&str]) -> String {
        let parameters = self
            .parameters
            .iter()
            .zip(&signature.params)
            .map(|(name, ty)| format!("{name}: {ty}"))
            .collect::<Vec<_>>()
            .join(", ");
        let generics = if generics.is_empty() {
            String::new()
        } else {
            format!("<{}>", generics.join(", "))
        };
        let effect = if suspends { " @suspends" } else { "" };
        let ret = if signature.ret == Type::Unit {
            String::new()
        } else {
            format!(" -> {}", signature.ret)
        };
        format!("fn {}{generics}({parameters}){effect}{ret}", self.name)
    }
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
