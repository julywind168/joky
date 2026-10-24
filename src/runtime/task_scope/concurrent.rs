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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultState {
    Pending,
    Ready,
    Taken,
}

pub(crate) struct LocalTaskGroup {
    children: LocalTaskScope<usize>,
    policy: CompletionPolicy,
    active: Option<(ChildId, usize)>,
    cleanup_active: usize,
    results: Vec<ResultState>,
    completion_order: Vec<usize>,
    result_cursor: usize,
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
        let mut results = Vec::new();
        results.try_reserve_exact(count).map_err(|_| {
            Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Task result capacity exhausted",
            )
        })?;
        results.resize(count, ResultState::Pending);
        let mut completion_order = Vec::new();
        completion_order.try_reserve_exact(count).map_err(|_| {
            Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Task completion capacity exhausted",
            )
        })?;
        Ok(Self {
            children,
            policy,
            active: None,
            cleanup_active: usize::MAX,
            results,
            completion_order,
            result_cursor: 0,
        })
    }

    pub(crate) fn active_index(&self) -> Option<usize> {
        self.active
            .map(|(_, index)| index)
            .or_else(|| (self.cleanup_active != usize::MAX).then_some(self.cleanup_active))
    }

    pub(crate) fn is_selected(&self) -> bool {
        self.children.lifecycle.selected()
    }

    pub(crate) fn next(&mut self) -> Result<GroupPoll, Error> {
        if self.active.is_some() {
            return Err(invalid(
                "A task child already holds the group's polling turn",
            ));
        }
        if self.is_selected() {
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

    /// Enter cleanup with no execution owner active. Callbacks must run after
    /// this mutable borrow ends; reentry cannot begin a second cleanup pass.
    pub(crate) fn begin_drain(&mut self) -> Result<bool, Error> {
        if !self.is_selected() || self.active.is_some() {
            return Err(invalid("Task group has not selected its result"));
        }
        self.children.begin_drain()
    }

    pub(crate) fn finish_drain(&mut self, outcome: Result<(), Error>) -> Result<(), Error> {
        self.children.finish_drain(outcome)
    }

    pub(crate) fn drain_finished(&self) -> bool {
        self.children.lifecycle.drained()
    }

    fn require_drained(&self) -> Result<(), Error> {
        self.children.lifecycle.require_result()
    }

    pub(crate) fn stop(&mut self, failure: Option<Error>) {
        if let Some((_, index)) = self.active.take() {
            self.cleanup_active = index;
        }
        match failure {
            Some(error) => self.children.fail(error),
            None => self.children.cancel(),
        }
    }

    pub(crate) fn completion_order(&self) -> &[usize] {
        &self.completion_order
    }

    pub(crate) fn next_result(&mut self) -> Result<Option<usize>, Error> {
        self.require_drained()?;
        while let Some(&index) = self.completion_order.get(self.result_cursor) {
            if self.results[index] == ResultState::Ready {
                return Ok(Some(index));
            }
            self.result_cursor += 1;
        }
        Ok(None)
    }

    /// Claim a result once, after selection and successful child cleanup.
    /// Backends retain the packet until ownership has transferred or cleanup runs.
    pub(crate) fn take_result(&mut self, index: usize) -> Result<(), Error> {
        self.require_drained()?;
        if self.results.get(index) != Some(&ResultState::Ready) {
            return Err(invalid("Task result is not ready or was already taken"));
        }
        self.results[index] = ResultState::Taken;
        Ok(())
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
        if complete {
            self.results[index] = ResultState::Ready;
            self.completion_order.push(index);
        }
        if complete && (self.policy == CompletionPolicy::First || self.children.is_empty()) {
            self.children.lifecycle.complete();
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
        assert!(group.take_result(1).is_err());
        assert!(group.next_result().is_err());
        assert_eq!(group.next().unwrap(), GroupPoll::Child(2));
        group.polled(2, false).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Pending);
        assert_eq!(group.next().unwrap(), GroupPoll::Child(0));
        group.polled(0, true).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Child(2));
        group.polled(2, true).unwrap();
        assert_eq!(group.next().unwrap(), GroupPoll::Selected);
        assert!(group.polled(2, true).is_err());
        assert_eq!(group.completion_order(), [1, 0, 2]);
        assert!(group.next_result().is_err());
        assert!(group.take_result(1).is_err());
        assert!(group.finish_drain(Ok(())).is_err());
        assert!(group.begin_drain().unwrap());
        assert!(group.begin_drain().is_err());
        assert!(group.take_result(1).is_err());
        group.finish_drain(Ok(())).unwrap();
        assert!(!group.begin_drain().unwrap());
        for index in [1, 0, 2] {
            assert_eq!(group.next_result().unwrap(), Some(index));
            group.take_result(index).unwrap();
            assert!(group.take_result(index).is_err());
        }
        assert_eq!(group.next_result().unwrap(), None);
        assert!(group.take_result(3).is_err());
    }

    #[test]
    fn task_group_cleanup_failure_prevents_result_delivery_and_retry() {
        let mut group = LocalTaskGroup::new(CompletionPolicy::First, 2).unwrap();
        assert!(group.begin_drain().is_err());
        assert!(!group.drain_finished());
        assert_eq!(group.next().unwrap(), GroupPoll::Child(0));
        group.polled(0, true).unwrap();
        assert!(group.begin_drain().unwrap());
        let failure =
            Error::new_with_code(ErrorPhase::Runtime, ErrorCode::ResourceLimit, "Stop failed");
        assert!(group.finish_drain(Err(failure)).is_err());
        assert!(group.drain_finished());
        for error in [
            group.begin_drain().unwrap_err(),
            group.next_result().unwrap_err(),
            group.take_result(0).unwrap_err(),
        ] {
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
            assert_eq!(error.message(), "Stop failed");
        }
        assert!(group.finish_drain(Ok(())).is_err());
        assert_eq!(group.take_result(0).unwrap_err().message(), "Stop failed");
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
        assert!(group.take_result(0).is_err());
        assert!(group.take_result(1).is_err());
        assert!(group.begin_drain().unwrap());
        group.finish_drain(Ok(())).unwrap();
        assert_eq!(group.next_result().unwrap(), Some(1));
        group.take_result(1).unwrap();
        assert_eq!(group.next_result().unwrap(), None);
        assert!(group.take_result(1).is_err());
    }
}
