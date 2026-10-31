//! Cleanup owner lanes: keep the overwhelmingly common single lane inline.
use super::Buffer;
use std::ops::{Deref, DerefMut};

type Owner = Option<*mut Buffer>;

#[derive(Default)]
pub(crate) enum Owners {
    #[default]
    Empty,
    One(Owner),
    Many(Vec<Owner>),
}
impl Owners {
    pub(crate) fn push(&mut self, owner: Owner) {
        match self {
            Self::Empty => *self = Self::One(owner),
            Self::One(first) => *self = Self::Many(vec![*first, owner]),
            Self::Many(owners) => owners.push(owner),
        }
    }
}
impl Deref for Owners {
    type Target = [Owner];
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Empty => &[],
            Self::One(owner) => std::slice::from_ref(owner),
            Self::Many(owners) => owners,
        }
    }
}
impl DerefMut for Owners {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Empty => &mut [],
            Self::One(owner) => std::slice::from_mut(owner),
            Self::Many(owners) => owners,
        }
    }
}
impl FromIterator<Owner> for Owners {
    fn from_iter<T: IntoIterator<Item = Owner>>(iter: T) -> Self {
        let mut iter = iter.into_iter();
        let Some(first) = iter.next() else {
            return Self::Empty;
        };
        let Some(second) = iter.next() else {
            return Self::One(first);
        };
        let mut owners = Vec::with_capacity(iter.size_hint().0.saturating_add(2));
        owners.push(first);
        owners.push(second);
        owners.extend(iter);
        Self::Many(owners)
    }
}
impl<const N: usize> From<[Owner; N]> for Owners {
    fn from(owners: [Owner; N]) -> Self {
        owners.into_iter().collect()
    }
}
impl IntoIterator for Owners {
    type Item = Owner;
    type IntoIter = std::iter::Chain<std::option::IntoIter<Owner>, std::vec::IntoIter<Owner>>;
    fn into_iter(self) -> Self::IntoIter {
        let (first, rest) = match self {
            Self::Empty => (None, Vec::new()),
            Self::One(owner) => (Some(owner), Vec::new()),
            Self::Many(owners) => (None, owners),
        };
        first.into_iter().chain(rest)
    }
}
