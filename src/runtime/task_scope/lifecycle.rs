//! Terminal arbitration is separate from the cleanup barrier. Local backends
//! invoke the same transitions and run callbacks only after metadata borrows end.
use crate::{Error, ErrorCode, ErrorPhase};

enum Outcome {
    Complete,
    Failed(Box<Error>),
    Cancelled,
}

#[derive(Default, PartialEq, Eq)]
enum Drain {
    #[default]
    NotStarted,
    Running,
    Finished,
}

#[derive(Default)]
pub(super) struct Lifecycle {
    outcome: Option<Outcome>,
    drain: Drain,
}

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}

impl Lifecycle {
    pub(super) fn draining(&self) -> bool {
        self.drain == Drain::Running
    }
    pub(super) fn selected(&self) -> bool {
        self.outcome.is_some()
    }
    pub(super) fn complete(&mut self) {
        self.outcome.get_or_insert(Outcome::Complete);
    }
    pub(super) fn fail(&mut self, error: Error) {
        if self.outcome.is_none() {
            self.outcome = Some(Outcome::Failed(Box::new(error)));
        }
    }
    pub(super) fn cancel(&mut self) {
        self.outcome.get_or_insert(Outcome::Cancelled);
    }
    pub(super) fn begin_drain(&mut self) -> Result<bool, Error> {
        if !self.selected() {
            return Err(invalid("Task scope has no selected terminal outcome"));
        }
        match self.drain {
            Drain::NotStarted => {
                self.drain = Drain::Running;
                Ok(true)
            }
            Drain::Running => Err(invalid("Task scope cleanup is already running")),
            Drain::Finished => self.error().map(|()| false),
        }
    }
    pub(super) fn finish_drain(&mut self, cleanup: Result<(), Error>) -> Result<(), Error> {
        if self.drain != Drain::Running {
            return Err(invalid("Task scope cleanup was not started"));
        }
        if let Err(error) = cleanup {
            if matches!(self.outcome, Some(Outcome::Complete)) {
                self.outcome = Some(Outcome::Failed(Box::new(error)));
            }
        }
        self.drain = Drain::Finished;
        self.error()
    }
    pub(super) fn drained(&self) -> bool {
        self.drain == Drain::Finished
    }
    fn error(&self) -> Result<(), Error> {
        match &self.outcome {
            Some(Outcome::Failed(error)) => Err((**error).clone()),
            _ => Ok(()),
        }
    }
    pub(super) fn require_result(&self) -> Result<(), Error> {
        if !self.drained() {
            return Err(invalid("Task scope cleanup has not completed"));
        }
        self.error()?;
        if !matches!(self.outcome, Some(Outcome::Complete)) {
            return Err(invalid("A cancelled task scope cannot deliver results"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lifecycle_failure_and_cancellation_survive_late_completion() {
        for cancelled in [false, true] {
            let mut state = Lifecycle::default();
            assert!(state.begin_drain().is_err());
            if cancelled {
                state.cancel();
            } else {
                state.fail(invalid("Execution failed"));
            }
            state.complete();
            state.fail(invalid("Late failure"));
            assert!(state.begin_drain().unwrap());
            assert!(state.draining());
            assert!(state.begin_drain().is_err());
            assert!(state.require_result().is_err());
            let result = state.finish_drain(Err(invalid("Cleanup failed")));
            assert!(state.drained());
            if cancelled {
                result.unwrap();
                assert!(!state.begin_drain().unwrap());
            } else {
                assert_eq!(result.unwrap_err().message(), "Execution failed");
                assert_eq!(
                    state.begin_drain().unwrap_err().message(),
                    "Execution failed"
                );
            }
            assert!(state.require_result().is_err());
            assert!(state.finish_drain(Ok(())).is_err());
        }
    }
}
