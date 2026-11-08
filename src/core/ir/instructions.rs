//! IR instructions and source locations with a shared length and ordering.
//! Keep the separate vectors for existing instruction-only analysis and lowering.
use super::{Op, Span};
use std::ops::{Deref, DerefMut};

#[derive(Default)]
pub(crate) struct Instructions {
    ops: Vec<Op>,
    spans: Vec<Span>,
}

impl Instructions {
    pub(super) fn push(&mut self, op: Op, span: Span) {
        self.ops.push(op);
        self.spans.push(span);
    }

    pub(super) fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub(crate) fn iter_spanned(&self) -> impl DoubleEndedIterator<Item = (&Op, &Span)> {
        self.ops.iter().zip(&self.spans)
    }

    pub(super) fn iter_spanned_mut(&mut self) -> impl Iterator<Item = (&mut Op, &mut Span)> {
        self.ops.iter_mut().zip(&mut self.spans)
    }

    /// Rebuild a pass from owned pairs without borrowing the plan while it emits.
    pub(super) fn take_spanned(
        &mut self,
    ) -> std::iter::Zip<std::vec::IntoIter<Op>, std::vec::IntoIter<Span>> {
        std::mem::take(&mut self.ops)
            .into_iter()
            .zip(std::mem::take(&mut self.spans))
    }

    pub(super) fn retain_mut(&mut self, mut keep: impl FnMut(&mut Op) -> bool) {
        let mut spans = Vec::with_capacity(self.spans.len());
        let mut index = 0;
        self.ops.retain_mut(|op| {
            let span = self.spans[index];
            index += 1;
            if !keep(op) {
                return false;
            }
            spans.push(span);
            true
        });
        self.spans = spans;
    }
}

// Slice access permits replacement, but cannot change the number of instructions.
impl Deref for Instructions {
    type Target = [Op];

    fn deref(&self) -> &Self::Target {
        &self.ops
    }
}

impl DerefMut for Instructions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ops
    }
}

impl<'a> IntoIterator for &'a Instructions {
    type Item = &'a Op;
    type IntoIter = std::slice::Iter<'a, Op>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut Instructions {
    type Item = &'a mut Op;
    type IntoIter = std::slice::IterMut<'a, Op>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}
