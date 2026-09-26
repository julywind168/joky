//! A single continuation protocol for generated JIT and AOT entries.
use super::cps::{CpsEntry, CpsFrame, DONE};
use std::{any::Any, rc::Rc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationStatus {
    Pending,
    Completed,
    Failed(ResumeError),
}

pub struct Continuation {
    entry: CpsEntry,
    frame: CpsFrame,
    module_version: ModuleId,
    status: ContinuationStatus,
    _code_owner: Rc<dyn Any>,
}

impl Continuation {
    /// # Safety
    /// The entry must match the frame layout. The owner must keep all reachable
    /// code alive; generated code must not unwind or retain borrowed frame pointers.
    pub(crate) unsafe fn new(
        entry: CpsEntry,
        frame: CpsFrame,
        module_version: ModuleId,
        owner: Rc<dyn Any>,
    ) -> Self {
        Self {
            entry,
            frame,
            module_version,
            status: ContinuationStatus::Pending,
            _code_owner: owner,
        }
    }

    pub fn module_version(&self) -> ModuleId {
        self.module_version
    }
    pub fn status(&self) -> ContinuationStatus {
        self.status
    }
    pub fn is_finished(&self) -> bool {
        self.status != ContinuationStatus::Pending
    }

    /// Raw return bits, available only after successful completion.
    pub fn cps_result(&self) -> Option<u64> {
        (self.status == ContinuationStatus::Completed).then_some(self.frame.result)
    }

    pub fn resume(&mut self) -> Result<(), ResumeError> {
        if self.is_finished() {
            return Err(ResumeError::AlreadyFinished);
        }
        let status = unsafe { (self.entry)(&mut self.frame) };
        let result = match status {
            0 if self.frame.state == DONE => {
                self.status = ContinuationStatus::Completed;
                Ok(())
            }
            0 => Err(ResumeError::MissingCompletionState),
            1 => Ok(()),
            value => Err(ResumeError::InvalidCpsStatus(value)),
        };
        if let Err(error) = result {
            self.status = ContinuationStatus::Failed(error);
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeError {
    AlreadyFinished,
    MissingCompletionState,
    InvalidCpsStatus(i32),
}

impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuation")
            .field("module_version", &self.module_version)
            .field("status", &self.status)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_completion_cannot_expose_a_result() {
        unsafe extern "C" fn missing_done(frame: *mut CpsFrame) -> i32 {
            (*frame).result = 42;
            0
        }
        let mut task =
            unsafe { Continuation::new(missing_done, CpsFrame::new(0), ModuleId(1), Rc::new(())) };
        assert_eq!(task.resume(), Err(ResumeError::MissingCompletionState));
        assert_eq!(task.cps_result(), None);
        assert_eq!(
            task.status(),
            ContinuationStatus::Failed(ResumeError::MissingCompletionState)
        );
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}
