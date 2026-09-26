//! Continuations across native and JIT boundaries.
//!
//! Native continuations resume once; CPS continuations may remain pending.
//! Terminal tasks reject further polls and retain their code owners until dropped.

use super::cps::{CpsEntry, CpsFrame, DONE};
use std::{any::Any, rc::Rc};

/// Resume entry.
#[derive(Debug, Clone, Copy)]
pub enum ResumePtr {
    /// Native function pointer.
    Native(fn()),
    /// Native entry with access to the saved continuation frame.
    NativeWithFrame(fn(&mut Continuation)),
    /// Generated code entry.
    Jit(*const u8),
}

/// Module version identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u64);

/// Execution status returned by a CPS entry.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpsStatus {
    Completed = 0,
    Suspended = 1,
}

impl CpsStatus {
    pub fn from_i32(value: i32) -> Result<Self, CpsStatusError> {
        match value {
            0 => Ok(Self::Completed),
            1 => Ok(Self::Suspended),
            other => Err(CpsStatusError::InvalidValue(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpsStatusError {
    InvalidValue(i32),
}

/// Persistent execution outcome, including the original failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationStatus {
    Pending,
    Completed,
    Failed(ResumeError),
}

/// State retained across suspension points.
pub struct Continuation {
    /// Entry invoked on resumption.
    resume_ptr: ResumePtr,
    /// Native byte frame; the generated CPS frame is private to the runtime.
    pub frame_data: Vec<u8>,
    /// Code version retained by this continuation.
    pub module_version: ModuleId,
    status: ContinuationStatus,
    cps_frame: Option<CpsFrame>,
    _code_owner: Option<Rc<dyn Any>>,
}

impl Continuation {
    pub fn new_native(resume: fn(), version: ModuleId) -> Self {
        Continuation {
            resume_ptr: ResumePtr::Native(resume),
            frame_data: Vec::new(),
            module_version: version,
            status: ContinuationStatus::Pending,
            cps_frame: None,
            _code_owner: None,
        }
    }

    /// # Safety
    /// `entry` must use CpsEntry ABI and match the supplied frame layout.
    /// `owner` must keep entry and every reachable generated function alive.
    pub unsafe fn new_jit(
        entry: *const u8,
        frame: CpsFrame,
        version: ModuleId,
        owner: Rc<dyn Any>,
    ) -> Self {
        Self {
            resume_ptr: ResumePtr::Jit(entry),
            frame_data: Vec::new(),
            module_version: version,
            status: ContinuationStatus::Pending,
            cps_frame: Some(frame),
            _code_owner: Some(owner),
        }
    }

    /// Raw CPS return bits; available only after successful completion.
    pub fn cps_result(&self) -> Option<u64> {
        self.cps_frame
            .as_ref()
            .filter(|f| self.status == ContinuationStatus::Completed && f.state == DONE)
            .map(|f| f.result)
    }

    /// Create a native continuation with saved bytes.
    pub fn with_frame(resume: fn(), frame_data: Vec<u8>, version: ModuleId) -> Self {
        Self {
            resume_ptr: ResumePtr::Native(resume),
            frame_data,
            module_version: version,
            status: ContinuationStatus::Pending,
            cps_frame: None,
            _code_owner: None,
        }
    }

    /// Create a native continuation whose entry can update its frame.
    pub fn with_frame_entry(
        resume: fn(&mut Continuation),
        frame_data: Vec<u8>,
        version: ModuleId,
    ) -> Self {
        Self {
            resume_ptr: ResumePtr::NativeWithFrame(resume),
            frame_data,
            module_version: version,
            status: ContinuationStatus::Pending,
            cps_frame: None,
            _code_owner: None,
        }
    }

    /// Create a byte frame with a four-byte state prefix followed by locals.
    pub fn with_state(
        resume: fn(&mut Continuation),
        state: u32,
        locals: Vec<u8>,
        version: ModuleId,
    ) -> Self {
        let mut frame_data = Vec::with_capacity(4 + locals.len());
        frame_data.extend_from_slice(&state.to_le_bytes());
        frame_data.extend_from_slice(&locals);
        Self::with_frame_entry(resume, frame_data, version)
    }

    /// Whether execution is terminal, either completed or failed.
    pub fn is_resumed(&self) -> bool {
        self.status != ContinuationStatus::Pending
    }

    pub fn status(&self) -> ContinuationStatus {
        self.status
    }

    /// Read saved bytes to reconstruct native local state.
    pub fn frame(&self) -> &[u8] {
        &self.frame_data
    }

    /// Update saved bytes without changing the frame layout.
    pub fn frame_mut(&mut self) -> &mut [u8] {
        &mut self.frame_data
    }

    pub fn state(&self) -> Result<u32, FrameError> {
        let bytes = self.frame_data.get(..4).ok_or(FrameError::TooShort)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn set_state(&mut self, state: u32) -> Result<(), FrameError> {
        let bytes = self.frame_data.get_mut(..4).ok_or(FrameError::TooShort)?;
        bytes.copy_from_slice(&state.to_le_bytes());
        Ok(())
    }

    pub fn locals(&self) -> Result<&[u8], FrameError> {
        self.frame_data.get(4..).ok_or(FrameError::TooShort)
    }

    pub fn locals_mut(&mut self) -> Result<&mut [u8], FrameError> {
        self.frame_data.get_mut(4..).ok_or(FrameError::TooShort)
    }

    /// Resume native code once, or poll generated CPS code.
    pub fn resume(&mut self) -> Result<(), ResumeError> {
        if self.is_resumed() {
            return Err(ResumeError::AlreadyResumed);
        }
        match self.resume_ptr {
            ResumePtr::Native(f) => {
                self.status = ContinuationStatus::Failed(ResumeError::Interrupted);
                f();
                self.status = ContinuationStatus::Completed;
                Ok(())
            }
            ResumePtr::NativeWithFrame(f) => {
                self.status = ContinuationStatus::Failed(ResumeError::Interrupted);
                f(self);
                self.status = ContinuationStatus::Completed;
                Ok(())
            }
            ResumePtr::Jit(ptr) => {
                let frame = self
                    .cps_frame
                    .as_mut()
                    .ok_or(ResumeError::JitEntryRequiresRuntime)?;
                // Mark terminal before entering user code; a pending return
                // explicitly makes the continuation runnable again.
                self.status = ContinuationStatus::Failed(ResumeError::Interrupted);
                let entry: CpsEntry = unsafe { std::mem::transmute(ptr) };
                let status = unsafe { entry(frame) };
                match CpsStatus::from_i32(status) {
                    Ok(CpsStatus::Completed) if frame.state == DONE => {
                        self.status = ContinuationStatus::Completed;
                        Ok(())
                    }
                    Ok(CpsStatus::Completed) => {
                        let error = ResumeError::MissingCompletionState;
                        self.status = ContinuationStatus::Failed(error);
                        Err(error)
                    }
                    Ok(CpsStatus::Suspended) => {
                        self.status = ContinuationStatus::Pending;
                        Ok(())
                    }
                    Err(_) => {
                        let error = ResumeError::InvalidCpsStatus(status);
                        self.status = ContinuationStatus::Failed(error);
                        Err(error)
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeError {
    Interrupted,
    MissingCompletionState,
    AlreadyResumed,
    JitEntryRequiresRuntime,
    InvalidCpsStatus(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    TooShort,
}

impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuation")
            .field("resume_ptr", &self.resume_ptr)
            .field("frame_data_len", &self.frame_data.len())
            .field("module_version", &self.module_version)
            .field("status", &self.status)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resume_with_frame(cont: &mut Continuation) {
        let value = u32::from_le_bytes(cont.frame().try_into().unwrap());
        cont.frame_mut().copy_from_slice(&(value + 1).to_le_bytes());
    }

    #[test]
    fn native_entry_reads_and_updates_saved_frame() {
        let mut continuation = Continuation::with_frame_entry(
            resume_with_frame,
            41u32.to_le_bytes().to_vec(),
            ModuleId(7),
        );
        continuation.resume().unwrap();
        assert_eq!(continuation.frame(), &42u32.to_le_bytes());
        assert_eq!(continuation.resume(), Err(ResumeError::AlreadyResumed));
    }

    fn resume_state(cont: &mut Continuation) {
        assert_eq!(cont.state().unwrap(), 1);
        cont.locals_mut()
            .unwrap()
            .copy_from_slice(&42u32.to_le_bytes());
        cont.set_state(2).unwrap();
    }

    #[test]
    fn native_frame_contains_state_and_locals() {
        let mut continuation =
            Continuation::with_state(resume_state, 1, 7u32.to_le_bytes().to_vec(), ModuleId(8));
        continuation.resume().unwrap();
        assert_eq!(continuation.state().unwrap(), 2);
        assert_eq!(continuation.locals().unwrap(), &42u32.to_le_bytes());
    }

    #[test]
    fn cps_status_has_stable_abi_values() {
        assert_eq!(CpsStatus::from_i32(0).unwrap(), CpsStatus::Completed);
        assert_eq!(CpsStatus::from_i32(1).unwrap(), CpsStatus::Suspended);
        assert_eq!(CpsStatus::from_i32(9), Err(CpsStatusError::InvalidValue(9)));
    }
    #[test]
    fn terminal_status_is_preserved_after_native_panic() {
        fn fail() {
            panic!("native failure");
        }
        let mut task = Continuation::new_native(fail, ModuleId(1));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task.resume()));
        assert!(result.is_err());
        assert_eq!(
            task.status(),
            ContinuationStatus::Failed(ResumeError::Interrupted)
        );
        assert_eq!(task.resume(), Err(ResumeError::AlreadyResumed));
    }

    #[test]
    fn malformed_completion_cannot_expose_a_result() {
        unsafe extern "C" fn missing_done(frame: *mut CpsFrame) -> i32 {
            (*frame).result = 42;
            0
        }
        let mut task = unsafe {
            Continuation::new_jit(
                missing_done as *const u8,
                CpsFrame::new(0),
                ModuleId(1),
                Rc::new(()),
            )
        };
        assert_eq!(task.resume(), Err(ResumeError::MissingCompletionState));
        assert_eq!(task.cps_result(), None);
        assert_eq!(
            task.status(),
            ContinuationStatus::Failed(ResumeError::MissingCompletionState)
        );
    }
}
