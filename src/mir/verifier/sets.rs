//! Dense, function-local sets. IDs must be validated before insertion;
//! membership queries for malformed IDs are deliberately bounds checked.

use std::marker::PhantomData;

use super::{MirBlockId, MirLocalId, MirValueId};

pub(super) trait SetIndex: Copy {
    fn index(self) -> usize;
    fn from_index(index: usize) -> Self;
}

macro_rules! set_index {
    ($($id:ident),*) => {$(
        impl SetIndex for $id {
            fn index(self) -> usize { self.0 }
            fn from_index(index: usize) -> Self { Self(index) }
        }
    )*};
}
set_index!(MirBlockId, MirLocalId, MirValueId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct IndexSet<I> {
    words: Vec<u64>,
    size: usize,
    index: PhantomData<I>,
}

impl<I: SetIndex> IndexSet<I> {
    pub(super) fn empty(size: usize) -> Self {
        Self {
            words: vec![0; size.div_ceil(64)],
            size,
            index: PhantomData,
        }
    }

    pub(super) fn full(size: usize) -> Self {
        let mut set = Self::empty(size);
        set.words.fill(u64::MAX);
        if !size.is_multiple_of(64) {
            *set.words.last_mut().unwrap() = (1 << (size % 64)) - 1;
        }
        set
    }

    pub(super) fn contains(&self, id: &I) -> bool {
        let i = id.index();
        i < self.size && self.words[i / 64] & (1 << (i % 64)) != 0
    }

    pub(super) fn insert(&mut self, id: I) -> bool {
        let i = id.index();
        assert!(i < self.size, "unvalidated MIR id inserted into a set");
        let mask = 1 << (i % 64);
        let word = &mut self.words[i / 64];
        let changed = *word & mask == 0;
        *word |= mask;
        changed
    }

    pub(super) fn remove(&mut self, id: &I) -> bool {
        let i = id.index();
        if i >= self.size {
            return false;
        }
        let mask = 1 << (i % 64);
        let word = &mut self.words[i / 64];
        let changed = *word & mask != 0;
        *word &= !mask;
        changed
    }

    pub(super) fn union_with(&mut self, other: &Self) {
        assert_eq!(self.size, other.size);
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word |= other;
        }
    }

    pub(super) fn intersect_with(&mut self, other: &Self) {
        assert_eq!(self.size, other.size);
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word &= other;
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = I> + '_ {
        indices(self.words.iter().copied())
    }

    pub(super) fn symmetric_difference<'a>(
        &'a self,
        other: &'a Self,
    ) -> impl Iterator<Item = I> + 'a {
        assert_eq!(self.size, other.size);
        indices(self.words.iter().zip(&other.words).map(|(a, b)| a ^ b))
    }
}

impl<I: SetIndex> Extend<I> for IndexSet<I> {
    fn extend<T: IntoIterator<Item = I>>(&mut self, iter: T) {
        for id in iter {
            self.insert(id);
        }
    }
}

fn indices<I: SetIndex>(words: impl Iterator<Item = u64>) -> impl Iterator<Item = I> {
    words.enumerate().flat_map(|(word_index, mut word)| {
        std::iter::from_fn(move || {
            if word == 0 {
                return None;
            }
            let bit = word.trailing_zeros() as usize;
            word &= word - 1;
            Some(I::from_index(word_index * 64 + bit))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn dense_sets_match_hash_sets_across_word_boundaries() {
        for size in [0, 1, 63, 64, 65, 127, 128, 129, 257] {
            let mut a = IndexSet::empty(size);
            let mut expected = HashSet::new();
            for i in (0..size).step_by(3).chain((0..size).step_by(7)) {
                assert_eq!(a.insert(MirValueId(i)), expected.insert(MirValueId(i)));
            }
            for i in (0..size).step_by(5) {
                assert_eq!(a.remove(&MirValueId(i)), expected.remove(&MirValueId(i)));
            }
            assert_eq!(a.iter().collect::<HashSet<_>>(), expected);
            assert!(!a.contains(&MirValueId(size)));
            assert!(!a.contains(&MirValueId(usize::MAX)));
            assert!(!a.remove(&MirValueId(usize::MAX)));
            let full = IndexSet::<MirValueId>::full(size);
            assert_eq!(full.iter().count(), size);
            let mut b = IndexSet::empty(size);
            b.extend((0..size).step_by(2).map(MirValueId));
            let expected_b = b.iter().collect::<HashSet<_>>();
            assert_eq!(
                a.symmetric_difference(&b).collect::<HashSet<_>>(),
                expected
                    .symmetric_difference(&expected_b)
                    .copied()
                    .collect()
            );
            let mut union = a.clone();
            union.union_with(&b);
            assert_eq!(
                union.iter().collect::<HashSet<_>>(),
                expected.union(&expected_b).copied().collect()
            );
            a.intersect_with(&b);
            assert_eq!(
                a.iter().collect::<HashSet<_>>(),
                expected.intersection(&expected_b).copied().collect()
            );
        }
    }
}
