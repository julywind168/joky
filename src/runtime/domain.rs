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
    Pair,
    SnapshotPair,
    CandidatePair,
    CommitPair,
    CommitPairBefore,
    CommitSecondRevision,
    AccessSet,
    AccessInclude,
    SnapshotSet,
    SetRead,
    SetCheck,
    SetCandidate,
    SetAdd,
    CommitSet,
    CommitSetBefore,
    SetRevision,
    AdmissionCreate,
    AdmissionHandle,
    AdmissionClose,
    AdmissionTry,
    AdmissionAcquire,
    AdmissionStatus,
    OutboxCreate = 43,
    OutboxHandle,
    OutboxClose,
    OutboxBatch,
    OutboxStage,
    OutboxReceive,
    OutboxAttach,
    OutboxAttachPair,
    OutboxAttachSet,
    OutboxMessageClosed,
    OutboxMessageBytes,
    DirectoryCreate,
    DirectoryHandle,
    DirectoryClose,
    DirectoryInsert,
    DirectoryReplace,
    DirectoryRemove,
    DirectoryLookup,
    DirectoryFound,
    TimerCreate,
    TimerHandle,
    TimerClose,
    TimerCancel,
    TimerStatus,
    TimerStart,
    Values,
    CurrentValues,
    Query,
    CurrentQuery,
    CreateValues,
    CandidateValues,
    CandidatePairValues,
    SetCandidateValues,
    SetAddValues,
    SnapshotValues,
    SnapshotPairValues,
    SnapshotReadValues,
    SetReadValues,
    RequestValues,
    TimerStartValues,
    CandidateAt,
    CandidatePairAt,
    CandidateAtValues,
    CandidatePairAtValues,
    SetRevisionChecked,
    SetEditSnapshot,
    SetEditCandidate,
    When,
    PeriodicStatus,
    CommitFailedOutbox,
    DirectoryInsertOwned,
    OutboxCreateDisconnect,
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
        "__domain_query_at" => Operation::Query,
        "__domain_query_scalar" => Operation::CurrentQuery,
        "__domain_when" => Operation::When,
        "__domain_values" => Operation::Values,
        "__domain_current_values" => Operation::CurrentValues,
        "__timer_create" => Operation::TimerCreate,
        "__timer_handle" => Operation::TimerHandle,
        "__timer_close" => Operation::TimerClose,
        "__timer_cancel" => Operation::TimerCancel,
        "__timer_status" => Operation::TimerStatus,
        "__domain_periodic_status" => Operation::PeriodicStatus,
        "__timer_start" => Operation::TimerStart,
        "__timer_start_values" => Operation::TimerStartValues,
        "__directory_create" => Operation::DirectoryCreate,
        "__directory_handle" => Operation::DirectoryHandle,
        "__directory_close" => Operation::DirectoryClose,
        "__directory_insert" => Operation::DirectoryInsert,
        "__directory_insert_owned" => Operation::DirectoryInsertOwned,
        "__directory_replace" => Operation::DirectoryReplace,
        "__directory_remove" => Operation::DirectoryRemove,
        "__directory_lookup" => Operation::DirectoryLookup,
        "__directory_found" => Operation::DirectoryFound,

        "__outbox_message_closed" => Operation::OutboxMessageClosed,
        "__outbox_message_bytes" => Operation::OutboxMessageBytes,
        "__outbox_create_disconnect" => Operation::OutboxCreateDisconnect,
        "__outbox_create" => Operation::OutboxCreate,
        "__outbox_handle" => Operation::OutboxHandle,
        "__outbox_close" => Operation::OutboxClose,
        "__outbox_batch" => Operation::OutboxBatch,
        "__outbox_stage" => Operation::OutboxStage,
        "__outbox_receive" => Operation::OutboxReceive,
        "__outbox_attach" => Operation::OutboxAttach,
        "__outbox_attach_pair" => Operation::OutboxAttachPair,
        "__outbox_attach_set" => Operation::OutboxAttachSet,
        "__domain_create" => Operation::Create,
        "__domain_create_values" => Operation::CreateValues,
        "__domain_handle" => Operation::Handle,
        "__domain_set_edit_snapshot" => Operation::SetEditSnapshot,
        "__domain_set_edit_candidate" => Operation::SetEditCandidate,
        "__domain_readonly" => Operation::ReadOnly,
        "__domain_close" => Operation::Close,
        "__domain_request" => Operation::Request,
        "__domain_request_values" => Operation::RequestValues,
        "__domain_read_at" => Operation::Read,
        "__domain_write_at" => Operation::Write,
        "__domain_read_scalar" => Operation::CurrentRead,
        "__domain_write_scalar" => Operation::CurrentWrite,
        "__domain_snapshot_values" => Operation::SnapshotValues,
        "__domain_snapshot_pair_values" => Operation::SnapshotPairValues,
        "__domain_snapshot_read_values" => Operation::SnapshotReadValues,
        "__domain_set_read_values" => Operation::SetReadValues,
        "__domain_snapshot" => Operation::Snapshot,
        "__domain_snapshot_value" => Operation::SnapshotValue,
        "__domain_snapshot_check" => Operation::SnapshotCheck,
        "__domain_candidate_values" => Operation::CandidateValues,
        "__domain_candidate_at" => Operation::CandidateAt,
        "__domain_candidate_pair_at" => Operation::CandidatePairAt,
        "__domain_candidate_at_values" => Operation::CandidateAtValues,
        "__domain_candidate_pair_at_values" => Operation::CandidatePairAtValues,
        "__domain_set_revision_checked" => Operation::SetRevisionChecked,
        "__domain_candidate" => Operation::Candidate,
        "__domain_commit" => Operation::Commit,
        "__domain_commit_wait" => Operation::CommitWait,
        "__domain_commit_failed_outbox" => Operation::CommitFailedOutbox,
        "__domain_commit_revision" => Operation::CommitRevision,
        "__domain_commit_cancel" => Operation::CommitCancel,
        "__domain_commit_identity" => Operation::CommitIdentity,
        "__domain_deadline_after" => Operation::DeadlineAfter,
        "__domain_commit_before" => Operation::CommitBefore,
        "__domain_pair" => Operation::Pair,
        "__domain_snapshot_pair" => Operation::SnapshotPair,
        "__domain_candidate_pair_values" => Operation::CandidatePairValues,
        "__domain_candidate_pair" => Operation::CandidatePair,
        "__domain_commit_pair" => Operation::CommitPair,
        "__domain_commit_pair_before" => Operation::CommitPairBefore,
        "__domain_commit_second_revision" => Operation::CommitSecondRevision,
        "__domain_access_set" => Operation::AccessSet,
        "__domain_access_include" => Operation::AccessInclude,
        "__domain_snapshot_set" => Operation::SnapshotSet,
        "__domain_set_read" => Operation::SetRead,
        "__domain_set_check" => Operation::SetCheck,
        "__domain_set_candidate_values" => Operation::SetCandidateValues,
        "__domain_set_candidate" => Operation::SetCandidate,
        "__domain_set_add_values" => Operation::SetAddValues,
        "__domain_set_add" => Operation::SetAdd,
        "__domain_commit_set" => Operation::CommitSet,
        "__domain_commit_set_before" => Operation::CommitSetBefore,
        "__domain_set_revision" => Operation::SetRevision,
        "__domain_admission_create" => Operation::AdmissionCreate,
        "__domain_admission_handle" => Operation::AdmissionHandle,
        "__domain_admission_close" => Operation::AdmissionClose,
        "__domain_admission_try" => Operation::AdmissionTry,
        "__domain_admission_acquire" => Operation::AdmissionAcquire,
        "__domain_admission_status" => Operation::AdmissionStatus,
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
pub(crate) fn access_set_type() -> Type {
    Type::Resource("domain.AccessSetOwner".into())
}
pub(crate) fn snapshot_set_type() -> Type {
    Type::Resource("domain.SnapshotSetOwner".into())
}
pub(crate) fn proposal_type() -> Type {
    Type::Resource("domain.CandidateSetOwner".into())
}
pub(crate) fn candidate_pair_type() -> Type {
    Type::Resource("domain.CandidatePairOwner".into())
}
pub(crate) fn commit_type() -> Type {
    Type::Resource("domain.CommitOwner".into())
}
pub(crate) fn deadline_type() -> Type {
    Type::Resource("domain.DeadlineOwner".into())
}
impl Operation {
    pub(crate) fn access(self) -> bool {
        matches!(
            self,
            Self::Read
                | Self::Write
                | Self::CurrentRead
                | Self::CurrentWrite
                | Self::Pair
                | Self::When
                | Self::Values
                | Self::CurrentValues
                | Self::Query
                | Self::CurrentQuery
        )
    }
    pub(crate) fn status_payload(self) -> bool {
        matches!(self, Self::Request | Self::SetRevisionChecked | Self::When)
            || (self.access() && !matches!(self, Self::Values | Self::CurrentValues | Self::When))
    }
    pub(crate) fn signature(self) -> FunctionSignature {
        let token = handle_type();
        let text = Type::String;
        let i = Type::UInt64;
        let m = module_type();
        let owner = owner_type();
        let pool = Type::Resource("admission.PoolOwner".into());
        let permit = Type::Resource("admission.PermitOwner".into());
        let admission_handle = Type::Named("admission.Token".into());
        let queue = Type::Resource("outbox.QueueOwner".into());
        let batch = Type::Resource("outbox.BatchOwner".into());
        let message = Type::Resource("outbox.MessageOwner".into());
        let bytes = Type::Buffer(std::sync::Arc::new(Type::UInt8));
        let directory = Type::Resource("domain.DirectoryOwner".into());
        let (params, ret) = match self {
            Self::PeriodicStatus => (vec![token, text], Type::Int32),
            Self::When => (
                vec![
                    m,
                    text,
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), false),
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), true),
                ],
                Type::tuple(vec![Type::Int32, Type::UInt64]),
            ),
            Self::CurrentValues => (
                vec![
                    text,
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), true),
                ],
                Type::Int32,
            ),
            Self::Values => (
                vec![
                    m,
                    token.clone(),
                    token,
                    i,
                    text,
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), true),
                ],
                Type::Int32,
            ),
            Self::TimerCreate => (vec![], Type::Resource("domain.TimerOwner".into())),
            Self::TimerHandle => (
                vec![Type::reference(
                    Type::Resource("domain.TimerOwner".into()),
                    false,
                )],
                i,
            ),
            Self::TimerClose | Self::TimerCancel => (vec![i], Type::Unit),
            Self::TimerStatus => (vec![i], Type::Int32),
            Self::TimerStartValues => (
                vec![
                    m,
                    i,
                    token,
                    Type::Int32,
                    text,
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), false),
                    Type::Bool,
                ],
                Type::Int32,
            ),
            Self::TimerStart => (
                vec![m, i.clone(), token, Type::Int32, text, i, Type::Bool],
                Type::Int32,
            ),
            Self::DirectoryCreate => (vec![i], directory),
            Self::DirectoryHandle => (vec![Type::reference(directory, false)], i),
            Self::DirectoryClose => (vec![i], Type::Unit),
            Self::DirectoryInsert => (vec![i.clone(), i, token], Type::Int32),
            Self::DirectoryInsertOwned => (
                vec![i.clone(), i, Type::reference(owner, true)],
                Type::Int32,
            ),
            Self::DirectoryReplace => (vec![i.clone(), i, token.clone(), token], Type::Int32),
            Self::DirectoryRemove => (vec![i.clone(), i, token], Type::Bool),
            Self::DirectoryLookup => (vec![m, i.clone(), i, text, Type::Bool], token),
            Self::DirectoryFound => (vec![token], Type::Bool),
            Self::OutboxCreateDisconnect => (vec![i.clone(), i.clone(), i.clone()], queue),
            Self::OutboxCreate => (vec![i.clone(), i.clone()], queue),
            Self::OutboxHandle => (vec![Type::reference(queue, false)], i),
            Self::OutboxClose => (vec![i], Type::Unit),
            Self::OutboxBatch => (vec![i.clone(), i], batch),
            Self::OutboxStage => (vec![Type::reference(batch, true), i, bytes], Type::Int32),
            Self::OutboxReceive => (vec![Type::reference(queue, true)], message),
            Self::OutboxMessageClosed => (vec![Type::reference(message, false)], Type::Bool),
            Self::OutboxMessageBytes => (vec![Type::reference(message, true)], bytes),
            Self::OutboxAttach | Self::OutboxAttachPair | Self::OutboxAttachSet => (
                vec![
                    Type::reference(
                        match self {
                            Self::OutboxAttach => candidate_type(),
                            Self::OutboxAttachPair => candidate_pair_type(),
                            _ => proposal_type(),
                        },
                        true,
                    ),
                    Type::reference(batch, true),
                ],
                Type::Unit,
            ),
            Self::AdmissionCreate => (vec![Type::UInt64, Type::UInt64], pool),
            Self::AdmissionHandle => (vec![Type::reference(pool, false)], admission_handle),
            Self::AdmissionClose => (vec![Type::reference(pool, false)], Type::Unit),
            Self::AdmissionTry | Self::AdmissionAcquire => (vec![admission_handle], permit),
            Self::AdmissionStatus => (vec![Type::reference(permit, false)], Type::Int32),
            Self::AccessSet => (vec![], access_set_type()),
            Self::AccessInclude => (
                vec![Type::reference(access_set_type(), true), token],
                Type::Unit,
            ),
            Self::SnapshotSet => (
                vec![Type::reference(access_set_type(), true)],
                snapshot_set_type(),
            ),
            Self::SetRead => (
                vec![m, Type::reference(snapshot_set_type(), false), token, text],
                i,
            ),
            Self::SetCheck => (
                vec![Type::reference(snapshot_set_type(), false)],
                Type::Int32,
            ),
            Self::SetCandidate => (
                vec![
                    m,
                    Type::reference(snapshot_set_type(), true),
                    token,
                    text,
                    i,
                ],
                proposal_type(),
            ),
            Self::SetAdd => (
                vec![m, Type::reference(proposal_type(), true), token, text, i],
                proposal_type(),
            ),
            Self::CommitSet => (vec![Type::reference(proposal_type(), true)], commit_type()),
            Self::CommitSetBefore => (
                vec![
                    Type::reference(proposal_type(), true),
                    Type::reference(deadline_type(), false),
                ],
                commit_type(),
            ),
            Self::SetEditSnapshot => return Self::SetCandidateValues.signature(),
            Self::SetEditCandidate => return Self::SetAddValues.signature(),
            Self::SetRevisionChecked => (
                vec![Type::reference(commit_type(), false), token],
                Type::tuple(vec![Type::Int32, i]),
            ),
            Self::CandidateAt
            | Self::CandidatePairAt
            | Self::CandidateAtValues
            | Self::CandidatePairAtValues => {
                let base = match self {
                    Self::CandidateAt => Self::Candidate,
                    Self::CandidatePairAt => Self::CandidatePair,
                    Self::CandidateAtValues => Self::CandidateValues,
                    Self::CandidatePairAtValues => Self::CandidatePairValues,
                    _ => unreachable!(),
                };
                let mut signature = base.signature();
                signature.params.insert(2, token);
                return signature;
            }
            Self::SetRevision => (vec![Type::reference(commit_type(), false), token], i),
            Self::Create => (vec![m, text, i], owner),
            Self::CandidateValues
            | Self::CandidatePairValues
            | Self::SetCandidateValues
            | Self::SetAddValues => {
                let scalar = match self {
                    Self::CandidateValues => Self::Candidate,
                    Self::CandidatePairValues => Self::CandidatePair,
                    Self::SetCandidateValues => Self::SetCandidate,
                    Self::SetAddValues => Self::SetAdd,
                    _ => unreachable!(),
                };
                let mut signature = scalar.signature();
                *signature.params.last_mut().unwrap() =
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), false);
                return signature;
            }
            Self::CreateValues => (
                vec![
                    m,
                    text,
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), false),
                ],
                owner,
            ),
            Self::Handle => (vec![Type::reference(owner, false)], token),
            Self::ReadOnly => (vec![token.clone()], token),
            Self::Close => (vec![Type::reference(owner, true)], Type::Unit),
            Self::RequestValues => (
                vec![
                    m,
                    token,
                    text,
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), true),
                ],
                Type::Int32,
            ),
            Self::Request => (
                vec![m, token, text, i],
                Type::tuple(vec![Type::Int32, Type::UInt64]),
            ),
            Self::CurrentRead => (vec![text], Type::tuple(vec![Type::Int32, Type::UInt64])),
            Self::CurrentWrite | Self::CurrentQuery => (
                vec![text, i.clone()],
                Type::tuple(vec![Type::Int32, Type::UInt64]),
            ),
            Self::Read => (
                vec![m, token, text],
                Type::tuple(vec![Type::Int32, Type::UInt64]),
            ),
            Self::Write | Self::Query => (
                vec![m, token, text, i.clone()],
                Type::tuple(vec![Type::Int32, Type::UInt64]),
            ),
            Self::SnapshotReadValues => (
                vec![
                    Type::reference(snapshot_type(), false),
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), true),
                ],
                Type::Unit,
            ),
            Self::SetReadValues => (
                vec![
                    m,
                    Type::reference(snapshot_set_type(), false),
                    token,
                    text,
                    Type::reference(Type::Buffer(std::sync::Arc::new(Type::UInt64)), true),
                ],
                Type::Unit,
            ),
            Self::Snapshot | Self::SnapshotValues => (vec![m, token, text], snapshot_type()),
            Self::SnapshotValue => (vec![Type::reference(snapshot_type(), false)], i),
            Self::SnapshotCheck => (vec![Type::reference(snapshot_type(), false)], Type::Int32),
            Self::Candidate => (
                vec![m, Type::reference(snapshot_type(), false), text, i],
                candidate_type(),
            ),
            Self::CandidatePair => (
                vec![m, Type::reference(candidate_type(), true), text, i],
                candidate_pair_type(),
            ),
            Self::CommitPair => (
                vec![
                    token.clone(),
                    token,
                    Type::reference(candidate_pair_type(), true),
                ],
                commit_type(),
            ),
            Self::CommitPairBefore => (
                vec![
                    token.clone(),
                    token,
                    Type::reference(candidate_pair_type(), true),
                    Type::reference(deadline_type(), false),
                ],
                commit_type(),
            ),
            Self::Commit => (
                vec![token, Type::reference(candidate_type(), true)],
                commit_type(),
            ),
            Self::Pair => (
                vec![m, token.clone(), token, text, i.clone()],
                Type::tuple(vec![Type::Int32, Type::UInt64]),
            ),
            Self::SnapshotPair | Self::SnapshotPairValues => {
                (vec![m, token.clone(), token, text], snapshot_type())
            }
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
            Self::CommitRevision
            | Self::CommitIdentity
            | Self::CommitSecondRevision
            | Self::CommitFailedOutbox => (vec![Type::reference(commit_type(), false)], i),
            Self::CommitCancel => (vec![Type::reference(commit_type(), false)], Type::Unit),
        };
        FunctionSignature::new(params, ret)
    }
    pub(crate) fn validate(
        self,
        signature: &FunctionSignature,
        suspends: bool,
    ) -> Result<(), Error> {
        // Array<UInt8> has one owned Buffer lane. Validate the nominal wrapper
        // before normalizing only this private byte boundary's ABI slot.
        let mut actual = signature.clone();
        let bytes = |ty: &Type| matches!(ty, Type::Record(r) if r.name == "joky/array.Array" && r.arguments == [Type::UInt8] && r.fields.len() == 1 && matches!(&r.fields[0].ty, Type::Buffer(element) if **element == Type::UInt8));
        if self == Self::OutboxStage && actual.params.get(2).is_some_and(bytes) {
            actual.params[2] = Type::Buffer(std::sync::Arc::new(Type::UInt8));
        }
        if self == Self::OutboxMessageBytes && bytes(&actual.ret) {
            actual.ret = Type::Buffer(std::sync::Arc::new(Type::UInt8));
        }
        if !suspends || actual != self.signature() {
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
#[export_name = "__joky_domain_poll_v3"]
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
        20 => Operation::Pair,
        21 => Operation::SnapshotPair,
        22 => Operation::CandidatePair,
        23 => Operation::CommitPair,
        24 => Operation::CommitPairBefore,
        25 => Operation::CommitSecondRevision,
        26 => Operation::AccessSet,
        27 => Operation::AccessInclude,
        28 => Operation::SnapshotSet,
        29 => Operation::SetRead,
        30 => Operation::SetCheck,
        31 => Operation::SetCandidate,
        32 => Operation::SetAdd,
        33 => Operation::CommitSet,
        34 => Operation::CommitSetBefore,
        35 => Operation::SetRevision,
        36 => Operation::AdmissionCreate,
        37 => Operation::AdmissionHandle,
        38 => Operation::AdmissionClose,
        39 => Operation::AdmissionTry,
        40 => Operation::AdmissionAcquire,
        41 => Operation::AdmissionStatus,
        // Compatibility with objects emitted before VM calls had their own adapter.
        42 => return crate::stdlib::call_poll_native(frame),
        43 => Operation::OutboxCreate,
        44 => Operation::OutboxHandle,
        45 => Operation::OutboxClose,
        46 => Operation::OutboxBatch,
        47 => Operation::OutboxStage,
        48 => Operation::OutboxReceive,
        49 => Operation::OutboxAttach,
        50 => Operation::OutboxAttachPair,
        51 => Operation::OutboxAttachSet,
        52 => Operation::OutboxMessageClosed,
        53 => Operation::OutboxMessageBytes,
        54 => Operation::DirectoryCreate,
        55 => Operation::DirectoryHandle,
        56 => Operation::DirectoryClose,
        57 => Operation::DirectoryInsert,
        58 => Operation::DirectoryReplace,
        59 => Operation::DirectoryRemove,
        60 => Operation::DirectoryLookup,
        61 => Operation::DirectoryFound,
        62 => Operation::TimerCreate,
        63 => Operation::TimerHandle,
        64 => Operation::TimerClose,
        65 => Operation::TimerCancel,
        66 => Operation::TimerStatus,
        67 => Operation::TimerStart,
        68 => Operation::Values,
        69 => Operation::CurrentValues,
        70 => Operation::Query,
        71 => Operation::CurrentQuery,
        72 => Operation::CreateValues,
        73 => Operation::CandidateValues,
        74 => Operation::CandidatePairValues,
        75 => Operation::SetCandidateValues,
        76 => Operation::SetAddValues,
        77 => Operation::SnapshotValues,
        78 => Operation::SnapshotPairValues,
        79 => Operation::SnapshotReadValues,
        80 => Operation::SetReadValues,
        81 => Operation::RequestValues,
        82 => Operation::TimerStartValues,
        83 => Operation::CandidateAt,
        84 => Operation::CandidatePairAt,
        85 => Operation::CandidateAtValues,
        86 => Operation::CandidatePairAtValues,
        87 => Operation::SetRevisionChecked,
        88 => Operation::SetEditSnapshot,
        89 => Operation::SetEditCandidate,
        90 => Operation::When,
        91 => Operation::PeriodicStatus,
        92 => Operation::CommitFailedOutbox,
        93 => Operation::DirectoryInsertOwned,
        94 => Operation::OutboxCreateDisconnect,

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
            if op == Operation::OutboxMessageBytes {
                // Native aggregate results follow the one parameter lane.
                (&mut (*frame).slots)[1] = value;
            }
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

pub(crate) use crate::vm::state_domains::emit;
/// # Safety
/// The compiler supplies a live shared UInt8 buffer.
#[export_name = "__joky_outbox_emit_v1"]
pub(crate) unsafe extern "C" fn emit_native(token: u64, bytes: *const super::buffer::Buffer) {
    if let Err(error) = emit(token, &*bytes) {
        super::error::record_error(error);
    }
}
