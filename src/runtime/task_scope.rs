//! Local child registration shared by native and interpreted task scopes.
//!
//! A caller prepares captures before publication and retains responsibility on
//! publication failure. Published children belong to this scope until completion
//! hands them back for backend-specific cleanup. Tokens survive compaction and
//! are never reused; storage positions and frame pointers are not task identities.
//! This is the local policy, not a cross-thread execution or wake protocol.
use crate::{Error, ErrorCode, ErrorPhase};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ChildId(u64);

impl ChildId {
    fn allocate(counter: &AtomicU64) -> Result<Self, Error> {
        counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map(Self)
            .map_err(|_| {
                Error::new_with_code(
                    ErrorPhase::Runtime,
                    ErrorCode::ResourceLimit,
                    "Task identity space exhausted",
                )
            })
    }
}

static NEXT_CHILD: AtomicU64 = AtomicU64::new(1);

struct Child<T> {
    id: ChildId,
    value: Option<T>,
}

pub(crate) struct LocalTaskScope<T> {
    children: Vec<Child<T>>,
    cursor: usize,
    joining: bool,
    live: usize,
}

impl<T> Default for LocalTaskScope<T> {
    fn default() -> Self {
        Self {
            children: Vec::new(),
            cursor: 0,
            joining: false,
            live: 0,
        }
    }
}

impl<T> LocalTaskScope<T> {
    pub(crate) fn publish(&mut self, child: T) -> Result<ChildId, (Error, T)> {
        if self.joining {
            return Err((
                Error::new_with_code(
                    ErrorPhase::Runtime,
                    ErrorCode::InvalidState,
                    "Cannot publish a child into a joining task scope",
                ),
                child,
            ));
        }
        let id = match ChildId::allocate(&NEXT_CHILD) {
            Ok(id) => id,
            Err(error) => return Err((error, child)),
        };
        self.children.push(Child {
            id,
            value: Some(child),
        });
        self.live += 1;
        Ok(id)
    }

    pub(crate) fn close(&mut self) {
        self.joining = true;
    }

    pub(crate) fn live_len(&self) -> usize {
        self.live
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Registration order, with eligibility for the body's scheduling round.
    /// Joining scopes are advanced by their join site instead of that round.
    pub(crate) fn entries(&self) -> impl DoubleEndedIterator<Item = (ChildId, &T, bool)> {
        self.children
            .iter()
            .enumerate()
            .filter_map(|(index, child)| {
                child
                    .value
                    .as_ref()
                    .map(|value| (child.id, value, !self.joining && index >= self.cursor))
            })
    }

    pub(crate) fn entries_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = (ChildId, &mut T, bool)> {
        let cursor = self.cursor;
        let joining = self.joining;
        self.children
            .iter_mut()
            .enumerate()
            .filter_map(move |(index, child)| {
                child
                    .value
                    .as_mut()
                    .map(|value| (child.id, value, !joining && index >= cursor))
            })
    }

    fn index(&self, id: ChildId) -> Option<usize> {
        self.children
            .binary_search_by_key(&id, |child| child.id)
            .ok()
    }

    pub(crate) fn get(&self, id: ChildId) -> Option<&T> {
        self.children.get(self.index(id)?)?.value.as_ref()
    }

    pub(crate) fn get_mut(&mut self, id: ChildId) -> Option<&mut T> {
        let index = self.index(id)?;
        self.children[index].value.as_mut()
    }

    pub(crate) fn next(&mut self) -> Option<ChildId> {
        while self.cursor < self.children.len() && self.children[self.cursor].value.is_none() {
            self.cursor += 1;
        }
        self.children.get(self.cursor).map(|child| child.id)
    }

    /// Record a poll once. Completion returns ownership for cleanup; a stale
    /// token cannot remove a different child after compaction or registration.
    pub(crate) fn polled(&mut self, id: ChildId, complete: bool) -> Result<Option<T>, Error> {
        let index = self
            .index(id)
            .filter(|&index| index >= self.cursor && self.children[index].value.is_some())
            .ok_or_else(|| {
                Error::new_with_code(
                    ErrorPhase::Runtime,
                    ErrorCode::InvalidState,
                    "Unknown, completed, or already polled task child",
                )
            })?;
        self.cursor = self.cursor.max(index + 1);
        Ok(if complete {
            self.live -= 1;
            self.children[index].value.take()
        } else {
            None
        })
    }

    fn compact(&mut self) {
        self.cursor = self.children[..self.cursor]
            .iter()
            .filter(|child| child.value.is_some())
            .count();
        self.children.retain(|child| child.value.is_some());
        if self.children.capacity() > self.children.len().saturating_mul(4).max(16) {
            self.children.shrink_to(self.children.len().max(8));
        }
    }

    /// Finish a body round without restarting a join interrupted by slicing.
    pub(crate) fn finish_round(&mut self) {
        self.compact();
        if !self.joining {
            self.cursor = 0;
        }
    }

    pub(crate) fn restart_join_round(&mut self) {
        self.compact();
        self.cursor = 0;
    }

    #[cfg(test)]
    pub(crate) fn storage(&self) -> (usize, usize) {
        (self.children.len(), self.children.capacity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_scope_tokens_survive_compaction_and_reject_late_completion() {
        let mut scope = LocalTaskScope::default();
        let first = scope.publish(10).unwrap();
        let second = scope.publish(20).unwrap();
        scope.polled(first, false).unwrap();
        assert!(scope.polled(first, false).is_err());
        assert_eq!(scope.polled(second, true).unwrap(), Some(20));
        scope.finish_round();
        let third = scope.publish(30).unwrap();
        assert_eq!(scope.get(first), Some(&10));
        assert!(scope.polled(second, true).is_err());
        assert_eq!(scope.get(third), Some(&30));
        assert_eq!(scope.next(), Some(first));
        scope.polled(first, false).unwrap();
        assert_eq!(scope.next(), Some(third));
        let mut other = LocalTaskScope::default();
        let other_id = other.publish(40).unwrap();
        assert_ne!(other_id, first);
        assert_eq!(other.get(first), None);
        assert_eq!(scope.get(other_id), None);
    }

    #[test]
    fn task_scope_close_preserves_cursor_and_returns_rejected_ownership() {
        use std::cell::Cell;
        struct Owner<'a>(&'a Cell<usize>);
        impl Drop for Owner<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let drops = Cell::new(0);
        let mut scope = LocalTaskScope::default();
        let first = scope.publish(Owner(&drops)).unwrap_or_else(|_| panic!());
        let second = scope.publish(Owner(&drops)).unwrap_or_else(|_| panic!());
        scope.close();
        drop(scope.polled(first, true).unwrap());
        assert_eq!(drops.get(), 1);
        scope.finish_round();
        assert_eq!(scope.next(), Some(second));
        assert!(scope.entries().all(|(_, _, ready)| !ready));
        let (_, rejected) = scope.publish(Owner(&drops)).err().unwrap();
        assert_eq!(drops.get(), 1);
        drop(rejected);
        scope.polled(second, false).unwrap();
        assert_eq!(scope.next(), None);
        assert!(!scope.is_empty());
        scope.restart_join_round();
        assert_eq!(scope.next(), Some(second));
        drop(scope);
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn task_scope_identity_exhaustion_does_not_wrap() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(ChildId::allocate(&counter).unwrap(), ChildId(u64::MAX - 1));
        assert!(ChildId::allocate(&counter).is_err());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
