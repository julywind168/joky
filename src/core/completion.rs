//! Cursor-local recovery and completion against a checked module.
use super::{
    imports::ImportScope,
    parser::{
        ast::{Item, Module},
        lexer::{InterpolationPart, Lexer, Token, TokenKind},
    },
};
use crate::{SourceId, Span, Type};
use std::{collections::BTreeMap, path::Path};

pub(crate) const MARKER: &str = "__joky_editor_completion";

/// The current word and a private source snapshot that can be passed to the checker.
pub struct CompletionSite {
    source: String,
    replacement: Span,
    prefix: String,
    member: bool,
    qualifier: Option<String>,
    marker: usize,
}

impl CompletionSite {
    /// Prepare a completion at a UTF-8 byte offset in the editor text.
    pub fn new(text: &str, offset: usize) -> Option<Self> {
        let before = text.get(..offset)?;
        let word = trailing_word(before);
        if !word.is_empty() && !starts_name(word) {
            return None;
        }
        let start = offset - word.len();
        let suffix = text[offset..]
            .char_indices()
            .find(|(_, ch)| !name_character(*ch))
            .map_or(text.len(), |(i, _)| offset + i);
        let mut source = text.to_owned();
        // Preserve the receiver so the ordinary planner can resolve its type.
        source.replace_range(start..suffix, MARKER);
        let tokens = Lexer::tokenize(&source).ok()?;
        let mut context = Vec::new();
        completion_context(&tokens, start, &mut context)?;
        let (tokens, token) = context[0];
        let preceding = &tokens[..token];
        let member = preceding.last().is_some_and(|t| t.kind == TokenKind::Dot);
        let qualifier = match preceding {
            [before @ .., receiver, dot]
                if dot.kind == TokenKind::Dot
                    && before.last().is_none_or(|t| t.kind != TokenKind::Dot) =>
            {
                match &receiver.kind {
                    TokenKind::Ident(name) => Some(name.clone()),
                    _ => None,
                }
            }
            _ => None,
        };
        let mut repairs = Vec::new();
        for (tokens, cursor) in context {
            repair_delimiters(tokens, cursor, &mut repairs);
        }
        for (offset, closers) in repairs.into_iter().rev() {
            source.insert_str(offset, &closers);
        }
        Some(Self {
            source,
            replacement: Span::new(start, suffix),
            prefix: word.into(),
            member,
            qualifier,
            marker: start,
        })
    }

    /// Source used only for this completion query. All changes start at the marker.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The range in the original text to replace when a candidate is accepted.
    pub fn replacement(&self) -> Span {
        self.replacement
    }
}

// Keep each interpolation's tokens separate so braces and delimiters cannot
// affect another hole or the surrounding source. Inner streams come first.
fn completion_context<'a>(
    tokens: &'a [Token],
    offset: usize,
    context: &mut Vec<(&'a [Token], usize)>,
) -> Option<()> {
    let index = tokens.partition_point(|token| token.span.end() <= offset);
    let token = tokens.get(index)?;
    if !token.span.contains(offset) {
        return None;
    }
    match &token.kind {
        TokenKind::Ident(name) if token.span.start() == offset && name == MARKER => {}
        TokenKind::Interpolated(parts) | TokenKind::SqlTemplate(parts) => {
            parts.iter().find_map(|part| match part {
                InterpolationPart::Expression(tokens) | InterpolationPart::Parameter(tokens) => {
                    completion_context(tokens, offset, context)
                }
                InterpolationPart::Text(..) => None,
            })?;
        }
        _ => return None,
    }
    context.push((tokens, index));
    Some(())
}

fn repair_delimiters(tokens: &[Token], cursor: usize, repairs: &mut Vec<(usize, String)>) {
    let mut delimiters = Vec::new();
    for (index, t) in tokens.iter().enumerate() {
        let end = match t.kind {
            TokenKind::LParen => Some(')'),
            TokenKind::LBracket => Some(']'),
            TokenKind::LBrace => Some('}'),
            _ => None,
        };
        if let Some(end) = end {
            delimiters.push((end, index));
            continue;
        }
        let closing = match t.kind {
            TokenKind::RParen => Some(')'),
            TokenKind::RBracket => Some(']'),
            TokenKind::RBrace => Some('}'),
            TokenKind::Eof => None,
            _ => continue,
        };
        // Only close delimiters which were already open at the cursor.
        let mut missing = String::new();
        while let Some(&(end, opening)) = delimiters.last() {
            if Some(end) == closing || opening > cursor || index <= cursor {
                break;
            }
            delimiters.pop();
            missing.push(end);
        }
        if !missing.is_empty() {
            if closing.is_none() && t.span.is_empty() {
                missing.insert(0, '\n');
            }
            repairs.push((t.span.start(), missing));
        }
        if delimiters
            .last()
            .is_some_and(|(end, _)| Some(*end) == closing)
        {
            delimiters.pop();
        }
    }
}

fn name_character(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn starts_name(word: &str) -> bool {
    word.starts_with(|ch: char| ch == '_' || ch.is_alphabetic())
}

fn trailing_word(text: &str) -> &str {
    text.rsplit(|ch| !name_character(ch))
        .next()
        .unwrap_or_default()
}

/// The source-level role of a candidate.
#[derive(Clone, Copy)]
pub enum CompletionKind {
    Function,
    Field,
    Method,
    Variable,
    Module,
    Type,
    Constructor,
    Constant,
}

/// A spelling visible to the user and its resolved type or signature.
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: String,
}

pub(crate) struct CompletionQuery {
    source: SourceId,
    marker: usize,
    prefix: String,
    pub(crate) member: bool,
    qualifier: Option<String>,
    best_span: Option<Span>,
    names: BTreeMap<String, CompletionItem>,
    locals: BTreeMap<String, CompletionItem>,
    hidden_module: bool,
    members: Option<BTreeMap<String, CompletionItem>>,
    method_parameters: BTreeMap<String, Vec<String>>,
}

impl CompletionQuery {
    pub(crate) fn new(module: &Module, path: &Path, site: &CompletionSite) -> Option<Self> {
        let file = module
            .sources()
            .files()
            .find(|f| f.path() == Some(path) && f.text() == site.source())?;
        let mut query = Self {
            source: file.id(),
            marker: site.marker,
            prefix: site.prefix.clone(),
            member: site.member,
            qualifier: site.qualifier.clone(),
            best_span: None,
            names: BTreeMap::new(),
            locals: BTreeMap::new(),
            hidden_module: false,
            members: None,
            method_parameters: BTreeMap::new(),
        };
        let scope = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(f) if f.span.source_id() == file.id() => {
                    f.scope.as_ref().map(|s| &s.imports)
                }
                _ => None,
            })
            .or(module.scope.as_ref());
        query.declarations(module, scope.map(AsRef::as_ref));
        Some(query)
    }

    fn declarations(&mut self, module: &Module, scope: Option<&ImportScope>) {
        let functions: BTreeMap<_, _> = module
            .items
            .iter()
            .map(|item| {
                let name = match item {
                    Item::FnDef(f) => &f.name,
                    Item::ExternFn(f) => &f.name,
                };
                (name.as_str(), item)
            })
            .collect();
        if self.member {
            if let Some((scope, qualifier)) = scope
                .zip(self.qualifier.clone())
                .filter(|(scope, qualifier)| scope.modules.contains_key(qualifier))
            {
                let qualified = format!("{qualifier}.");
                for (name, canonical) in scope.functions.range(qualified.clone()..) {
                    let Some(name) = name.strip_prefix(&qualified) else {
                        break;
                    };
                    if let Some(item) = functions.get(canonical.as_str()) {
                        self.function(name, item);
                    }
                }
            }
            return;
        }
        for item in &module.items {
            let (name, span) = match item {
                Item::FnDef(f) => (&f.name, f.span),
                Item::ExternFn(f) => (&f.name, f.span),
            };
            if span.source_id() == self.source {
                self.function(name.rsplit('.').next().unwrap(), item);
            }
        }
        let Some(scope) = scope else { return };
        for (name, canonical) in &scope.functions {
            if let Some(item) = functions.get(canonical.as_str()) {
                self.function(name, item);
            }
        }
        for (name, path) in &scope.modules {
            self.add(name, CompletionKind::Module, path.clone());
        }
        for (name, binding) in &scope.types {
            self.add(name, CompletionKind::Type, binding.name.clone());
        }
        for (name, binding) in &scope.traits {
            self.add(name, CompletionKind::Type, binding.name.clone());
        }
        for (name, bindings) in &scope.constructors {
            if let [binding] = bindings.as_slice() {
                self.add(
                    name,
                    CompletionKind::Constructor,
                    binding.enumeration.name.clone(),
                );
            }
        }
        for (name, canonical) in &scope.constants {
            if let Some(decl) = module.constants.iter().find(|decl| {
                decl.path.as_ref().map_or_else(
                    || decl.name == *canonical,
                    |path| {
                        canonical
                            .strip_prefix(path)
                            .and_then(|name| name.strip_prefix('.'))
                            == Some(&decl.name)
                    },
                )
            }) {
                self.add(name, CompletionKind::Constant, decl.ty.to_string());
            }
        }
    }

    fn function(&mut self, name: &str, item: &Item) {
        if self.matches(name) {
            self.add(
                name,
                CompletionKind::Function,
                super::semantics::function_display(item),
            );
        }
    }

    pub(crate) fn matches(&self, name: &str) -> bool {
        starts_name(name)
            && name.chars().all(name_character)
            && name != "_"
            && name != MARKER
            && name.starts_with(&self.prefix)
    }

    fn add(&mut self, name: &str, kind: CompletionKind, detail: String) {
        if self.matches(name) {
            self.names.insert(
                name.into(),
                CompletionItem {
                    label: name.into(),
                    kind,
                    detail,
                },
            );
        }
    }

    pub(crate) fn in_function(&self, span: Span) -> bool {
        span.source_id() == self.source && span.contains(self.marker)
    }

    pub(crate) fn contains(&self, span: Span) -> bool {
        self.in_function(span) && self.best_span.is_none_or(|old| span.len() < old.len())
    }

    pub(crate) fn scope<'a>(
        &mut self,
        span: Span,
        bindings: impl Iterator<Item = (&'a str, Type)>,
    ) {
        self.best_span = Some(span);
        self.locals.clear();
        self.hidden_module = false;
        for (name, ty) in bindings {
            if let Some(qualifier) = &self.qualifier {
                self.hidden_module |= qualifier == name;
            } else if !self.member && self.matches(name) {
                self.locals.insert(
                    name.into(),
                    CompletionItem {
                        label: name.into(),
                        kind: CompletionKind::Variable,
                        detail: ty.to_string(),
                    },
                );
            }
        }
    }

    pub(crate) fn finish(mut self) -> Vec<CompletionItem> {
        if let Some(members) = self.members {
            return members.into_values().collect();
        }
        if self.hidden_module {
            return Vec::new();
        }
        self.names.append(&mut self.locals);
        self.names.into_values().collect()
    }

    pub(crate) fn declare_method(&mut self, method: &super::parser::ast::FnDef) {
        if self.member {
            self.method_parameters.insert(
                method.name.clone(),
                method
                    .params
                    .iter()
                    .skip(1)
                    .map(|p| p.name.clone())
                    .collect(),
            );
        }
    }

    pub(crate) fn method_parameters(&self, name: &str) -> Option<&[String]> {
        self.method_parameters.get(name).map(Vec::as_slice)
    }

    pub(crate) fn value_receiver(&mut self) {
        self.members.get_or_insert_with(BTreeMap::new);
    }

    pub(crate) fn add_member(&mut self, name: &str, kind: CompletionKind, detail: String) {
        self.members
            .as_mut()
            .unwrap()
            .entry(name.into())
            .or_insert_with(|| CompletionItem {
                label: name.into(),
                kind,
                detail,
            });
    }
}

pub(crate) fn member_receiver<'a>(
    expr: &'a super::parser::ast::Expr,
    local_callee: impl FnOnce(&str, Span) -> Option<super::parser::ast::Expr>,
) -> Option<std::borrow::Cow<'a, super::parser::ast::Expr>> {
    use super::parser::ast::Expr;
    use std::borrow::Cow;
    let expr = match expr {
        Expr::FieldAssign { target, .. } => target.as_ref(),
        _ => expr,
    };
    match expr {
        Expr::Field { base, name, .. } if name == MARKER => Some(Cow::Borrowed(base)),
        Expr::MethodCall(call) if call.name == MARKER => Some(Cow::Borrowed(&call.receiver)),
        Expr::Call { callee, span, .. } => {
            let (receiver, member) = callee.rsplit_once('.')?;
            if member != MARKER {
                return None;
            }
            local_callee(receiver, *span).map(Cow::Owned)
        }
        _ => None,
    }
}
