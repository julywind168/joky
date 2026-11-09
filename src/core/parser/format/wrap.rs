//! Width planning uses syntax-safe breaks, never a byte-column text splitter.
use super::{
    is_close, is_open,
    layout::{binary, space_between},
    syntax::Syntax,
    Piece, Role, TokenKind,
};
use std::{collections::BTreeSet, ops::Range};

pub(super) const WIDTH: usize = 100;

pub(super) fn after_operator(kind: &TokenKind, role: Role) -> bool {
    binary(kind, role) || (role == Role::Normal && *kind == TokenKind::Colon)
}

#[derive(Default)]
struct Group {
    end: usize,
    width: Option<usize>,
    multiline: bool,
}

pub(super) struct Wrap<'a> {
    syntax: &'a Syntax,
    pieces: &'a [Piece<'a>],
    groups: Vec<Option<Group>>,
    protected: Vec<bool>,
    forced: Vec<bool>,
    flat_gaps: Vec<bool>,
    signature_tails: Vec<Option<usize>>,
    return_widths: Vec<Option<usize>>,
    broken_chains: BTreeSet<usize>,
    pub changed: bool,
}

impl<'a> Wrap<'a> {
    pub fn new(syntax: &'a Syntax, pieces: &'a [Piece<'a>], skipped: &[Range<usize>]) -> Self {
        let mut groups: Vec<Option<Group>> = (0..pieces.len()).map(|_| None).collect();
        let mut widths = vec![0usize; pieces.len() + 1];
        let mut barriers = vec![0usize; pieces.len() + 1];
        let mut stack: Vec<usize> = Vec::new();
        let protected: Vec<_> = pieces
            .iter()
            .map(|piece| {
                let start = piece.end - piece.text.len();
                let region = skipped.partition_point(|range| range.end <= start);
                skipped
                    .get(region)
                    .is_some_and(|range| range.contains(&start))
            })
            .collect();
        let mut token_pieces = vec![pieces.len(); syntax.tokens.len()];
        for (i, piece) in pieces.iter().enumerate() {
            if let Some(token) = piece.token {
                token_pieces[token] = i;
            }
        }
        let mut flat_gaps = vec![false; pieces.len()];
        let mut signatures = Vec::new();
        for signature in &syntax.signatures {
            let open = token_pieces[signature.parameters];
            let close = token_pieces[syntax.pairs[signature.parameters].unwrap()];
            let end = token_pieces[signature.end - 1] + 1;
            // Reflow old return-type layouts, but never join across comments or
            // disabled regions. Parameter layout remains independently preserved.
            if pieces[close + 1..end]
                .iter()
                .enumerate()
                .all(|(offset, piece)| {
                    piece.token.is_some()
                        && !protected[close + 1 + offset]
                        && !piece.text.contains(['\r', '\n'])
                })
            {
                flat_gaps[close + 1..end].fill(true);
                signatures.push((signature, open, close, end));
            }
        }
        let mut broken_chains = BTreeSet::new();
        for (i, piece) in pieces.iter().enumerate() {
            let skipped = protected[i];
            let line_break = !flat_gaps[i] && piece.gap.contains(['\r', '\n']);
            let space = i > 0 && space_between(syntax, &pieces[i - 1], piece);
            widths[i + 1] = widths[i] + piece.text.chars().count() + usize::from(space);
            let role = piece.token.map_or(Role::Normal, |t| syntax.roles[t]);
            let block = matches!(
                role,
                Role::BlockOpen | Role::ArmsOpen | Role::DeclarationsOpen
            );
            barriers[i + 1] = barriers[i]
                + usize::from(skipped || block || piece.text.contains(['\r', '\n']))
                + usize::from(line_break);
            if line_break {
                if let Some(&open) = stack.last() {
                    if let Some(group) = &mut groups[open] {
                        group.multiline = true;
                    }
                }
            }
            let Some(t) = piece.token else { continue };
            if piece.gap.contains(['\r', '\n']) && !skipped {
                if let Some(root) = syntax.chain_root[t] {
                    broken_chains.insert(root);
                }
            }
            // A nonempty source block will expand even if it started on one line.
            // Give every containing argument/list group its own vertical layout.
            if block
                && syntax.pairs[t].is_some_and(|close| {
                    pieces
                        .get(i + 1)
                        .is_some_and(|next| next.token != Some(close))
                })
            {
                for &open in stack.iter().rev() {
                    let parent_role = pieces[open].token.map_or(Role::Normal, |t| syntax.roles[t]);
                    if matches!(
                        parent_role,
                        Role::BlockOpen | Role::ArmsOpen | Role::DeclarationsOpen
                    ) {
                        break;
                    }
                    groups[open].as_mut().unwrap().multiline = true;
                }
            }
            let kind = &syntax.tokens[t].kind;
            if is_close(kind, role) {
                if let Some(open) = stack.pop() {
                    if let Some(group) = &mut groups[open] {
                        group.end = i;
                        // The opening token's preceding gap is outside the group.
                        let own_barrier = usize::from(
                            !flat_gaps[open] && pieces[open].gap.contains(['\r', '\n']),
                        );
                        group.width = (barriers[i + 1] - barriers[open] == own_barrier).then_some(
                            widths[i + 1]
                                - widths[open]
                                - usize::from(
                                    open > 0
                                        && space_between(syntax, &pieces[open - 1], &pieces[open]),
                                ),
                        );
                    }
                }
            } else if is_open(kind, role) {
                groups[i] = Some(Group::default());
                stack.push(i);
            }
        }
        let mut signature_tails = vec![None; pieces.len()];
        let mut return_widths = vec![None; pieces.len()];
        for (signature, open, close, end) in signatures {
            let body = usize::from(signature.body) * 2; // Space and opening brace.
            signature_tails[open] = Some(widths[end] - widths[close + 1] + body);
            if let Some(result) = signature.result {
                let result = token_pieces[result];
                return_widths[result] = Some(
                    widths[end] - widths[result] + body
                        - usize::from(space_between(syntax, &pieces[result - 1], &pieces[result])),
                );
            }
        }
        Self {
            syntax,
            pieces,
            groups,
            protected,
            forced: vec![false; pieces.len()],
            flat_gaps,
            signature_tails,
            return_widths,
            broken_chains,
            changed: false,
        }
    }

    pub fn expand(&self, i: usize, column: usize) -> bool {
        if self.protected[i] {
            return false;
        }
        self.forced[i]
            || self.groups[i].as_ref().is_some_and(|group| {
                group.end > i + 1
                    && (group.multiline
                        || group.width.is_some_and(|width| {
                            column + width + self.signature_tails[i].unwrap_or(0) > WIDTH
                        }))
            })
    }

    pub fn force(&mut self, i: usize) {
        if !self.forced[i] && !self.protected[i] {
            self.forced[i] = true;
            self.changed = true;
        }
    }

    pub fn preserves_gap(&self, i: usize) -> bool {
        !self.flat_gaps[i]
    }

    pub fn break_before(&mut self, i: usize, column: usize, continuation_column: usize) -> bool {
        if i == 0 || self.protected[i] {
            return false;
        }
        // Once parameters have wrapped, keep a return type intact on a fresh
        // continuation line when it fits there; only larger types split inside.
        if let Some(width) = self.return_widths[i] {
            return column + width > WIDTH && continuation_column + width <= WIDTH;
        }
        let token = self.pieces[i].token;
        let previous = self.pieces[i - 1].token;
        let chain = token.is_some_and(|t| self.syntax.chain[t]);
        let operator = previous
            .is_some_and(|t| after_operator(&self.syntax.tokens[t].kind, self.syntax.roles[t]));
        let root = token.and_then(|t| self.syntax.chain_root[t]);
        if root.is_some_and(|root| self.broken_chains.contains(&root)) {
            return true;
        }
        let breaks = (chain || operator)
            && self
                .chunk_width(i, WIDTH.saturating_sub(column))
                .is_some_and(|width| column + width > WIDTH);
        if breaks {
            if let Some(root) = root {
                self.changed |= self.broken_chains.insert(root);
            }
        }
        breaks
    }

    fn chunk_width(&self, start: usize, available: usize) -> Option<usize> {
        let mut width = 0;
        let mut i = start;
        while i < self.pieces.len() {
            let piece = &self.pieces[i];
            if self.protected[i] || piece.text.contains(['\r', '\n']) {
                return None;
            }
            if i > start {
                if (!self.flat_gaps[i] && piece.gap.contains(['\r', '\n']))
                    || piece.token.is_some_and(|t| self.syntax.chain[t])
                    || self.pieces[i - 1].token.is_some_and(|t| {
                        after_operator(&self.syntax.tokens[t].kind, self.syntax.roles[t])
                    })
                {
                    break;
                }
                width += usize::from(space_between(self.syntax, &self.pieces[i - 1], piece));
            }
            if let Some(group) = &self.groups[i] {
                // Empty groups cannot wrap: budget both delimiters.
                if group.end == i + 1 {
                    width +=
                        piece.text.chars().count() + self.pieces[group.end].text.chars().count();
                    i = group.end + 1;
                    continue;
                }
                if let Some(flat) = group.width.filter(|flat| width + flat <= available) {
                    width += flat;
                    i = group.end + 1;
                    continue;
                }
                // A long group can wrap itself. Keep its short header attached
                // to `let x =`, an operator or a method name when that fits.
                return Some(width + piece.text.chars().count());
            }
            width += piece.text.chars().count();
            if piece.token.is_some_and(|t| {
                matches!(
                    self.syntax.tokens[t].kind,
                    TokenKind::Comma | TokenKind::Semi
                ) || is_close(&self.syntax.tokens[t].kind, self.syntax.roles[t])
                    || self.syntax.break_after[t]
            }) {
                break;
            }
            i += 1;
        }
        Some(width)
    }
}
