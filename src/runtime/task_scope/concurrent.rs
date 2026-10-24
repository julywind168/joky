//! Static groups share registration and local polling rules with dynamic scopes.
//! Selection does not release losers or deliver owning results: the backend must
//! retain all captures/results until its cleanup and result-transfer boundary.
use super::{ChildId, LocalTaskScope};
use crate::{Error, ErrorCode, ErrorPhase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionPolicy {
    All,
    First,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupPoll {
    Child(usize),
    Pending,
    Selected,
}

pub(crate) struct LocalTaskGroup {
    children: LocalTaskScope<usize>,
    policy: CompletionPolicy,
    active: Option<(ChildId, usize)>,
    selected: bool,
}

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}

impl LocalTaskGroup {
    pub(crate) fn new(policy: CompletionPolicy, count: usize) -> Result<Self, Error> {
        if count == 0 {
            return Err(invalid("A static task group requires children"));
        }
        let mut children = LocalTaskScope::default();
        let mut indices = Vec::new();
        indices.try_reserve_exact(count).map_err(|_| {
            Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Static task group capacity exhausted",
            )
        })?;
        indices.extend(0..count);
        children
            .publish_batch(indices)
            .map_err(|(error, _)| error)?;
        children.close();
        Ok(Self {
            children,
            policy,
            active: None,
            selected: false,
        })
    }

    pub(crate) fn active_index(&self) -> Option<usize> {
        self.active.map(|(_, index)| index)
    }

    pub(crate) fn is_selected(&self) -> bool {
        self.selected
    }

    pub(crate) fn next(&mut self) -> Result<GroupPoll, Error> {
        if self.active.is_some() {
            return Err(invalid(
                "A task child already holds the group's polling turn",
            ));
        }
        if self.selected {
            return Ok(GroupPoll::Selected);
        }
        if let Some(id) = self.children.next() {
            let index = *self.children.get(id).expect("registered static child");
            self.active = Some((id, index));
            return Ok(GroupPoll::Child(index));
        }
        self.children.restart_join_round();
        Ok(GroupPoll::Pending)
    }

    pub(crate) fn polled(&mut self, index: usize, complete: bool) -> Result<(), Error> {
        let (id, active) = self
            .active
            .ok_or_else(|| invalid("No task child holds a polling turn"))?;
        if active != index {
            return Err(invalid("Task completion does not match the active child"));
        }
        self.children.polled(id, complete)?;
        self.active = None;
        if complete && (self.policy == CompletionPolicy::First || self.children.is_empty()) {
            self.selected = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_group_join_keeps_source_positions_across_pending_rounds() {
        assert!(LocalTaskGroup::new(CompletionPolicy::All, 0).is_err());
        assert!(LocalTaskGroup::new(CompletionPolicy::All, usize::MAX).is_err());
        let mut group = LocalTaskGroup::new(CompletionPolicy::All, 3).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Child(0));
        assert!(group.next().is_err());
        assert!(group.polled(1, true).is_err());
        group.polled(0, false).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Child(1));
        group.polled(1, true).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Child(2));
        group.polled(2, false).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Pending);
        assert_eq!(group.next().unwrap(), GroupPoll::Child(0));
        group.polled(0, true).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Child(2));
        group.polled(2, true).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Selected);
        assert!(group.polled(2, true).is_err());
    }

    #[test]
    fn task_group_race_selection_retains_losers_for_cleanup() {
        let mut group = LocalTaskGroup::new(CompletionPolicy::First, 3).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Child(0));
        group.polled(0, false).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Child(1));
        group.polled(1, true).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Selected);
        assert_eq!(
            group
                .children
                .entries()
                .map(|(_, &i, _)| i)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        assert!(group.polled(0, true).is_err());
        let rejected = group.children.publish_batch(vec![3, 4]).unwrap_err();
        assert_eq!(rejected.1, [3, 4]);
        assert_eq!(group.children.live_len(), 2);
    }
}
