//! Opaque, bounded read credentials captured in one synchronous executor turn.
use super::*;
#[derive(Clone, Copy)]
pub(super) struct Version {
    pub(super) handle: DomainHandle,
    pub(super) revision: u64,
}
#[derive(Clone, Copy)]
pub(super) struct ReadSet {
    pub(super) first: Version,
    second: Option<Version>,
}
impl ReadSet {
    pub(super) fn single(handle: DomainHandle, revision: u64) -> Self {
        Self {
            first: Version { handle, revision },
            second: None,
        }
    }
    pub(super) fn capture(domains: &Domains, handles: [DomainHandle; 2]) -> Self {
        let mut reads = Self::single(handles[0], domains.entries[&handles[0].sequence].revision);
        if handles[0].sequence != handles[1].sequence {
            reads.second = Some(Version {
                handle: handles[1],
                revision: domains.entries[&handles[1].sequence].revision,
            });
        }
        reads
    }
    pub(super) fn touches(&self, domain: u64) -> bool {
        std::iter::once(&self.first)
            .chain(self.second.iter())
            .any(|v| v.handle.sequence == domain)
    }
    pub(super) fn validate(&self, domains: &Domains) -> Result<(), commits::Status> {
        use commits::Status;
        // Validate identities/lifecycles across the full set before revisions.
        for version in std::iter::once(&self.first).chain(self.second.iter()) {
            if version.handle.registry != domains.identity {
                return Err(Status::InvalidAccess);
            }
            if domains.shutting_down {
                return Err(Status::Closed);
            }
            let domain = domains
                .entries
                .get(&version.handle.sequence)
                .ok_or(Status::Closed)?;
            if domain.closing {
                return Err(Status::Closed);
            }
            if domain.failure.is_some() {
                return Err(Status::DomainFailed);
            }
        }
        for version in std::iter::once(&self.first).chain(self.second.iter()) {
            if domains.entries[&version.handle.sequence].revision != version.revision {
                return Err(Status::Conflict);
            }
        }
        Ok(())
    }
}
