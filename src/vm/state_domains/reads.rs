//! Opaque, bounded read credentials captured in one synchronous executor turn.
use super::*;
pub(super) struct Charge {
    budget: Rc<Budget>,
    bytes: usize,
}
impl Charge {
    pub(super) fn new(budget: Rc<Budget>, bytes: usize) -> Result<Self, Error> {
        budget.charge(bytes)?;
        Ok(Self { budget, bytes })
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.refund(self.bytes);
    }
}
struct Extra {
    versions: Vec<Version>,
    _charge: Charge,
}
#[derive(Clone, Copy)]
pub(super) struct Version {
    pub(super) handle: DomainHandle,
    pub(super) revision: u64,
    pub(super) code: u64,
}
#[derive(Clone)]
pub(super) struct ReadSet {
    pub(super) first: Version,
    second: Option<Version>,
    extra: Option<Rc<Extra>>,
}
impl ReadSet {
    pub(super) fn single(handle: DomainHandle, revision: u64, code: u64) -> Self {
        Self {
            first: Version {
                handle,
                revision,
                code,
            },
            second: None,
            extra: None,
        }
    }
    pub(super) fn capture(domains: &Domains, handles: [DomainHandle; 2]) -> Self {
        let mut reads = Self::single(
            handles[0],
            domains.entries[&handles[0].sequence].revision,
            domains.entries[&handles[0].sequence].code,
        );
        if handles[0].sequence != handles[1].sequence {
            reads.second = Some(Version {
                handle: handles[1],
                revision: domains.entries[&handles[1].sequence].revision,
                code: domains.entries[&handles[1].sequence].code,
            });
        }
        reads
    }
    pub(super) fn capture_many(domains: &Domains, handles: &[DomainHandle]) -> Result<Self, Error> {
        let first = *handles
            .first()
            .ok_or_else(|| invalid("Empty domain access set"))?;
        let mut result = Self::single(
            first,
            domains.get(first)?.revision,
            domains.get(first)?.code,
        );
        if handles.len() == 2 {
            domains.get(handles[1])?;
            return Ok(Self::capture(domains, [first, handles[1]]));
        }
        let mut versions = Vec::new();
        versions
            .try_reserve_exact(handles.len())
            .map_err(|_| limited("Read set allocation failed"))?;
        for handle in handles {
            domains.get(*handle)?;
            if handle.sequence != first.sequence
                && !versions
                    .iter()
                    .any(|v: &Version| v.handle.sequence == handle.sequence)
            {
                versions.push(Version {
                    handle: *handle,
                    revision: domains.get(*handle)?.revision,
                    code: domains.get(*handle)?.code,
                });
            }
        }
        if !versions.is_empty() {
            result.second = Some(versions.remove(0));
        }
        if !versions.is_empty() {
            let charge = Charge::new(
                domains.budget.clone(),
                std::mem::size_of::<Extra>() + versions.capacity() * std::mem::size_of::<Version>(),
            )?;
            result.extra = Some(Rc::new(Extra {
                versions,
                _charge: charge,
            }));
        }
        Ok(result)
    }
    fn iter(&self) -> impl Iterator<Item = &Version> {
        std::iter::once(&self.first)
            .chain(self.second.iter())
            .chain(self.extra.iter().flat_map(|e| e.versions.iter()))
    }
    pub(super) fn version(&self, handle: DomainHandle) -> Option<Version> {
        self.iter()
            .find(|v| v.handle.registry == handle.registry && v.handle.sequence == handle.sequence)
            .copied()
    }
    pub(super) fn second(&self) -> Option<Version> {
        self.second
    }
    pub(super) fn touches(&self, domain: u64) -> bool {
        self.iter().any(|v| v.handle.sequence == domain)
    }
    pub(super) fn validate(&self, domains: &Domains) -> Result<(), commits::Status> {
        use commits::Status;
        // Validate identities/lifecycles across the full set before data and code versions.
        for version in self.iter() {
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
        for version in self.iter() {
            let domain = &domains.entries[&version.handle.sequence];
            if domain.revision != version.revision || domain.code != version.code {
                return Err(Status::Conflict);
            }
        }
        Ok(())
    }
}
