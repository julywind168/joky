//! In-place calls expose preflight rejections. Candidate calls also expose
//! resource exhaustion after calculation without publishing any state.
use super::*;
use crate::runtime::domain::Operation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub(super) enum Rejection {
    Closed = 1,
    DomainFailed = 2,
    InvalidAccess = 3,
    Incompatible = 4,
    ResourceLimit = 5,
}
impl Rejection {
    pub(super) fn from_error(error: &Error) -> Self {
        match error.code() {
            ErrorCode::ResourceLimit => Self::ResourceLimit,
            ErrorCode::TypeMismatch | ErrorCode::NotFound => Self::Incompatible,
            _ => Self::InvalidAccess,
        }
    }
}
fn lifecycle(domains: &Domains, domain: Option<&Domain>) -> Option<Rejection> {
    if domains.shutting_down || domain.is_none_or(|d| d.closing) {
        Some(Rejection::Closed)
    } else if domain.is_some_and(|d| d.failure.is_some()) {
        Some(Rejection::DomainFailed)
    } else {
        None
    }
}
pub(super) fn classify(domains: &Domains, handles: &[DomainHandle], error: &Error) -> Rejection {
    for handle in handles {
        if handle.registry != domains.identity {
            return Rejection::InvalidAccess;
        }
        if let Some(status) = lifecycle(domains, domains.entries.get(&handle.sequence)) {
            return status;
        }
    }
    Rejection::from_error(error)
}
pub(super) fn source_rejection(
    registry: &Rc<RefCell<Domains>>,
    frame: &CpsFrame,
    op: Operation,
    error: &Error,
) -> Rejection {
    if matches!(
        op,
        Operation::CurrentRead
            | Operation::CurrentWrite
            | Operation::CurrentQuery
            | Operation::CurrentValues
    ) {
        return tasks::access_rejection(registry, error);
    }
    let count = if op == Operation::Pair {
        2
    } else if op == Operation::Values {
        (frame.slots[3] as usize).min(2)
    } else {
        1
    };
    source_tokens(registry, &frame.slots[1..1 + count], error)
}
pub(super) fn source_tokens(
    registry: &Rc<RefCell<Domains>>,
    tokens: &[u64],
    error: &Error,
) -> Rejection {
    let domains = registry.borrow();
    for token in tokens {
        // Source capabilities intentionally carry no registry identity. Unknown
        // tokens (including a different executor's) are unavailable here: Closed.
        let domain = domains
            .entries
            .values()
            .find(|d| d.source_token == token / 2);
        if let Some(status) = lifecycle(&domains, domain) {
            return status;
        }
    }
    Rejection::from_error(error)
}
pub(super) fn reply(frame: &mut CpsFrame, op: Operation, status: u64, bits: u64) -> u64 {
    let offset = match op {
        Operation::CurrentRead => 1,
        Operation::CurrentWrite | Operation::CurrentQuery => 2,
        Operation::Read => 3,
        Operation::Write | Operation::Query => 4,
        Operation::Pair => 5,
        _ => unreachable!("Only scalar access operations return status and payload slots"),
    };
    frame.slots[offset] = status;
    frame.slots[offset + 1] = bits;
    0
}

#[cfg(test)]
mod tests;
