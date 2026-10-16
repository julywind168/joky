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
pub struct FileService(Service);
#[derive(Clone)]
enum Service {
    Native(NativeService),
    External(Arc<External>),
}
impl FileService {
    pub fn new(backend: Arc<dyn FileSystem>, limits: FileLimits) -> Result<Self, Error> {
        Ok(Self(Service::Native(NativeService::new(backend, limits)?)))
    }
    pub fn native(base: impl AsRef<Path>, limits: FileLimits) -> Result<Self, Error> {
        Ok(Self(Service::Native(NativeService::native(base, limits)?)))
    }
    /// Install a nonblocking host service, retaining its context after validation.
    /// # Safety
    /// The descriptor must obey the protocol and lifetime contract in `abi`.
    pub unsafe fn from_descriptor(descriptor: FileServiceV1) -> Result<Self, Error> {
        Ok(Self(Service::External(External::new(descriptor)?)))
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
