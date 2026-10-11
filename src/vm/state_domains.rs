//! Host-driven scalar state domains. Source task/capture integration is separate.
use super::{engine::LoadedModule, InstanceValue, InterpreterLimits, VmEngine};
use crate::{
    core::{instance, ir::FunctionInfo},
    runtime::arena::{Arena, Budget},
    Error, ErrorCode, ErrorPhase, Type,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::atomic::{AtomicU64, Ordering},
};

fn error(code: ErrorCode, message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, code, message)
}
fn invalid(message: &str) -> Error {
    error(ErrorCode::InvalidCall, message)
}
fn limited(message: &str) -> Error {
    error(ErrorCode::ResourceLimit, message)
}

/// Limits include completed requests until their unique owner collects or drops them.
#[derive(Clone, Copy, Debug)]
pub struct DomainLimits {
    pub max_domains: usize,
    pub max_state_bytes: usize,
    pub max_requests: usize,
    /// Logical request storage: entry, function name and scalar input storage.
    pub max_request_bytes: usize,
}
impl Default for DomainLimits {
    fn default() -> Self {
        Self {
            max_domains: 4096,
            max_state_bytes: 64 * 1024 * 1024,
            max_requests: 4096,
            max_request_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Opaque capability; copying it never keeps state alive or grants write access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomainHandle {
    registry: u64,
    sequence: u64,
    writable: bool,
}
impl DomainHandle {
    pub fn read_only(self) -> Self {
        Self {
            writable: false,
            ..self
        }
    }
}

/// Unique close responsibility. Dropping an owner closes state and queued operations.
/// The owner does not keep its VM alive.
pub struct DomainOwner {
    domains: Weak<RefCell<Domains>>,
    handle: DomainHandle,
}
impl DomainOwner {
    pub fn read_handle(&self) -> DomainHandle {
        self.handle.read_only()
    }
    pub fn write_handle(&self) -> DomainHandle {
        self.handle
    }
    pub fn close(&self) {
        if let Some(domains) = self.domains.upgrade() {
            domains.borrow_mut().close(self.handle.sequence);
        }
    }
}
impl Drop for DomainOwner {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone, Debug)]
pub enum DomainOutcome {
    Value(InstanceValue),
    Cancelled,
    Closed,
    Failed(Error),
}

/// Unique request/result owner. Drop removes pending work and releases its quota.
pub struct DomainRequest {
    domains: Weak<RefCell<Domains>>,
    sequence: Option<u64>,
}
impl DomainRequest {
    /// Cancellation is effective only before a short operation begins.
    pub fn cancel(&self) {
        if let (Some(domains), Some(sequence)) = (self.domains.upgrade(), self.sequence) {
            let mut domains = domains.borrow_mut();
            if let Some(request) = domains.requests.get_mut(&sequence) {
                if request.outcome.is_none() {
                    request.outcome = Some(DomainOutcome::Cancelled);
                    domains.queue.retain(|id| *id != sequence);
                }
            }
        }
    }
    /// None means pending or already collected. VM destruction yields Closed once.
    pub fn take_result(&mut self) -> Option<DomainOutcome> {
        let sequence = self.sequence?;
        let Some(domains) = self.domains.upgrade() else {
            self.sequence = None;
            return Some(DomainOutcome::Closed);
        };
        let mut domains = domains.borrow_mut();
        domains.requests.get(&sequence)?.outcome.as_ref()?;
        let request = domains.remove_request(sequence)?;
        self.sequence = None;
        request.outcome
    }
}
impl Drop for DomainRequest {
    fn drop(&mut self) {
        if let (Some(domains), Some(sequence)) = (self.domains.upgrade(), self.sequence) {
            domains.borrow_mut().remove_request(sequence);
        }
    }
}

struct Root {
    // The arena owns the allocation; no user cleanup is admitted for scalar roots.
    _arena: Arena,
    pointer: NonNull<u8>,
}
struct Domain {
    root: Option<Root>,
    module: LoadedModule,
    ty: Type,
    revision: u64,
    failure: Option<Error>,
}
struct Request {
    domain: u64,
    method: String,
    info: FunctionInfo,
    // Scalar copies form an independent input environment, never state references.
    words: Vec<u64>,
    limits: InterpreterLimits,
    charge: usize,
    outcome: Option<DomainOutcome>,
}
pub(super) struct Domains {
    identity: u64,
    next_domain: u64,
    next_request: u64,
    entries: HashMap<u64, Domain>,
    requests: HashMap<u64, Request>,
    queue: VecDeque<u64>,
    request_bytes: usize,
    budget: Rc<Budget>,
    limits: DomainLimits,
}
impl Default for Domains {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let limits = DomainLimits::default();
        Self {
            identity: NEXT
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect("State domain registry identities exhausted"),
            next_domain: 1,
            next_request: 1,
            entries: HashMap::new(),
            requests: HashMap::new(),
            queue: VecDeque::new(),
            request_bytes: 0,
            budget: Rc::new(Budget {
                bytes: Cell::new(0),
                limit: Cell::new(limits.max_state_bytes),
            }),
            limits,
        }
    }
}
impl Domains {
    fn get(&self, handle: DomainHandle) -> Result<&Domain, Error> {
        if handle.registry != self.identity {
            return Err(invalid("State handle belongs to another VM"));
        }
        let domain = self
            .entries
            .get(&handle.sequence)
            .ok_or_else(|| invalid("State domain is closed"))?;
        if let Some(failure) = &domain.failure {
            return Err(failure.clone());
        }
        Ok(domain)
    }
    fn remove_request(&mut self, sequence: u64) -> Option<Request> {
        let request = self.requests.remove(&sequence)?;
        self.request_bytes -= request.charge;
        self.queue.retain(|id| *id != sequence);
        Some(request)
    }
    fn finish_pending(&mut self, domain: u64, outcome: DomainOutcome) {
        self.queue.retain(|id| {
            let request = self.requests.get_mut(id).unwrap();
            if request.domain != domain {
                return true;
            }
            request.outcome = Some(outcome.clone());
            false
        });
    }
    fn close(&mut self, sequence: u64) {
        self.entries.remove(&sequence);
        self.finish_pending(sequence, DomainOutcome::Closed);
    }
}

impl VmEngine {
    pub fn set_domain_limits(&mut self, limits: DomainLimits) -> Result<(), Error> {
        let mut domains = self.domains.borrow_mut();
        if limits.max_domains == 0
            || limits.max_state_bytes == 0
            || limits.max_requests == 0
            || limits.max_request_bytes == 0
            || domains.entries.len() > limits.max_domains
            || domains.budget.bytes.get() > limits.max_state_bytes
            || domains.requests.len() > limits.max_requests
            || domains.request_bytes > limits.max_request_bytes
        {
            return Err(invalid(
                "Domain limits cannot exclude live state or requests",
            ));
        }
        domains.budget.limit.set(limits.max_state_bytes);
        domains.limits = limits;
        Ok(())
    }

    /// Construct a flat scalar record through a compiler-proved host-free entry.
    /// Existing persistent instances and their fail-fast task groups are independent.
    pub fn create_domain(
        &self,
        constructor: &str,
        args: &[InstanceValue],
    ) -> Result<DomainOwner, Error> {
        let module = self
            .current
            .clone()
            .ok_or_else(|| invalid("No module loaded"))?;
        let info = module.instance_info(constructor)?;
        if !info.domain_entry || !instance::constructor(&info) {
            return Err(invalid(
                "Domain constructor requires a proved scalar state entry",
            ));
        }
        let words = arguments(&info.signature.params, args)?;
        let mut domains = self.domains.borrow_mut();
        if domains.entries.len() >= domains.limits.max_domains {
            return Err(limited("State domain count limit exceeded"));
        }
        let sequence = domains.next_domain;
        let next = sequence
            .checked_add(1)
            .ok_or_else(|| limited("State domain identities exhausted"))?;
        domains
            .entries
            .try_reserve(1)
            .map_err(|_| limited("State domain registry allocation failed"))?;
        let mut arena = Arena::with_budget(domains.budget.clone(), None);
        let layout = info
            .signature
            .ret
            .storage_layout()
            .map_err(|e| invalid(&e.to_string()))?;
        let pointer = arena.allocate(layout.size, layout.align)?;
        // Constructor temporaries cannot allocate or invoke user cleanup. Root padding
        // is initialized as well, independently of each backend's scalar ABI.
        unsafe {
            pointer.as_ptr().write_bytes(0, layout.size.max(1));
        }
        let result =
            module.invoke_instance(constructor, &words, self.interpreter_limits(), &mut arena)?;
        super::instances::initialize(pointer, &info.signature.ret, &result)?;
        domains.entries.insert(
            sequence,
            Domain {
                root: Some(Root {
                    _arena: arena,
                    pointer,
                }),
                module,
                ty: info.signature.ret,
                revision: 0,
                failure: None,
            },
        );
        domains.next_domain = next;
        Ok(DomainOwner {
            domains: Rc::downgrade(&self.domains),
            handle: DomainHandle {
                registry: domains.identity,
                sequence,
                writable: true,
            },
        })
    }

    pub fn domain_revision(&self, handle: DomainHandle) -> Result<u64, Error> {
        Ok(self.domains.borrow().get(handle)?.revision)
    }

    /// Validate and copy inputs before FIFO admission; never invoke user code here.
    pub fn queue_domain_call(
        &self,
        handle: DomainHandle,
        method: &str,
        args: &[InstanceValue],
    ) -> Result<DomainRequest, Error> {
        let mut domains = self.domains.borrow_mut();
        let domain = domains.get(handle)?;
        let info = domain.module.instance_info(method)?;
        if !info.domain_entry || instance::receiver(&info) != Some(&domain.ty) {
            return Err(invalid(
                "Domain operation requires a proved entry for this exact state type",
            ));
        }
        let mutable = info.signature.params[0].reference_type().unwrap().mutable;
        if mutable && !handle.writable {
            return Err(invalid("Read-only state handle cannot authorize a write"));
        }
        if domains.requests.len() >= domains.limits.max_requests {
            return Err(limited("Domain request count limit exceeded"));
        }
        let charge = args
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(std::mem::size_of::<u64>()))
            .and_then(|n| n.checked_add(method.len()))
            .and_then(|n| n.checked_add(std::mem::size_of::<Request>()))
            .ok_or_else(|| limited("Domain request size overflow"))?;
        let bytes = domains
            .request_bytes
            .checked_add(charge)
            .filter(|n| *n <= domains.limits.max_request_bytes)
            .ok_or_else(|| limited("Domain request byte limit exceeded"))?;
        let mut words = arguments(&info.signature.params[1..], args)?;
        words
            .try_reserve(1)
            .map_err(|_| limited("Domain input allocation failed"))?;
        words.insert(0, 0); // Resolve state only when the operation is granted access.
        let sequence = domains.next_request;
        let next = sequence
            .checked_add(1)
            .ok_or_else(|| limited("Domain request identities exhausted"))?;
        domains
            .requests
            .try_reserve(1)
            .map_err(|_| limited("Domain request registry allocation failed"))?;
        domains
            .queue
            .try_reserve(1)
            .map_err(|_| limited("Domain queue allocation failed"))?;
        domains.requests.insert(
            sequence,
            Request {
                domain: handle.sequence,
                method: method.to_owned(),
                info,
                words,
                limits: self.interpreter_limits(),
                charge,
                outcome: None,
            },
        );
        domains.queue.push_back(sequence);
        domains.request_bytes = bytes;
        domains.next_request = next;
        Ok(DomainRequest {
            domains: Rc::downgrade(&self.domains),
            sequence: Some(sequence),
        })
    }

    /// Drive at most `max_operations` queued synchronous turns. This is an operation
    /// budget, not a time bound: a cooperative turn may run arbitrarily long.
    /// No operation can suspend, call hosts or recursively acquire state permissions.
    pub fn poll_domains(&self, max_operations: usize) -> usize {
        let mut domains = self.domains.borrow_mut();
        let count = domains.queue.len().min(max_operations);
        let mut ran = 0;
        for _ in 0..count {
            let Some(sequence) = domains.queue.pop_front() else {
                break;
            };
            let mut request = domains.requests.remove(&sequence).unwrap();
            let domain = domains.entries.get_mut(&request.domain).unwrap();
            let mutable = request.info.signature.params[0]
                .reference_type()
                .unwrap()
                .mutable;
            let next = domain.revision.checked_add(u64::from(mutable));
            let outcome = if let Some(next) = next {
                request.words[0] = domain.root.as_ref().unwrap().pointer.as_ptr() as u64;
                // Allocating operations are excluded by the compiler. The separate
                // empty arena never lends request storage to the persistent root.
                let mut arena = Arena::default();
                let result = domain
                    .module
                    .invoke_instance(&request.method, &request.words, request.limits, &mut arena)
                    .and_then(|words| {
                        InstanceValue::from_bits(
                            &request.info.signature.ret,
                            words.first().copied().unwrap_or(0),
                        )
                        .ok_or_else(|| invalid("Invalid domain scalar result"))
                    });
                match result {
                    Ok(value) => {
                        domain.revision = next;
                        DomainOutcome::Value(value)
                    }
                    Err(error) => {
                        if mutable {
                            domain.root = None;
                            domain.failure = Some(error.clone());
                        }
                        DomainOutcome::Failed(error)
                    }
                }
            } else {
                DomainOutcome::Failed(limited("State domain revision exhausted"))
            };
            if mutable && domain.failure.is_some() {
                domains.finish_pending(request.domain, outcome.clone());
            }
            // Erase the temporary address before retaining a completed receipt.
            request.words[0] = 0;
            request.outcome = Some(outcome);
            domains.requests.insert(sequence, request);
            ran += 1;
        }
        ran
    }
}

fn arguments(types: &[Type], values: &[InstanceValue]) -> Result<Vec<u64>, Error> {
    if types.len() != values.len()
        || types
            .iter()
            .zip(values)
            .any(|(ty, v)| *ty != v.value_type())
    {
        return Err(invalid("Domain argument signature mismatch"));
    }
    Ok(values.iter().map(|value| value.bits()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{state_domain_tests::SOURCE, VmMode};

    #[test]
    fn state_domains_limits_and_exhausted_revision_fail_before_mutation() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: 1,
                ..Default::default()
            })
            .unwrap();
            assert!(vm
                .create_domain("make", &[InstanceValue::Int32(1)])
                .is_err());
            assert_eq!(vm.domains.borrow().budget.bytes.get(), 0);
            vm.set_domain_limits(DomainLimits {
                max_domains: 1,
                max_request_bytes: 1,
                ..Default::default()
            })
            .unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            assert!(vm
                .create_domain("make", &[InstanceValue::Int32(1)])
                .is_err());
            assert!(vm
                .queue_domain_call(owner.read_handle(), "read", &[])
                .is_err());
            assert_eq!(vm.domains.borrow().request_bytes, 0);
            assert!(vm.domains.borrow().queue.is_empty());
            vm.set_domain_limits(Default::default()).unwrap();
            vm.domains
                .borrow_mut()
                .entries
                .get_mut(&owner.handle.sequence)
                .unwrap()
                .revision = u64::MAX;
            let mut write = vm
                .queue_domain_call(owner.write_handle(), "add", &[InstanceValue::Int32(10)])
                .unwrap();
            let mut read = vm
                .queue_domain_call(owner.read_handle(), "read", &[])
                .unwrap();
            vm.poll_domains(10);
            assert!(
                matches!(write.take_result(), Some(DomainOutcome::Failed(e)) if e.code() == ErrorCode::ResourceLimit)
            );
            assert!(matches!(
                read.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(7)))
            ));
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), u64::MAX);
            let mut domains = vm.domains.borrow_mut();
            domains.next_request = u64::MAX;
            drop(domains);
            assert!(vm
                .queue_domain_call(owner.read_handle(), "read", &[])
                .is_err());
            owner.close();
            vm.domains.borrow_mut().next_domain = u64::MAX;
            assert!(vm
                .create_domain("make", &[InstanceValue::Int32(1)])
                .is_err());
            assert_eq!(vm.domains.borrow().budget.bytes.get(), 0);
        }
    }

    #[test]
    fn state_domains_cancel_close_poll_orders_have_one_terminal_result() {
        lifecycle_orders(1);
    }

    #[test]
    #[ignore]
    fn exhaustive_state_domains_lifecycle_orders_restore_all_quotas() {
        lifecycle_orders(64);
    }

    fn lifecycle_orders(rounds: usize) {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            for _ in 0..rounds {
                for order in [
                    [0, 1, 2],
                    [0, 2, 1],
                    [1, 0, 2],
                    [1, 2, 0],
                    [2, 0, 1],
                    [2, 1, 0],
                ] {
                    let owner = vm
                        .create_domain("make", &[InstanceValue::Int32(1)])
                        .unwrap();
                    let mut request = vm
                        .queue_domain_call(owner.write_handle(), "add", &[InstanceValue::Int32(2)])
                        .unwrap();
                    for event in order {
                        match event {
                            0 => request.cancel(),
                            1 => owner.close(),
                            2 => {
                                vm.poll_domains(1);
                            }
                            _ => unreachable!(),
                        }
                    }
                    match (order[0], request.take_result()) {
                        (0, Some(DomainOutcome::Cancelled))
                        | (1, Some(DomainOutcome::Closed))
                        | (2, Some(DomainOutcome::Value(InstanceValue::Int32(3)))) => {}
                        other => panic!("unexpected lifecycle result: {other:?}"),
                    }
                    assert!(request.take_result().is_none());
                    let domains = vm.domains.borrow();
                    assert!(domains.entries.is_empty());
                    assert!(domains.requests.is_empty());
                    assert!(domains.queue.is_empty());
                    assert_eq!(domains.request_bytes, 0);
                    assert_eq!(domains.budget.bytes.get(), 0);
                }
            }
        }
    }
}
