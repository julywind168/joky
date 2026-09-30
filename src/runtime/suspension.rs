//! A single continuation protocol for generated JIT and AOT entries.
use super::{
    abi,
    cps::{CpsEntry, CpsFrame, DONE, FAILED, RUNTIME_ERROR},
};
use crate::{Error, ErrorCode, ErrorPhase};
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
    sources: std::sync::Arc<crate::SourceMap>,
}

enum ContinuationInner {
    Native { entry: CpsEntry, frame: CpsFrame },
    Interpreted(Box<dyn InterpretedTask>),
}

pub(crate) trait InterpretedTask {
    fn resume(&mut self) -> Result<(), ResumeError>;
    fn is_complete(&self) -> bool;
    fn result(&self) -> u64;
    fn wait_plan(&self) -> super::driver::WaitPlan;
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
            sources: Default::default(),
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
            sources: Default::default(),
        }
    }

    pub(crate) fn with_sources(mut self, sources: std::sync::Arc<crate::SourceMap>) -> Self {
        self.sources = sources;
        self
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

    pub(crate) fn wait_plan(&self) -> super::driver::WaitPlan {
        match &self.inner {
            ContinuationInner::Native { frame, .. } => frame.wait_plan(),
            ContinuationInner::Interpreted(task) => task.wait_plan(),
        }
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
                match unsafe { poll_root(*entry, frame) } {
                    Ok(RootPoll::Completed) => {
                        self.status = ContinuationStatus::Completed;
                        Ok(())
                    }
                    Ok(RootPoll::Pending) => Ok(()),
                    Err(ResumeError::Failed(error)) => {
                        Err(ResumeError::Failed(error.with_sources(&self.sources)))
                    }
                    Err(error) => Err(error),
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

/// Root entries require DONE on completion. Legacy host callbacks have a
/// different contract and continue to use abi::poll_cps directly.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RootPoll {
    Pending,
    Completed,
}

/// Poll a native root, validate its status, and release storage on every terminal
/// outcome. Scalar result bits remain readable only after successful completion.
/// # Safety
/// The live frame must be exclusively borrowed on its owning thread and match
/// the entry's complete slot layout. Keep all reachable code and host data alive
/// through cleanup. Only poll unfinished roots; entries must obey the CPS ABI.
pub(crate) unsafe fn poll_root(
    entry: CpsEntry,
    frame: &mut CpsFrame,
) -> Result<RootPoll, ResumeError> {
    let result = match abi::poll_cps(entry, frame) {
        Err(error) => Err(ResumeError::Failed(error)),
        Ok(0) if frame.state == DONE => Ok(RootPoll::Completed),
        Ok(0) => Err(ResumeError::MissingCompletionState),
        Ok(1) if !matches!(frame.state, DONE | FAILED) => Ok(RootPoll::Pending),
        Ok(1) => Err(ResumeError::Failed(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::InvalidState,
            "Pending task has a terminal state",
        ))),
        Ok(status) => Err(ResumeError::InvalidCpsStatus(status)),
    };
    if !matches!(result, Ok(RootPoll::Pending)) {
        if result.is_err() {
            frame.state = FAILED;
        }
        frame.release();
    }
    result
}

impl ResumeError {
    pub(crate) fn cps_status(&self) -> i32 {
        match self {
            Self::InvalidCpsStatus(status) => *status,
            _ => RUNTIME_ERROR,
        }
    }
}

/// Preserve detailed runtime failures and normalize protocol failures uniformly
/// at VM startup, blocking Rust, and C driver boundaries.
impl From<ResumeError> for Error {
    fn from(error: ResumeError) -> Self {
        let (code, message) = match error {
            ResumeError::Failed(error) => return error,
            ResumeError::InvalidCpsStatus(status) => (
                ErrorCode::HostError,
                format!("Suspending host returned failure status {status}"),
            ),
            ResumeError::MissingCompletionState => (
                ErrorCode::InvalidState,
                "Completed task did not set DONE".into(),
            ),
            ResumeError::AlreadyFinished => {
                (ErrorCode::InvalidState, "Task has already finished".into())
            }
        };
        Self::new_with_code(ErrorPhase::Runtime, code, message)
    }
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

    #[test]
    fn root_protocol_errors_match_c_driver_and_cleanup_before_task_drop() {
        use std::cell::Cell;
        thread_local! { static STOPS: Cell<usize> = const { Cell::new(0) }; }
        unsafe extern "C" fn stop(_: *mut CpsFrame, reason: u32) {
            assert_eq!(reason, super::super::borrowed_host::STOP_CANCELLED);
            STOPS.with(|count| count.set(count.get() + 1));
        }
        unsafe extern "C" fn malformed(frame: *mut CpsFrame) -> i32 {
            let child = super::super::cps::child(frame, 0);
            (*child).host_stop = Some(stop);
            let frame = &mut *frame;
            frame.state = frame.slots[0] as u32;
            frame.result = 42;
            frame.slots[1] as i32
        }
        for (state, status) in [(DONE, 1), (FAILED, 1), (DONE, 7), (0, 0)] {
            STOPS.with(|count| count.set(0));
            let frame = || {
                let mut frame = CpsFrame::new(2);
                frame.slots[0] = state as u64;
                frame.slots[1] = status as u64;
                frame
            };
            let mut task =
                unsafe { Continuation::new(malformed, frame(), ModuleId(1), Rc::new(())) };
            let failure = task.resume().unwrap_err();
            assert!(task.is_finished());
            assert_eq!(task.cps_result(), None);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
            let ContinuationInner::Native { frame, .. } = &task.inner else {
                unreachable!()
            };
            assert_eq!(frame.state, FAILED);
            assert_eq!(STOPS.with(Cell::get), 1);
            let expected_status = failure.cps_status();
            let expected_error = Error::from(failure);
            drop(task);
            assert_eq!(STOPS.with(Cell::get), 1);

            let mut frame = CpsFrame::new(2);
            frame.slots[0] = state as u64;
            frame.slots[1] = status as u64;
            let actual = super::super::error::checked(|| unsafe {
                assert_eq!(
                    super::super::driver::joky_cps_block_on(malformed, &mut frame),
                    expected_status
                );
            })
            .unwrap_err();
            assert_eq!(actual, expected_error);
            assert_eq!(frame.state, FAILED);
            assert_eq!(STOPS.with(Cell::get), 2);
            drop(frame);
            assert_eq!(STOPS.with(Cell::get), 2);
        }
    }
}
