//! Width planning uses syntax-safe breaks, never a byte-column text splitter.
use super::{
    is_close, is_open,
    layout::{binary, space_between},
    syntax::Syntax,
    Piece, Role, TokenKind,
};
use std::ops::Range;

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
    pub changed: bool,
}

impl<'a> Wrap<'a> {
    pub fn new(syntax: &'a Syntax, pieces: &'a [Piece<'a>], skipped: &[Range<usize>]) -> Self {
        let mut groups: Vec<Option<Group>> = (0..pieces.len()).map(|_| None).collect();
        let mut widths = vec![0usize; pieces.len() + 1];
        let mut barriers = vec![0usize; pieces.len() + 1];
        let mut stack: Vec<usize> = Vec::new();
        let mut protected = Vec::with_capacity(pieces.len());
        for (i, piece) in pieces.iter().enumerate() {
            let start = piece.end - piece.text.len();
            let region = skipped.partition_point(|range| range.end <= start);
            let skipped = skipped
                .get(region)
                .is_some_and(|range| range.contains(&start));
            protected.push(skipped);
            let space = i > 0 && space_between(syntax, &pieces[i - 1], piece);
            widths[i + 1] = widths[i] + piece.text.chars().count() + usize::from(space);
            let role = piece.token.map_or(Role::Normal, |t| syntax.roles[t]);
            let block = matches!(
                role,
                Role::BlockOpen | Role::ArmsOpen | Role::DeclarationsOpen
            );
            barriers[i + 1] = barriers[i]
                + usize::from(skipped || block || piece.text.contains(['\r', '\n']))
                + usize::from(piece.gap.contains(['\r', '\n']));
            if piece.gap.contains(['\r', '\n']) {
                if let Some(&open) = stack.last() {
                    if let Some(group) = &mut groups[open] {
                        group.multiline = true;
                    }
                }
            }
            let Some(t) = piece.token else { continue };
            let kind = &syntax.tokens[t].kind;
            if is_close(kind, role) {
                if let Some(open) = stack.pop() {
                    if let Some(group) = &mut groups[open] {
                        group.end = i;
                        // The opening token's preceding gap is outside the group.
                        let own_barrier = usize::from(pieces[open].gap.contains(['\r', '\n']));
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
        Self {
            syntax,
            pieces,
            groups,
            protected,
            forced: vec![false; pieces.len()],
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
                    && (group.multiline || group.width.is_some_and(|width| column + width > WIDTH))
            })
    }

    pub fn force(&mut self, i: usize) {
        if !self.forced[i] && !self.protected[i] {
            self.forced[i] = true;
            self.changed = true;
        }
    }

    pub fn break_before(&self, i: usize, column: usize) -> bool {
        if i == 0 || self.protected[i] {
            return false;
        }
        let token = self.pieces[i].token;
        let previous = self.pieces[i - 1].token;
        let chain = token.is_some_and(|t| self.syntax.chain[t]);
        let operator = previous
            .is_some_and(|t| after_operator(&self.syntax.tokens[t].kind, self.syntax.roles[t]));
        (chain || operator)
            && self
                .chunk_width(i, WIDTH.saturating_sub(column))
                .is_some_and(|width| column + width > WIDTH)
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
                if piece.gap.contains(['\r', '\n'])
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
