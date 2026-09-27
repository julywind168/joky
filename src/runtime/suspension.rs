//! A single continuation protocol for generated JIT and AOT entries.
use super::{
    abi,
    cps::{CpsEntry, CpsFrame, DONE},
};
use crate::Error;
use std::{any::Any, rc::Rc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationStatus {
    Pending,
    Completed,
    Failed(ResumeError),
}

pub struct Continuation {
    inner: ContinuationInner,
    module_version: ModuleId,
    status: ContinuationStatus,
    _code_owner: Rc<dyn Any>,
}

enum ContinuationInner {
    Native { entry: CpsEntry, frame: CpsFrame },
    Interpreted(Box<dyn InterpretedTask>),
}

pub(crate) trait InterpretedTask {
    fn resume(&mut self) -> Result<(), ResumeError>;
    fn is_complete(&self) -> bool;
    fn result(&self) -> u64;
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
            inner: ContinuationInner::Native { entry, frame },
            module_version,
            status: ContinuationStatus::Pending,
            _code_owner: owner,
        }
    }

    pub(crate) fn new_interpreted(
        task: Box<dyn InterpretedTask>,
        module_version: ModuleId,
        owner: Rc<dyn Any>,
    ) -> Self {
        Self {
            inner: ContinuationInner::Interpreted(task),
            module_version,
            status: ContinuationStatus::Pending,
            _code_owner: owner,
        }
    }

    pub fn module_version(&self) -> ModuleId {
        self.module_version
    }
    pub fn status(&self) -> ContinuationStatus {
        self.status.clone()
    }
    /// Borrowed status; avoids cloning the stored error when only inspecting.
    pub fn status_ref(&self) -> &ContinuationStatus {
        &self.status
    }
    pub fn is_finished(&self) -> bool {
        self.status != ContinuationStatus::Pending
    }

    /// Raw return bits, available only after successful completion.
    pub fn cps_result(&self) -> Option<u64> {
        if self.status != ContinuationStatus::Completed {
            return None;
        }
        Some(match &self.inner {
            ContinuationInner::Native { frame, .. } => frame.result,
            ContinuationInner::Interpreted(task) => task.result(),
        })
    }

    pub fn resume(&mut self) -> Result<(), ResumeError> {
        if self.is_finished() {
            return Err(ResumeError::AlreadyFinished);
        }
        let result = match &mut self.inner {
            ContinuationInner::Native { entry, frame } => {
                match unsafe { abi::poll_cps(*entry, frame) } {
                    Err(error) => {
                        frame.state = super::cps::FAILED;
                        unsafe { super::cps::release_children(frame) };
                        Err(ResumeError::Failed(error))
                    }
                    Ok(0) if frame.state == DONE => {
                        self.status = ContinuationStatus::Completed;
                        Ok(())
                    }
                    Ok(0) => Err(ResumeError::MissingCompletionState),
                    Ok(1) => Ok(()),
                    Ok(value) => Err(ResumeError::InvalidCpsStatus(value)),
                }
            }
            ContinuationInner::Interpreted(task) => {
                let result = task.resume();
                if result.is_ok() && task.is_complete() {
                    self.status = ContinuationStatus::Completed;
                }
                result
            }
        };
        if let Err(error) = &result {
            self.status = ContinuationStatus::Failed(error.clone());
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeError {
    AlreadyFinished,
    MissingCompletionState,
    InvalidCpsStatus(i32),
    /// A detailed runtime or ABI failure; carries the structured error
    /// instead of collapsing it into a numeric status.
    Failed(Error),
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
