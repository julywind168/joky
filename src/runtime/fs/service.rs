//! Unified optional capability: native workers or a versioned host descriptor.
pub use super::worker::{FileHandle, FileLimits, FileSystem};
use super::{
    abi::{External, ExternalFile, ExternalTicket, FileServiceV1},
    worker::{Input, NativeFile, NativeService, NativeTicket, Packet},
    FsError,
};
use crate::{runtime::executor::TaskWaker, Error};
use std::{path::Path, sync::Arc};
#[derive(Clone)]
pub struct FileService(
    Service,
    Option<Arc<crate::runtime::process::ProcessContext>>,
    Option<Arc<super::testing::Inputs>>,
);
#[derive(Clone)]
enum Service {
    Native(NativeService),
    External(Arc<External>),
}
impl FileService {
    pub fn new(backend: Arc<dyn FileSystem>, limits: FileLimits) -> Result<Self, Error> {
        Ok(Self(
            Service::Native(NativeService::new(backend, limits)?),
            None,
            None,
        ))
    }
    pub fn native(base: impl AsRef<Path>, limits: FileLimits) -> Result<Self, Error> {
        Ok(Self(
            Service::Native(NativeService::native(base, limits)?),
            None,
            None,
        ))
    }
    /// Explicitly grant filesystem and exclusive process stdin access.
    /// Cancelling an in-flight read revokes delivery; the OS read may finish later.
    pub fn native_with_stdio(base: impl AsRef<Path>, limits: FileLimits) -> Result<Self, Error> {
        Ok(Self(
            Service::Native(NativeService::native_with_stdio(base, limits, true)?),
            None,
            None,
        ))
    }
    /// Attach explicit startup inputs; clones and child tasks share this immutable snapshot.
    /// Replaces test configuration capability while retaining the file/stdin backend.
    pub fn with_process_context(
        mut self,
        context: crate::runtime::process::ProcessContext,
    ) -> Self {
        self.1 = Some(Arc::new(context));
        self.2 = None;
        self
    }
    /// Grant a test its own empty inputs and a filesystem rooted at an existing directory.
    /// The caller owns directory cleanup. Paths retain native filesystem semantics.
    pub fn testing(base: impl AsRef<Path>, limits: FileLimits) -> Result<Self, Error> {
        let directory = base
            .as_ref()
            .canonicalize()
            .map_err(|e| super::failure(e.to_string()))?;
        if !directory.is_dir() {
            return Err(super::failure("Test directory must be a directory"));
        }
        let inputs = Arc::new(super::testing::Inputs::new(directory.clone()));
        let backend = super::testing::Backend {
            native: super::worker::Native(directory, false),
            inputs: inputs.clone(),
        };
        Ok(Self(
            Service::Native(NativeService::new(Arc::new(backend), limits)?),
            None,
            Some(inputs),
        ))
    }
    pub(super) fn process_read(
        &self,
        kind: u64,
        index: u64,
        key: &str,
    ) -> Result<crate::runtime::process::Value, FsError> {
        if let Some(inputs) = &self.2 {
            return inputs.read(kind, index, key);
        }
        self.1
            .as_ref()
            .ok_or_else(|| FsError::new(1, "Host does not provide process inputs"))?
            .read(kind, index, key)
    }
    pub(super) fn test_input(
        &self,
        kind: u64,
        key: &str,
        value: &str,
    ) -> Result<Option<String>, FsError> {
        self.2
            .as_ref()
            .ok_or_else(|| FsError::new(1, "Host does not provide a test context"))?
            .configure(kind, key, value)
    }
    pub(crate) fn standalone() -> Result<Self, Error> {
        let context = crate::runtime::process::standalone();
        let mut service = if context.is_some() {
            Self::native_with_stdio(".", Default::default())?
        } else {
            Self::native(".", Default::default())?
        };
        service.1 = context;
        Ok(service)
    }
    /// Install a nonblocking host service, retaining its context after validation.
    /// # Safety
    /// The descriptor must obey the protocol, lifetime and concurrent-callback
    /// contract in `abi`. Its context and tokens must support arbitrary threads;
    /// descriptor validation cannot establish those guarantees.
    pub unsafe fn from_descriptor(descriptor: FileServiceV1) -> Result<Self, Error> {
        Ok(Self(
            Service::External(External::new(descriptor)?),
            None,
            None,
        ))
    }
    pub fn shutdown(&self) {
        match &self.0 {
            Service::Native(s) => s.shutdown(),
            Service::External(s) => s.shutdown(),
        }
    }
    pub fn is_quiescent(&self) -> bool {
        match &self.0 {
            Service::Native(s) => s.is_quiescent(),
            Service::External(s) => s.is_quiescent(),
        }
    }
    /// Native service counters (files, requests, bytes). Foreign accounting belongs to its host.
    pub fn usage(&self) -> Option<(usize, usize, usize)> {
        match &self.0 {
            Service::Native(s) => Some(s.usage()),
            Service::External(_) => None,
        }
    }
    pub(super) fn submit(
        &self,
        input: Input<'_>,
        file: Option<Arc<FileCell>>,
        wake: TaskWaker,
    ) -> Result<Arc<Ticket>, FsError> {
        Ok(Arc::new(match &self.0 {
            Service::Native(s) => Ticket::Native(s.submit(
                input,
                file.map(|f| match &*f {
                    FileCell::Native(f) => f.clone(),
                    _ => unreachable!(),
                }),
                wake,
            )?),
            Service::External(s) => Ticket::External(s.submit(
                input,
                file.map(|f| match &*f {
                    FileCell::External(f) => f.clone(),
                    _ => unreachable!(),
                }),
                wake,
            )?),
        }))
    }
}
pub(crate) enum FileCell {
    Native(Arc<NativeFile>),
    External(Arc<ExternalFile>),
}
impl FileCell {
    pub(crate) fn close(&self, wake: Option<TaskWaker>) {
        match self {
            Self::Native(f) => f.close(wake),
            Self::External(f) => f.close(wake),
        }
    }
    pub(super) fn is_closed(&self) -> bool {
        match self {
            Self::Native(f) => f.is_closed(),
            Self::External(f) => f.is_closed(),
        }
    }
    pub(super) fn close_result(&self) -> Option<Result<(), FsError>> {
        match self {
            Self::Native(f) => f.close_result(),
            Self::External(f) => f.close_result(),
        }
    }
    pub(super) fn stop_close(&self, reason: u32) {
        match self {
            Self::Native(f) => f.stop_close(),
            Self::External(f) => f.stop_close(reason),
        }
    }
}
pub(super) enum Ticket {
    Native(Arc<NativeTicket>),
    External(ExternalTicket),
}
impl Ticket {
    pub(super) fn take(&self) -> Option<Packet> {
        match self {
            Self::Native(t) => t.take(),
            Self::External(t) => t.take(),
        }
    }
    pub(super) fn stop(&self, reason: u32) {
        match self {
            Self::Native(t) if reason == 1 => t.fail(),
            Self::Native(t) => t.stop(),
            Self::External(t) => t.stop(reason),
        }
    }
}
