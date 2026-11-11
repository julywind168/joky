//! Source roles and a position-independent parse shape. Neither depends on types
//! being resolved, generated declaration identities, or debug representations.
use super::{is_close, is_open, Role};
use crate::core::parser::{
    ast::{Expr, FnDef, Item, Module},
    lexer::{Lexer, Token, TokenKind},
    Diagnostic, Parser,
};
use crate::Span;
use std::{collections::BTreeMap, mem::Discriminant, ops::Range};

pub(super) struct Syntax {
    pub tokens: Vec<Token>,
    pub roles: Vec<Role>,
    pub pairs: Vec<Option<usize>>,
    pub break_after: Vec<bool>,
    pub continuation: Vec<bool>,
    pub anchors: Vec<Option<usize>>,
    pub chain: Vec<bool>,
    pub chain_root: Vec<Option<usize>>,
    pub top_level: Vec<(Range<usize>, bool)>,
    pub separators: Vec<Separator>,
    pub signatures: Vec<Signature>,
    shape: Shape,
}

pub(super) struct Signature {
    pub parameters: usize,
    pub result: Option<usize>,
    pub end: usize,
    pub body: bool,
}

pub(super) struct Separator {
    pub tokens: Range<usize>,
    pub next: usize,
}

#[derive(Default, PartialEq, Eq)]
struct Shape {
    declarations: Vec<Declaration>,
    expressions: Vec<Expression>,
}

#[derive(PartialEq, Eq)]
struct Declaration {
    kind: &'static str,
    range: Range<usize>,
    parts: Vec<Range<usize>>,
}

#[derive(PartialEq, Eq)]
struct Expression {
    kind: Discriminant<Expr>,
    range: Range<usize>,
    children: usize,
    trailing: bool,
}

impl Syntax {
    pub fn parse(source: &str) -> Result<Self, Diagnostic> {
        let raw = Lexer::tokenize(source)?;
        let mut parser = Parser::new(raw.clone(), source);
        parser.formatting = Some(Vec::new());
        let module = parser.parse_module()?;
        let hints: BTreeMap<_, _> = parser
            .formatting
            .take()
            .unwrap()
            .into_iter()
            .map(|(span, role)| (span.start(), role))
            .collect();
        let tokens = split_type_delimiters(source, raw, &hints);
        let roles: Vec<_> = tokens
            .iter()
            .map(|token| hints.get(&token.span.start()).copied().unwrap_or_default())
            .collect();
        let mut syntax = Self {
            pairs: vec![None; tokens.len()],
            break_after: vec![false; tokens.len()],
            continuation: vec![false; tokens.len()],
            anchors: vec![None; tokens.len()],
            chain: vec![false; tokens.len()],
            chain_root: vec![None; tokens.len()],
            top_level: Vec::new(),
            separators: Vec::new(),
            signatures: Vec::new(),
            tokens,
            roles,
            shape: Shape::default(),
        };
        let mut stack = Vec::new();
        let mut depths = vec![0; syntax.tokens.len()];
        for (i, token) in syntax.tokens.iter().enumerate() {
            if is_close(&token.kind, syntax.roles[i]) {
                if let Some(open) = stack.pop() {
                    syntax.pairs[open] = Some(i);
                    syntax.pairs[i] = Some(open);
                }
            }
            depths[i] = stack.len();
            if is_open(&token.kind, syntax.roles[i]) {
                stack.push(i);
            }
        }
        syntax.declarations(&module);
        syntax.top_level = syntax
            .shape
            .declarations
            .iter()
            .filter(|d| !d.range.is_empty() && depths[d.range.start] == 0)
            .map(|d| (d.range.clone(), d.kind == "function"))
            .collect();
        syntax.top_level.sort_by_key(|(range, _)| range.start);
        for root in module.expression_roots() {
            // Include root boundaries so moving an expression between declarations
            // cannot be hidden by flattening the pre-order traversal.
            syntax.shape.declarations.push(Declaration {
                kind: "expression root",
                range: syntax.range(root.span()),
                parts: Vec::new(),
            });
            let mut pending = vec![root];
            while let Some(expr) = pending.pop() {
                let children = expr.children();
                let range = syntax.range(expr.span());
                syntax.shape.expressions.push(Expression {
                    kind: std::mem::discriminant(expr),
                    range: range.clone(),
                    children: children.len(),
                    trailing: matches!(expr, Expr::Sequence { trailing: true, .. }),
                });
                match expr {
                    Expr::Block { exprs, span } => {
                        syntax.braces(*span, Role::BlockOpen, true);
                        syntax.separators(exprs, *span, true);
                    }
                    Expr::Unsafe { exprs, span } => {
                        syntax.braces(*span, Role::BlockOpen, false);
                        syntax.separators(exprs, *span, false);
                    }
                    Expr::Graph { span, .. } => {
                        syntax.braces(*span, Role::BlockOpen, false);
                    }
                    Expr::Concurrent { span, .. }
                    | Expr::Match {
                        conditional: false,
                        iteration: false,
                        span,
                        ..
                    } => {
                        syntax.braces(*span, Role::ArmsOpen, false);
                    }
                    _ => {}
                }
                match expr {
                    Expr::Call { callee, .. } if callee.contains('.') => {
                        // Identifier-rooted calls are folded into a dotted name
                        // by the parser; their receiver dots have no Field nodes.
                        let end = range
                            .clone()
                            .take_while(|&i| {
                                matches!(
                                    syntax.tokens[i].kind,
                                    TokenKind::Ident(_) | TokenKind::Dot | TokenKind::TupleIndex(_)
                                )
                            })
                            .last()
                            .map_or(range.start, |i| i + 1);
                        let path: String = syntax.tokens[range.start..end]
                            .iter()
                            .map(|t| &source[t.span.start()..t.span.end()])
                            .collect();
                        if path == *callee {
                            for i in range.start..end {
                                if syntax.tokens[i].kind == TokenKind::Dot {
                                    syntax.chain[i] = true;
                                    syntax.chain_root[i] = Some(range.start);
                                }
                            }
                        }
                    }
                    Expr::MethodCall(call) => syntax.chain(&call.receiver),
                    Expr::Field { base, .. } => syntax.chain(base),
                    _ => {}
                }
                match expr {
                    Expr::If {
                        then_branch,
                        else_branch,
                        span,
                        ..
                    } => {
                        syntax.anchor(then_branch, *span);
                        if let Some(body) = else_branch {
                            syntax.anchor(body, *span);
                        }
                    }
                    Expr::IfChain {
                        arms,
                        fallback,
                        span,
                    } => {
                        for (_, body) in arms {
                            syntax.anchor(body, *span);
                        }
                        if let Some(body) = fallback {
                            syntax.anchor(body, *span);
                        }
                    }
                    Expr::While { body, span, .. }
                    | Expr::Isolate { body, span }
                    | Expr::TaskScope { body, span }
                    | Expr::Branch { body, span } => syntax.anchor(body, *span),
                    Expr::AnonymousFunction(f) => syntax.anchor(&f.body, f.span),
                    Expr::IndexedLoop(loop_) => syntax.anchor(&loop_.body, loop_.span),
                    Expr::Match {
                        arms,
                        span,
                        conditional,
                        ..
                    } => {
                        for arm in arms {
                            syntax.anchor(
                                &arm.body,
                                if *conditional {
                                    *span
                                } else {
                                    arm.pattern.span()
                                },
                            );
                        }
                    }
                    Expr::LetElse(binding) => syntax.anchor(&binding.fallback, binding.span),
                    Expr::When(w) => {
                        syntax.anchor(&w.body, w.span);
                        for body in w.missing.iter().chain(w.alias.iter()) {
                            syntax.anchor(body, w.span);
                        }
                    }
                    _ => {}
                }
                if matches!(
                    expr,
                    Expr::Let { .. }
                        | Expr::LetElse(_)
                        | Expr::Var { .. }
                        | Expr::Assign { .. }
                        | Expr::Destructure { .. }
                        | Expr::FieldAssign { .. }
                        | Expr::BinOp { .. }
                        | Expr::BinaryChain { .. }
                        | Expr::Logical { .. }
                        | Expr::Unary { .. }
                        | Expr::Call { .. }
                        | Expr::Invoke { .. }
                        | Expr::MethodCall(_)
                        | Expr::Field { .. }
                        | Expr::Index { .. }
                        | Expr::Return { .. }
                ) && !range.is_empty()
                {
                    for i in range.start + 1..range.end {
                        if depths[i] == depths[range.start]
                            && !is_close(&syntax.tokens[i].kind, syntax.roles[i])
                        {
                            syntax.continuation[i] = true;
                        }
                    }
                }
                pending.extend(children.into_iter().rev());
            }
        }
        Ok(syntax)
    }

    pub fn same_structure(&self, other: &Self) -> bool {
        self.roles == other.roles && self.shape == other.shape
    }

    fn range(&self, span: Span) -> Range<usize> {
        let start = self
            .tokens
            .partition_point(|t| t.span.end() <= span.start());
        let end = self.tokens.partition_point(|t| t.span.start() < span.end());
        start.min(end)..end
    }

    fn chain(&mut self, receiver: &Expr) {
        let dot = self.range(receiver.span()).end;
        if self
            .tokens
            .get(dot)
            .is_some_and(|t| t.kind == TokenKind::Dot)
        {
            self.chain[dot] = true;
            self.chain_root[dot] = Some(self.range(receiver.span()).start);
        }
    }

    fn separators(&mut self, statements: &[Expr], span: Span, exact_start: bool) {
        let range = self.range(span);
        let Some(close) = range.end.checked_sub(1) else {
            return;
        };
        let Some(open) = self.pairs[close] else {
            return;
        };
        if self.tokens[close].kind != TokenKind::RBrace
            || open < range.start
            || (exact_start && open != range.start)
        {
            return;
        }
        for statement in statements {
            let Expr::Sequence { exprs, .. } = statement else {
                continue;
            };
            let Some(first) = exprs.first() else { continue };
            // Parenthesized sequences keep their expression context. Generated
            // blocks are excluded by the real-brace check above.
            if self.range(statement.span()).start != self.range(first.span()).start {
                continue;
            }
            for pair in exprs.windows(2) {
                // These forms may stand alone without unused_value diagnostics.
                // Calls need no return-type inference: non-tail call results may
                // already be implicitly discarded by the language.
                if !matches!(
                    pair[0],
                    Expr::Let { .. }
                        | Expr::LetElse(_)
                        | Expr::Var { .. }
                        | Expr::Destructure { .. }
                        | Expr::Assign { .. }
                        | Expr::FieldAssign { .. }
                        | Expr::Call { .. }
                        | Expr::Invoke { .. }
                        | Expr::MethodCall(_)
                        | Expr::Return { .. }
                        | Expr::Break { .. }
                        | Expr::Continue { .. }
                ) {
                    continue;
                }
                let end = self.range(pair[0].span()).end;
                let next = self.range(pair[1].span()).start;
                if end < next
                    && self.tokens[end..next]
                        .iter()
                        .all(|t| t.kind == TokenKind::Semi)
                {
                    self.separators.push(Separator {
                        tokens: end..next,
                        next,
                    });
                }
            }
        }
    }

    fn declaration(&mut self, kind: &'static str, span: Span, parts: impl Iterator<Item = Span>) {
        let range = self.range(span);
        if matches!(kind, "function" | "extern" | "trait method") {
            self.signature(range.clone());
        }
        if !range.is_empty() {
            self.break_after[range.end - 1] = true;
        }
        self.shape.declarations.push(Declaration {
            kind,
            range,
            parts: parts.map(|span| self.range(span)).collect(),
        });
    }

    fn signature(&mut self, range: Range<usize>) {
        let Some(function) = range
            .clone()
            .find(|&i| self.tokens[i].kind == TokenKind::Fn)
        else {
            return;
        };
        let mut i = function + 1;
        // Skip generic bounds, including their callable parameter lists.
        while i < range.end && self.tokens[i].kind != TokenKind::LParen {
            i = self.pairs[i].filter(|&close| close > i).unwrap_or(i) + 1;
        }
        let Some(close) = self.pairs.get(i).copied().flatten() else {
            return;
        };
        let parameters = i;
        let mut result = None;
        i = close + 1;
        while i < range.end && !matches!(self.tokens[i].kind, TokenKind::LBrace | TokenKind::Semi) {
            if self.tokens[i].kind == TokenKind::Arrow && result.is_none() {
                result = Some(i + 1);
            }
            i = self.pairs[i].filter(|&close| close > i).unwrap_or(i) + 1;
        }
        self.signatures.push(Signature {
            parameters,
            result,
            end: i,
            body: i < range.end && self.tokens[i].kind == TokenKind::LBrace,
        });
    }

    fn function(&mut self, f: &FnDef) {
        self.declaration("function", f.span, f.params.iter().map(|p| p.span));
        self.anchor(&f.body, f.span);
    }

    fn declarations(&mut self, module: &Module) {
        for import in &module.imports {
            // The parser also inserts imports for desugared language features.
            let range = self.range(import.span);
            if self
                .tokens
                .get(range.start)
                .is_some_and(|t| t.kind == TokenKind::Import)
            {
                self.declaration("import", import.span, import.members.iter().map(|m| m.span));
            }
        }
        for item in &module.items {
            match item {
                Item::FnDef(f) => self.function(f),
                Item::ExternFn(f) => {
                    self.declaration("extern", f.span, f.params.iter().map(|p| p.span));
                }
            }
        }
        for record in &module.records {
            self.declaration("record", record.span, record.fields.iter().map(|p| p.span));
        }
        for enumeration in &module.enums {
            self.declaration(
                "enum",
                enumeration.span,
                enumeration.variants.iter().map(|v| v.span),
            );
        }
        for alias in &module.type_aliases {
            self.declaration("alias", alias.span, std::iter::once(alias.target_span));
        }
        for constant in &module.constants {
            self.declaration(
                "constant",
                constant.span,
                std::iter::once(constant.value.span()),
            );
        }
        for implementation in &module.implementations {
            self.declaration("impl", implementation.span, std::iter::empty());
            self.braces(implementation.span, Role::DeclarationsOpen, false);
            for method in &implementation.methods {
                self.function(method);
            }
            for ty in &implementation.associated_types {
                self.declaration("associated type", ty.span, std::iter::once(ty.target_span));
            }
            for constant in &implementation.constants {
                self.declaration(
                    "associated constant",
                    constant.span,
                    std::iter::once(constant.value.span()),
                );
            }
        }
        for definition in &module.traits {
            self.declaration("trait", definition.span, std::iter::empty());
            self.braces(definition.span, Role::DeclarationsOpen, false);
            for method in &definition.methods {
                self.declaration(
                    "trait method",
                    method.span,
                    method.params.iter().map(|p| p.span),
                );
                if let Some(body) = &method.body {
                    self.anchor(body, method.span);
                }
            }
            for ty in &definition.associated_types {
                self.declaration("associated type", ty.span, std::iter::empty());
            }
        }
    }

    fn braces(&mut self, span: Span, role: Role, exact_start: bool) {
        let range = self.range(span);
        let Some(close) = range.end.checked_sub(1) else {
            return;
        };
        if self.tokens[close].kind != TokenKind::RBrace {
            return;
        }
        if let Some(open) = self.pairs[close] {
            // Generated blocks may borrow a containing expression's span. Only
            // real source braces are allowed to acquire a block layout.
            if open >= range.start && (!exact_start || open == range.start) {
                self.roles[open] = role;
                self.roles[close] = match role {
                    Role::BlockOpen => Role::BlockClose,
                    Role::ArmsOpen => Role::ArmsClose,
                    Role::DeclarationsOpen => Role::DeclarationsClose,
                    _ => unreachable!(),
                };
                if !exact_start && range.start < open {
                    self.anchors[open] = Some(range.start);
                }
            }
        }
    }

    /// A block aligns with its header, including when a condition or signature
    /// ends on an indented continuation line. Desugared loops retain source spans.
    fn anchor(&mut self, body: &Expr, header: Span) {
        let range = self.range(body.span());
        let Some(close) = range.end.checked_sub(1) else {
            return;
        };
        if self.tokens[close].kind == TokenKind::RBrace {
            if let Some(open) = self.pairs[close] {
                let anchor = self.range(header).start;
                if open >= range.start && anchor < open {
                    self.anchors[open] = Some(anchor);
                }
            }
        }
    }
}

/// Treat type punctuation the way the parser consumes it. This permits `> >`
/// and `>>` to share a canonical spelling without confusing a shift with types.
fn split_type_delimiters(
    source: &str,
    raw: Vec<Token>,
    hints: &BTreeMap<usize, Role>,
) -> Vec<Token> {
    let mut result = Vec::with_capacity(raw.len());
    for token in raw {
        let angles: Vec<_> = hints
            .range(token.span.start()..token.span.end())
            .filter_map(|(&offset, role)| {
                matches!(role, Role::TypeOpen | Role::TypeClose).then_some(offset)
            })
            .collect();
        if angles.is_empty() {
            result.push(token);
            continue;
        }
        let mut cursor = token.span.start();
        for offset in angles.into_iter().chain(std::iter::once(token.span.end())) {
            if cursor < offset {
                result.push(punctuation(source, &token, cursor..offset));
            }
            if offset < token.span.end() {
                result.push(punctuation(source, &token, offset..offset + 1));
            }
            cursor = offset + 1;
        }
    }
    result
}

fn punctuation(source: &str, original: &Token, range: Range<usize>) -> Token {
    let kind = match &source[range.clone()] {
        "<" => TokenKind::Lt,
        ">" => TokenKind::Gt,
        "=" => TokenKind::Eq,
        "<<" => TokenKind::Shl,
        ">>" => TokenKind::Shr,
        ">=" => TokenKind::Ge,
        ">>=" => TokenKind::CompoundAssign(crate::parser::ast::BinOp::Shr),
        _ => unreachable!("only type punctuation is split"),
    };
    Token {
        kind,
        span: Span::from(range).with_source(original.span.source_id()),
        line: original.line,
        end_line: original.end_line,
    }
}
