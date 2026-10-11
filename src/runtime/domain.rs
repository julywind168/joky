//! Compiler-owned source domain adapters and versioned module metadata.
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Operation {
    Create,
    Handle,
    ReadOnly,
    Close,
    Request,
    Read,
    Write,
    CurrentRead,
    CurrentWrite,
    Snapshot,
    SnapshotValue,
    SnapshotCheck,
    Candidate,
    Commit,
    CommitWait,
    CommitRevision,
    CommitCancel,
    CommitIdentity,
    DeadlineAfter,
    CommitBefore,
}
/// Private reply status lane; kept separate from all 64 payload bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum RequestStatus {
    Completed = 0,
    Failed = 1,
    Cancelled = 2,
    Closed = 3,
    ResourceLimit = 4,
    InvalidAccess = 5,
    DomainFailed = 6,
}
impl RequestStatus {
    pub(crate) fn from_error(error: &Error) -> Self {
        match error.code() {
            ErrorCode::ResourceLimit => Self::ResourceLimit,
            ErrorCode::InvalidCall | ErrorCode::TypeMismatch | ErrorCode::NotFound => {
                Self::InvalidAccess
            }
            _ => Self::Failed,
        }
    }
}
pub(crate) fn operation(name: &str) -> Option<Operation> {
    Some(match name {
        "__domain_create" => Operation::Create,
        "__domain_handle" => Operation::Handle,
        "__domain_readonly" => Operation::ReadOnly,
        "__domain_close" => Operation::Close,
        "__domain_request" => Operation::Request,
        "__domain_read_at" => Operation::Read,
        "__domain_write_at" => Operation::Write,
        "__domain_read_scalar" => Operation::CurrentRead,
        "__domain_write_scalar" => Operation::CurrentWrite,
        "__domain_snapshot" => Operation::Snapshot,
        "__domain_snapshot_value" => Operation::SnapshotValue,
        "__domain_snapshot_check" => Operation::SnapshotCheck,
        "__domain_candidate" => Operation::Candidate,
        "__domain_commit" => Operation::Commit,
        "__domain_commit_wait" => Operation::CommitWait,
        "__domain_commit_revision" => Operation::CommitRevision,
        "__domain_commit_cancel" => Operation::CommitCancel,
        "__domain_commit_identity" => Operation::CommitIdentity,
        "__domain_deadline_after" => Operation::DeadlineAfter,
        "__domain_commit_before" => Operation::CommitBefore,
        _ => return None,
    })
}
pub(crate) fn module_type() -> Type {
    Type::Named("domain.Module".into())
}
pub(crate) fn owner_type() -> Type {
    Type::Resource("domain.StateOwner".into())
}
pub(crate) fn handle_type() -> Type {
    Type::Named("domain.Handle".into())
}
pub(crate) fn snapshot_type() -> Type {
    Type::Resource("domain.SnapshotOwner".into())
}
pub(crate) fn candidate_type() -> Type {
    Type::Resource("domain.CandidateOwner".into())
}
pub(crate) fn commit_type() -> Type {
    Type::Resource("domain.CommitOwner".into())
}
pub(crate) fn deadline_type() -> Type {
    Type::Resource("domain.DeadlineOwner".into())
}
impl Operation {
    pub(crate) fn signature(self) -> FunctionSignature {
        let token = handle_type();
        let text = Type::String;
        let i = Type::UInt64;
        let m = module_type();
        let owner = owner_type();
        let (params, ret) = match self {
            Self::Create => (vec![m, text, i], owner),
            Self::Handle => (vec![Type::reference(owner, false)], token),
            Self::ReadOnly => (vec![token.clone()], token),
            Self::Close => (vec![Type::reference(owner, true)], Type::Unit),
            Self::Request => (
                vec![m, token, text, i],
                Type::tuple(vec![Type::Int32, Type::UInt64]),
            ),
            Self::CurrentRead => (vec![text], i),
            Self::CurrentWrite => (vec![text, i.clone()], i),
            Self::Read => (vec![m, token, text], i),
            Self::Write => (vec![m, token, text, i.clone()], i),
            Self::Snapshot => (vec![m, token, text], snapshot_type()),
            Self::SnapshotValue => (vec![Type::reference(snapshot_type(), false)], i),
            Self::SnapshotCheck => (vec![Type::reference(snapshot_type(), false)], Type::Int32),
            Self::Candidate => (
                vec![m, Type::reference(snapshot_type(), false), text, i],
                candidate_type(),
            ),
            Self::Commit => (
                vec![token, Type::reference(candidate_type(), true)],
                commit_type(),
            ),
            Self::DeadlineAfter => (vec![Type::Int32], deadline_type()),
            Self::CommitBefore => (
                vec![
                    token,
                    Type::reference(candidate_type(), true),
                    Type::reference(deadline_type(), false),
                ],
                commit_type(),
            ),
            Self::CommitWait => (vec![Type::reference(commit_type(), true)], Type::Int32),
            Self::CommitRevision | Self::CommitIdentity => {
                (vec![Type::reference(commit_type(), false)], i)
            }
            Self::CommitCancel => (vec![Type::reference(commit_type(), false)], Type::Unit),
        };
        FunctionSignature::new(params, ret)
    }
    pub(crate) fn validate(
        self,
        signature: &FunctionSignature,
        suspends: bool,
    ) -> Result<(), Error> {
        if !suspends || *signature != self.signature() {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::InvalidCall,
                "Domain operation requires its exact compiler-owned signature",
            ));
        }
        Ok(())
    }
}

pub(crate) mod metadata;

#[export_name = "__joky_domain_module_placeholder"]
pub(crate) extern "C" fn module_placeholder() -> u64 {
    0
}
#[export_name = "__joky_domain_poll_v2"]
/// # Safety
/// `frame` must be a live, uniquely polled CPS frame with an attached arena and
/// slots matching `kind`'s exact compiler-owned signature. Code and borrowed
/// resources must remain live through completion or stop on this thread.
pub(crate) unsafe extern "C" fn poll_native(frame: *mut super::cps::CpsFrame, kind: u32) -> i32 {
    let op = match kind {
        0 => Operation::Create,
        1 => Operation::Handle,
        2 => Operation::ReadOnly,
        3 => Operation::Close,
        4 => Operation::Request,
        5 => Operation::Read,
        6 => Operation::Write,
        7 => Operation::CurrentRead,
        8 => Operation::CurrentWrite,
        9 => Operation::Snapshot,
        10 => Operation::SnapshotValue,
        11 => Operation::SnapshotCheck,
        12 => Operation::Candidate,
        13 => Operation::Commit,
        14 => Operation::CommitWait,
        15 => Operation::CommitRevision,
        16 => Operation::CommitCancel,
        17 => Operation::CommitIdentity,
        18 => Operation::DeadlineAfter,
        19 => Operation::CommitBefore,
        _ => {
            super::error::record_error(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::AbiError,
                "Unknown domain operation",
            ));
            return super::cps::RUNTIME_ERROR;
        }
    };
    match poll(&mut *super::cps::arena(frame), &mut *frame, op) {
        Ok(Some(value)) => {
            (*frame).result = value;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}
pub(crate) use crate::vm::state_domains::source::poll;
