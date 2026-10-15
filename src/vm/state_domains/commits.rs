//! Independently owned candidates and bounded, retained commit receipts.
use super::*;
use crate::runtime::{
    arena::ExternalCharge, buffer::Buffer, domain::Operation, resource::OwnedResource,
};
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

mod bundles;
pub(super) mod sets;
pub(super) use bundles::Bundle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub(super) enum Status {
    Committed,
    Conflict,
    Closed,
    DomainFailed,
    InvalidAccess,
    Cancelled,
    ResourceLimit,
    Expired,
    OutboxFull,
    OutboxClosed,
}
pub(super) struct Candidate {
    outbox: Option<Box<outbox::Batch>>,
    root: Root,
    module: LoadedModule,
    ty: Type,
    base: DomainHandle,
    pub(super) reads: reads::ReadSet,
    pub(super) deadline: Option<Deadline>,
}
impl Candidate {
    pub(crate) fn holds_module(&self, key: usize) -> bool {
        code::module_key(&self.module) == key
    }
}
#[derive(Clone, Copy)]
pub(super) struct Deadline {
    registry: u64,
    at: Instant,
}
fn deadline_after(registry: &Rc<RefCell<Domains>>, milliseconds: i32) -> Result<Deadline, Error> {
    let duration = u64::try_from(milliseconds)
        .map_err(|_| invalid("Commit deadline duration must be nonnegative"))?;
    let (identity, executor) = {
        let domains = registry.borrow();
        (
            domains.identity,
            domains
                .executor
                .upgrade()
                .ok_or_else(|| invalid("Domain executor is closed"))?,
        )
    };
    // A host clock may reenter the VM. No registry borrow crosses that callback.
    let at = executor
        .now()
        .checked_add(Duration::from_millis(duration))
        .ok_or_else(|| limited("Commit deadline overflow"))?;
    Ok(Deadline {
        registry: identity,
        at,
    })
}
struct Receipt {
    request: Option<DomainRequest>,
    rejected: Option<Status>,
}
pub(super) struct Stored<T> {
    pub(super) value: RefCell<T>,
    _charge: ExternalCharge,
}
unsafe extern "C" fn release<T>(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Stored<T>>()));
}
pub(super) fn adopt<T>(arena: &mut Arena, value: T) -> Result<u64, Error> {
    let charge = arena.reserve_external(std::mem::size_of::<Stored<T>>())?;
    let token = NonNull::new(
        Box::into_raw(Box::new(Stored {
            value: RefCell::new(value),
            _charge: charge,
        }))
        .cast(),
    )
    .unwrap();
    let resource = match OwnedResource::from_token(token, release::<T>) {
        Ok(resource) => resource,
        Err(error) => {
            unsafe { release::<T>(token.as_ptr()) };
            return Err(error);
        }
    };
    Ok(Buffer::external(arena, resource)?.as_ptr() as usize as u64)
}
/// # Safety
/// `bits` must point to a live managed resource buffer on this thread. Its owner
/// must remain live until the returned operation lease has been dropped.
pub(super) unsafe fn lease(bits: u64) -> Result<crate::runtime::resource::ResourceLease, Error> {
    (bits as *mut Buffer)
        .as_mut()
        .ok_or_else(|| invalid("Null domain resource"))?
        .resource_mut()?
        .lease()
}
enum ConstructorInput {
    Scalar(InstanceValue),
    Values(Vec<u64>),
}
impl From<InstanceValue> for ConstructorInput {
    fn from(value: InstanceValue) -> Self {
        Self::Scalar(value)
    }
}
/// # Safety
/// For a value operation, bits points to a live compiler-owned UInt64 buffer.
unsafe fn constructor_input(
    module: &LoadedModule,
    name: &str,
    bits: u64,
    op: Operation,
) -> Result<ConstructorInput, Error> {
    let info = module.instance_info(name)?;
    if matches!(
        op,
        Operation::CandidateValues
            | Operation::CandidatePairValues
            | Operation::SetCandidateValues
            | Operation::SetAddValues
    ) {
        if !info.domain_entry || !crate::core::state_domain::constructor_eligible(&info) {
            return Err(invalid(
                "Candidate requires a proved bounded pure-value constructor",
            ));
        }
        let packet = (bits as *const Buffer)
            .as_ref()
            .ok_or_else(|| invalid("Missing candidate input packet"))?;
        Ok(ConstructorInput::Values(values::input_words(
            &info.signature.params,
            packet,
        )?))
    } else {
        source::scalar_argument(&info.signature.params, bits).map(ConstructorInput::Scalar)
    }
}
fn construct(
    registry: &Rc<RefCell<Domains>>,
    module: LoadedModule,
    base: DomainHandle,
    revision: u64,
    name: &str,
    input: impl Into<ConstructorInput>,
    limits: InterpreterLimits,
) -> Result<Candidate, Error> {
    let info = module.instance_info(name)?;
    let (budget, code, schema) = {
        let domains = registry.borrow();
        let domain = domains.get(base)?;
        if !source::same_module(&module, &domain.module)
            || info.signature.ret != domain.ty
            || !info.domain_entry
            || !crate::core::state_domain::constructor_eligible(&info)
        {
            return Err(invalid("Candidate requires a proved constructor for the snapshot's exact module and state type"));
        }
        (domains.budget.clone(), domain.code, domain.schema)
    };
    let words = match input.into() {
        ConstructorInput::Scalar(input) => arguments(&info.signature.params, &[input])?,
        ConstructorInput::Values(words) => words,
    };
    let root = Root::construct(&module, &info, name, &words, limits, budget)?;
    Ok(Candidate {
        outbox: None,
        root,
        module,
        ty: info.signature.ret,
        base,
        reads: reads::ReadSet::single(base, revision, code, schema),
        deadline: None,
    })
}
fn validate(domains: &Domains, candidate: &Candidate, now: Option<Instant>) -> Result<u64, Status> {
    if candidate.base.registry != domains.identity {
        return Err(Status::InvalidAccess);
    }
    if domains.shutting_down {
        return Err(Status::Closed);
    }
    let domain = domains
        .entries
        .get(&candidate.base.sequence)
        .ok_or(Status::Closed)?;
    if domain.closing {
        return Err(Status::Closed);
    }
    if domain.failure.is_some() {
        return Err(Status::DomainFailed);
    }
    if let Some(deadline) = candidate.deadline {
        if deadline.registry != domains.identity {
            return Err(Status::InvalidAccess);
        }
        if now.expect("Timed commit samples its executor clock") >= deadline.at {
            return Err(Status::Expired);
        }
    }
    candidate.reads.validate(domains)?;
    if domain.ty != candidate.ty || !source::same_module(&domain.module, &candidate.module) {
        return Err(Status::Conflict);
    }
    domain.revision.checked_add(1).ok_or(Status::ResourceLimit)
}
pub(super) fn execute(domains: &mut Domains, request: &mut Request, now: Option<Instant>) {
    if matches!(request.work, Work::CommitSet(_)) {
        sets::execute_commit(domains, request, now);
        return;
    }
    if matches!(request.work, Work::CommitPair(_)) {
        bundles::execute(domains, request, now);
        return;
    }
    let Work::Commit(candidate) = &mut request.work else {
        unreachable!()
    };
    let mut candidate = candidate.take().unwrap();
    let next = match validate(domains, &candidate, now) {
        Ok(next) => next,
        Err(status) => {
            request.commit_status = Some(status);
            request.finish(DomainOutcome::Value(InstanceValue::Unit));
            return;
        }
    };
    if let Some(batch) = &mut candidate.outbox {
        if let Err(status) = batch.prepare(domains) {
            request.commit_status = Some(status);
            request.finish(DomainOutcome::Value(InstanceValue::Unit));
            return;
        }
    }
    let domain = domains.entries.get_mut(&candidate.base.sequence).unwrap();
    // Publication: only ownership moves and scalar stores; no user code,
    // allocation, cancellation point or fallible operation in this interval.
    let retired = domain.root.replace(candidate.root);
    domain.revision = next;
    if let Some(batch) = &mut candidate.outbox {
        batch.publish();
    }
    request.observed_revision = next;
    request.commit_status = Some(Status::Committed);
    request.outcome = Some(DomainOutcome::Value(InstanceValue::Unit));
    if let Some(batch) = &candidate.outbox {
        batch.wake();
    }
    // Retired storage is released only after the receipt records the fact.
    drop(retired);
    request.finish(DomainOutcome::Value(InstanceValue::Unit));
}
fn access(domains: &Domains, token: u64, candidate: &Candidate) -> Result<(), Status> {
    if candidate.base.registry != domains.identity
        || candidate
            .deadline
            .is_some_and(|d| d.registry != domains.identity)
    {
        Err(Status::InvalidAccess)
    } else if domains.shutting_down {
        Err(Status::Closed)
    } else if let Some(domain) = domains.entries.get(&candidate.base.sequence) {
        if domain.closing {
            Err(Status::Closed)
        } else if domain.failure.is_some() {
            Err(Status::DomainFailed)
        } else if token != domain.source_token * 2 + 1 {
            Err(Status::InvalidAccess)
        } else {
            Ok(())
        }
    } else {
        Err(Status::Closed)
    }
}
fn admit(registry: &Rc<RefCell<Domains>>, token: u64, candidate: Candidate) -> Receipt {
    let access = access(&registry.borrow(), token, &candidate);
    let result = access.and_then(|()| {
        enqueue(
            registry,
            candidate.base.sequence,
            Work::Commit(Some(candidate)),
            0,
        )
        .map_err(|_| Status::ResourceLimit)
    });
    match result {
        Ok(request) => Receipt {
            request: Some(request),
            rejected: None,
        },
        Err(status) => Receipt {
            request: None,
            rejected: Some(status),
        },
    }
}
impl Receipt {
    fn result(&self) -> Option<(Status, u64)> {
        if let Some(status) = self.rejected {
            return Some((status, 0));
        }
        let request = self.request.as_ref()?;
        let Some(domains) = request.domains.upgrade() else {
            return Some((Status::Closed, 0));
        };
        let domains = domains.borrow();
        let record = domains.requests.get(&request.sequence?)?;
        record
            .commit_status
            .map(|status| (status, record.observed_revision))
    }
}
pub(super) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<Option<u64>, Error> {
    if matches!(
        op,
        Operation::CandidatePair
            | Operation::CandidatePairValues
            | Operation::CommitPair
            | Operation::CommitPairBefore
    ) {
        return bundles::poll(arena, frame, registry, op);
    }
    if op == Operation::DeadlineAfter {
        return adopt(arena, deadline_after(registry, frame.slots[0] as i32)?).map(Some);
    }
    if matches!(op, Operation::Candidate | Operation::CandidateValues) {
        let snapshot_lease = lease(frame.slots[1])?;
        let snapshot = &*snapshot_lease.token().cast::<source::Snapshot>();
        let (module, _) = source::module(registry, frame.slots[0])?;
        let name = crate::runtime::owned_text::borrow(frame.slots[2] as *const Buffer)?;
        let input = constructor_input(&module, name, frame.slots[3], op)?;
        let mut candidate = construct(
            registry,
            module,
            snapshot.reads.first.handle,
            snapshot.reads.first.revision,
            name,
            input,
            crate::runtime::suspension::current_limits(),
        )?;
        candidate.reads = snapshot.reads.clone();
        return adopt(arena, Some(candidate)).map(Some);
    }
    if matches!(op, Operation::Commit | Operation::CommitBefore) {
        let candidate_lease = lease(frame.slots[1])?;
        let stored = &*candidate_lease.token().cast::<Stored<Option<Candidate>>>();
        let mut candidate = stored
            .value
            .borrow_mut()
            .take()
            .ok_or_else(|| invalid("Candidate was already consumed"))?;
        if op == Operation::CommitBefore {
            let deadline_lease = lease(frame.slots[2])?;
            let deadline = &*deadline_lease.token().cast::<Stored<Deadline>>();
            candidate.deadline = Some(*deadline.value.borrow());
        }
        // Allocate all caller-owned receipt storage before registering any work.
        let bits = adopt(
            arena,
            Receipt {
                request: None,
                rejected: None,
            },
        )?;
        let receipt_lease = lease(bits)?;
        let receipt = &*receipt_lease.token().cast::<Stored<Receipt>>();
        *receipt.value.borrow_mut() = admit(registry, frame.slots[0], candidate);
        return Ok(Some(bits));
    }
    let receipt_lease = lease(frame.slots[0])?;
    let stored = &*receipt_lease.token().cast::<Stored<Receipt>>();
    let receipt = stored.value.borrow();
    if receipt
        .request
        .as_ref()
        .and_then(|r| r.domains.upgrade())
        .is_some_and(|d| !Rc::ptr_eq(&d, registry))
    {
        return Err(invalid("Commit receipt belongs to another executor"));
    }
    match op {
        Operation::CommitWait => {
            if let Some((status, _)) = receipt.result() {
                return Ok(Some(status as u64));
            }
            receipt
                .request
                .as_ref()
                .ok_or_else(|| invalid("Commit receipt has no request"))?
                .register_current_wait()?;
            frame.wake = None;
            Ok(None)
        }
        Operation::CommitRevision => Ok(Some(receipt.result().map_or(0, |(_, revision)| revision))),
        Operation::CommitSecondRevision => Ok(Some(
            receipt
                .request
                .as_ref()
                .and_then(|request| {
                    let domains = request.domains.upgrade()?;
                    let domains = domains.borrow();
                    let record = domains.requests.get(&request.sequence?)?;
                    (record.commit_status == Some(Status::Committed))
                        .then_some(record.second_revision)
                })
                .unwrap_or(0),
        )),
        Operation::CommitIdentity => Ok(Some(
            receipt
                .request
                .as_ref()
                .and_then(|r| r.sequence)
                .unwrap_or(0),
        )),
        Operation::CommitCancel => {
            if let Some(request) = &receipt.request {
                request.cancel();
            }
            Ok(Some(0))
        }
        _ => unreachable!(),
    }
}

/// # Safety
/// The resource kind must match the compiler-checked attachment operation.
pub(super) unsafe fn attach_outbox(
    bits: u64,
    op: Operation,
    batch: &mut Option<outbox::Batch>,
) -> Result<(), Error> {
    let lease = lease(bits)?;
    match op {
        Operation::OutboxAttach => {
            let stored = &*lease.token().cast::<Stored<Option<Candidate>>>();
            let mut candidate = stored.value.borrow_mut();
            let candidate = candidate
                .as_mut()
                .ok_or_else(|| invalid("Candidate was already consumed"))?;
            attach(&mut candidate.outbox, batch)
        }
        Operation::OutboxAttachPair => {
            let stored = &*lease.token().cast::<Stored<Option<Bundle>>>();
            let mut candidate = stored.value.borrow_mut();
            let candidate = candidate
                .as_mut()
                .ok_or_else(|| invalid("Candidate pair was already consumed"))?;
            candidate.attach_outbox(batch)
        }
        Operation::OutboxAttachSet => {
            let stored = &*lease.token().cast::<Stored<Option<sets::Proposal>>>();
            let mut candidate = stored.value.borrow_mut();
            let candidate = candidate
                .as_mut()
                .ok_or_else(|| invalid("Candidate set was already consumed"))?;
            candidate.attach_outbox(batch)
        }
        _ => unreachable!(),
    }
}
fn attach(
    target: &mut Option<Box<outbox::Batch>>,
    batch: &mut Option<outbox::Batch>,
) -> Result<(), Error> {
    if target.is_some() {
        return Err(invalid("Candidate already has an outbox batch"));
    }
    *target =
        Some(Box::new(batch.take().ok_or_else(|| {
            invalid("Outbox batch was already attached")
        })?));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{state_domain_tests::SOURCE, VmMode};

    fn candidate(vm: &VmEngine, owner: &DomainOwner, value: i32) -> Candidate {
        construct(
            &vm.domains,
            vm.current.clone().unwrap(),
            owner.read_handle(),
            vm.domain_revision(owner.read_handle()).unwrap(),
            "make",
            InstanceValue::Int32(value),
            InterpreterLimits::default(),
        )
        .unwrap()
    }
    fn token(vm: &VmEngine, owner: &DomainOwner) -> u64 {
        vm.domains.borrow().entries[&owner.handle.sequence].source_token * 2 + 1
    }
    fn read(vm: &mut VmEngine, owner: &DomainOwner, expected: i32) {
        let mut request = vm
            .queue_domain_call(owner.read_handle(), "read", &[])
            .unwrap();
        vm.poll_tasks(100).unwrap();
        assert!(
            matches!(request.take_result(), Some(DomainOutcome::Value(InstanceValue::Int32(n))) if n == expected)
        );
    }
    #[test]
    fn state_domains_commit_fifo_conflicts_and_receipts_survive_close() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            let next = candidate(&vm, &owner, 12);
            assert!(vm.domains.borrow().budget.bytes.get() > baseline);
            read(&mut vm, &owner, 7);
            let receipt = admit(&vm.domains, token(&vm, &owner), next);
            let mut write = vm
                .queue_domain_call(owner.write_handle(), "add", &[InstanceValue::Int32(1)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert_eq!(receipt.result(), Some((Status::Committed, 1)));
            assert!(matches!(
                write.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(13)))
            ));
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            let stale = candidate(&vm, &owner, 99);
            let mut write = vm
                .queue_domain_call(owner.write_handle(), "add", &[InstanceValue::Int32(1)])
                .unwrap();
            let rejected = admit(&vm.domains, token(&vm, &owner), stale);
            vm.poll_tasks(100).unwrap();
            write.take_result();
            assert_eq!(rejected.result(), Some((Status::Conflict, 0)));
            read(&mut vm, &owner, 14);
            receipt.request.as_ref().unwrap().cancel();
            owner.close();
            assert_eq!(receipt.result(), Some((Status::Committed, 1)));
            assert_eq!(rejected.result(), Some((Status::Conflict, 0)));
            assert_eq!(vm.domains.borrow().usage().1, 2);
            drop((receipt, rejected));
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_commit_cancel_close_and_drop_before_publication_keep_old_root() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            for action in 0..3 {
                let owner = vm
                    .create_domain("make", &[InstanceValue::Int32(7)])
                    .unwrap();
                let baseline = vm.domains.borrow().budget.bytes.get();
                let next = candidate(&vm, &owner, 99);
                let receipt = admit(&vm.domains, token(&vm, &owner), next);
                match action {
                    0 => {
                        receipt.request.as_ref().unwrap().cancel();
                        assert_eq!(receipt.result(), Some((Status::Cancelled, 0)));
                        assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
                        vm.poll_tasks(100).unwrap();
                        read(&mut vm, &owner, 7);
                        assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
                    }
                    1 => {
                        owner.close();
                        assert_eq!(receipt.result(), Some((Status::Closed, 0)));
                    }
                    _ => {
                        drop(receipt);
                        read(&mut vm, &owner, 7);
                        owner.close();
                        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                        continue;
                    }
                }
                drop(receipt);
                owner.close();
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
    #[test]
    fn state_domains_candidate_failures_and_commit_limits_leave_state_usable() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&format!("{SOURCE}\nfn broken_make(zero: Int32) -> State {{ State {{ count: 1 / zero, scale: 0.0, flag: false }} }}")).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            assert!(construct(
                &vm.domains,
                vm.current.clone().unwrap(),
                owner.read_handle(),
                0,
                "broken_make",
                InstanceValue::Int32(0),
                InterpreterLimits::default()
            )
            .is_err());
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: baseline,
                ..Default::default()
            })
            .unwrap();
            assert!(construct(
                &vm.domains,
                vm.current.clone().unwrap(),
                owner.read_handle(),
                0,
                "make",
                InstanceValue::Int32(99),
                InterpreterLimits::default()
            )
            .is_err());
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            vm.set_domain_limits(Default::default()).unwrap();
            for failure in 0..4 {
                let next = candidate(&vm, &owner, 99);
                match failure {
                    0 => vm
                        .set_domain_limits(DomainLimits {
                            max_request_bytes: 1,
                            ..Default::default()
                        })
                        .unwrap(),
                    1 => vm.domains.borrow_mut().next_request = u64::MAX,
                    2 => {
                        vm.domains
                            .borrow_mut()
                            .entries
                            .get_mut(&owner.handle.sequence)
                            .unwrap()
                            .revision = u64::MAX;
                    }
                    _ => {}
                }
                let mut next = next;
                if failure == 2 {
                    next.reads.first.revision = u64::MAX;
                }
                let raw = token(&vm, &owner);
                let receipt = admit(&vm.domains, if failure == 3 { raw & !1 } else { raw }, next);
                vm.poll_tasks(100).unwrap();
                assert_eq!(
                    receipt.result().unwrap().0,
                    if failure == 3 {
                        Status::InvalidAccess
                    } else {
                        Status::ResourceLimit
                    }
                );
                drop(receipt);
                vm.domains.borrow_mut().next_request = 100;
                vm.domains
                    .borrow_mut()
                    .entries
                    .get_mut(&owner.handle.sequence)
                    .unwrap()
                    .revision = 0;
                vm.set_domain_limits(Default::default()).unwrap();
                read(&mut vm, &owner, 7);
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            }
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_candidate_and_receipt_partial_adoption_refunds_storage() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            for limit in [0, std::mem::size_of::<Stored<Option<Candidate>>>()] {
                let next = candidate(&vm, &owner, 99);
                let budget = Rc::new(Budget {
                    limit: Cell::new(limit),
                    ..Default::default()
                });
                let mut arena = Arena::with_budget(budget.clone(), None);
                assert!(adopt(&mut arena, Some(next)).is_err());
                assert_eq!(budget.bytes.get(), 0);
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            }
            for limit in [0, std::mem::size_of::<Stored<Receipt>>()] {
                let next = candidate(&vm, &owner, 99);
                let receipt = admit(&vm.domains, token(&vm, &owner), next);
                let budget = Rc::new(Budget {
                    limit: Cell::new(limit),
                    ..Default::default()
                });
                let mut arena = Arena::with_budget(budget.clone(), None);
                assert!(adopt(&mut arena, receipt).is_err());
                assert_eq!(budget.bytes.get(), 0);
                assert_eq!(vm.domains.borrow().usage().1, 0);
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            }
            read(&mut vm, &owner, 7);
        }
    }
    #[test]
    fn state_domains_candidates_pin_code_and_pending_commits_observe_domain_failure() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let next = candidate(&vm, &owner, 12);
            vm.load_source(&SOURCE.replace("scale: 0.5", "scale: 0.75"))
                .unwrap();
            assert!(construct(
                &vm.domains,
                vm.current.clone().unwrap(),
                owner.read_handle(),
                0,
                "make",
                InstanceValue::Int32(99),
                InterpreterLimits::default()
            )
            .is_err());
            let receipt = admit(&vm.domains, token(&vm, &owner), next);
            vm.poll_tasks(100).unwrap();
            assert_eq!(receipt.result(), Some((Status::Committed, 1)));
            let mut scale = vm
                .queue_domain_call(owner.read_handle(), "scale", &[])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(
                matches!(scale.take_result(), Some(DomainOutcome::Value(InstanceValue::Float64(n))) if n == 0.5)
            );
            drop(receipt);
            owner.close();

            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let next = candidate(&vm, &owner, 99);
            let mut failure = vm
                .queue_domain_call(
                    owner.write_handle(),
                    "fail_write",
                    &[InstanceValue::Int32(0)],
                )
                .unwrap();
            let receipt = admit(&vm.domains, token(&vm, &owner), next);
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                failure.take_result(),
                Some(DomainOutcome::Failed(_))
            ));
            assert_eq!(receipt.result(), Some((Status::DomainFailed, 0)));
            assert_eq!(vm.domains.borrow().budget.bytes.get(), 0);
            drop(receipt);
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));

            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let next = candidate(&vm, &owner, 99);
            let mut foreign = VmEngine::with_mode(mode);
            foreign.load_source(SOURCE).unwrap();
            let target = foreign
                .create_domain("make", &[InstanceValue::Int32(100)])
                .unwrap();
            let receipt = admit(&foreign.domains, token(&foreign, &target), next);
            assert_eq!(receipt.result(), Some((Status::InvalidAccess, 0)));
            read(&mut vm, &owner, 7);
            read(&mut foreign, &target, 100);
        }
    }
    struct CommitClock {
        time: Cell<Instant>,
        next: Cell<Option<Instant>>,
        skip: Cell<usize>,
        hook: RefCell<Option<Box<dyn FnOnce()>>>,
    }
    impl CommitClock {
        fn new() -> Rc<Self> {
            Rc::new(Self {
                time: Cell::new(Instant::now()),
                next: Cell::new(None),
                skip: Cell::new(0),
                hook: RefCell::new(None),
            })
        }
    }
    impl crate::runtime::executor::Clock for CommitClock {
        fn now(&self) -> Instant {
            let now = self.time.get();
            if let Some(next) = self.next.take() {
                self.time.set(next);
            }
            if self.skip.get() > 0 {
                self.skip.set(self.skip.get() - 1);
            } else {
                let hook = self.hook.borrow_mut().take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            now
        }
    }
    #[test]
    fn state_domains_deadline_partial_adoption_refunds_storage() {
        let vm = VmEngine::new();
        let deadline = deadline_after(&vm.domains, 10).unwrap();
        let live = crate::runtime::arena::LIVE.with(|n| n.get());
        for limit in [0, std::mem::size_of::<Stored<Deadline>>()] {
            let budget = Rc::new(Budget {
                limit: Cell::new(limit),
                ..Default::default()
            });
            let mut arena = Arena::with_budget(budget.clone(), None);
            assert!(adopt(&mut arena, deadline).is_err());
            assert_eq!(budget.bytes.get(), 0);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_deadlines_check_fresh_clock_and_preserve_receipts() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let clock = CommitClock::new();
            vm.configure_tasks(clock.clone(), 64).unwrap();
            vm.load_source(OWNED).unwrap();
            for offset in [-1i64, 0, 1] {
                let owner = vm
                    .create_domain("make", &[InstanceValue::Int32(7)])
                    .unwrap();
                let baseline = vm.domains.borrow().budget.bytes.get();
                let deadline = deadline_after(&vm.domains, 10).unwrap();
                let mut next = candidate(&vm, &owner, 12);
                next.deadline = Some(deadline);
                let receipt = admit(&vm.domains, token(&vm, &owner), next);
                // The executor's timer scan sees a time before expiry. Only the
                // fresh publication read observes this boundary, with no timer.
                clock.time.set(deadline.at - Duration::from_millis(1));
                clock.next.set(Some(if offset < 0 {
                    deadline.at - Duration::from_nanos(1)
                } else {
                    deadline.at + Duration::from_nanos(offset as u64)
                }));
                vm.poll_tasks(100).unwrap();
                let expected = if offset < 0 {
                    (Status::Committed, 1)
                } else {
                    (Status::Expired, 0)
                };
                assert_eq!(receipt.result(), Some(expected), "{mode:?}: {offset}");
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
                assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), expected.1);
                read(&mut vm, &owner, if offset < 0 { 12 } else { 7 });
                clock.time.set(deadline.at + Duration::from_secs(1));
                receipt.request.as_ref().unwrap().cancel();
                owner.close();
                assert_eq!(receipt.result(), Some(expected));
                drop(receipt);
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
    #[test]
    fn state_domains_deadlines_bind_executor_and_validate_after_clock_reentry() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let clock = CommitClock::new();
            vm.configure_tasks(clock.clone(), 64).unwrap();
            vm.load_source(OWNED).unwrap();
            assert!(deadline_after(&vm.domains, -1).is_err());
            let foreign = VmEngine::with_mode(mode);
            let foreign_deadline = deadline_after(&foreign.domains, 10).unwrap();
            for action in 0..4 {
                let owner = vm
                    .create_domain("make", &[InstanceValue::Int32(7)])
                    .unwrap();
                let mut next = candidate(&vm, &owner, 99);
                next.deadline = Some(if action == 0 {
                    foreign_deadline
                } else {
                    deadline_after(&vm.domains, 0).unwrap()
                });
                let receipt = admit(&vm.domains, token(&vm, &owner), next);
                if action == 0 {
                    assert_eq!(receipt.result(), Some((Status::InvalidAccess, 0)));
                    read(&mut vm, &owner, 7);
                    owner.close();
                } else if action == 1 {
                    receipt.request.as_ref().unwrap().cancel();
                    vm.poll_tasks(100).unwrap();
                    assert_eq!(receipt.result(), Some((Status::Cancelled, 0)));
                    read(&mut vm, &owner, 7);
                    owner.close();
                } else if action == 2 {
                    owner.close();
                    vm.poll_tasks(100).unwrap();
                    assert_eq!(receipt.result(), Some((Status::Closed, 0)));
                } else {
                    // Close at the final clock read, after the timer scan. The
                    // clock callback must run outside the registry borrow.
                    clock.skip.set(1);
                    *clock.hook.borrow_mut() = Some(Box::new(move || {
                        owner.close();
                    }));
                    vm.poll_tasks(100).unwrap();
                    assert!(clock.hook.borrow().is_none());
                    assert_eq!(receipt.result(), Some((Status::Closed, 0)));
                }
                drop(receipt);
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
    const OWNED: &str = include_str!("../../../tests/fixtures/state_domain_owned.jk");
    #[test]
    fn state_domains_owned_calls_and_publications_keep_only_live_graphs() {
        owned_calls(OWNED);
    }
    #[test]
    fn state_domains_nested_calls_and_publications_keep_only_live_graphs() {
        owned_calls(NESTED);
    }
    fn owned_calls(source: &str) {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            let live = crate::runtime::arena::LIVE.with(|n| n.get());
            for round in 0..32 {
                let mut inspection = vm
                    .queue_domain_call(owner.read_handle(), "inspect", &[])
                    .unwrap();
                vm.poll_tasks(100).unwrap();
                assert!(matches!(
                    inspection.take_result(),
                    Some(DomainOutcome::Value(InstanceValue::Bool(true)))
                ));
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
                assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
                let next = candidate(&vm, &owner, round);
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline * 2);
                let receipt = admit(&vm.domains, token(&vm, &owner), next);
                vm.poll_tasks(100).unwrap();
                assert_eq!(
                    receipt.result(),
                    Some((Status::Committed, round as u64 + 1))
                );
                drop(receipt);
                read(&mut vm, &owner, round);
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
                assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
            }
            // Allocation failure in a read releases its temporary owners and keeps the domain usable.
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: baseline,
                ..Default::default()
            })
            .unwrap();
            let mut query = vm
                .queue_domain_call(owner.read_handle(), "inspect", &[])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(
                matches!(query.take_result(), Some(DomainOutcome::Failed(e)) if e.code() == ErrorCode::ResourceLimit)
            );
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 32);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            vm.set_domain_limits(Default::default()).unwrap();
            read(&mut vm, &owner, 31);
            // A failed mutable turn disposes the entire owning root and isolates the domain.
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: baseline,
                ..Default::default()
            })
            .unwrap();
            let mut update = vm
                .queue_domain_call(owner.write_handle(), "change", &[InstanceValue::Int32(99)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                update.take_result(),
                Some(DomainOutcome::Failed(_))
            ));
            assert_eq!(vm.domains.borrow().budget.bytes.get(), 0);
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    fn owned_candidate_budgets(source: &str, limits: impl Clone + IntoIterator<Item = usize>) {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            let live = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut succeeded = false;
            for allowance in limits.clone() {
                vm.set_domain_limits(DomainLimits {
                    max_state_bytes: baseline + allowance,
                    ..Default::default()
                })
                .unwrap();
                let result = construct(
                    &vm.domains,
                    vm.current.clone().unwrap(),
                    owner.read_handle(),
                    0,
                    "make",
                    InstanceValue::Int32(12),
                    InterpreterLimits::default(),
                );
                match result {
                    Ok(candidate) => {
                        drop(candidate);
                        succeeded = true;
                    }
                    Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
                }
                assert_eq!(
                    vm.domains.borrow().budget.bytes.get(),
                    baseline,
                    "{mode:?}: {allowance}"
                );
                assert_eq!(
                    crate::runtime::arena::LIVE.with(|n| n.get()),
                    live,
                    "{mode:?}: {allowance}"
                );
                assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
                if succeeded {
                    break;
                }
            }
            assert!(succeeded);
            vm.set_domain_limits(Default::default()).unwrap();
            read(&mut vm, &owner, 7);
        }
    }
    #[test]
    fn state_domains_candidate_values_failures_leave_live_root_usable() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&format!("{OWNED} struct Input {{n:Int32,zero:Int32}} fn from_value(p:Input)->State{{make(p.n)}} fn broken_value(p:Input)->State{{let temporary=make(p.n) make(1/p.zero)}}")).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            let live = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut succeeded = false;
            for allowance in [0, 1, 64, 128, 256, 512, 1024, 2048, 4096] {
                vm.set_domain_limits(DomainLimits {
                    max_state_bytes: baseline + allowance,
                    ..Default::default()
                })
                .unwrap();
                let result = construct(
                    &vm.domains,
                    vm.current.clone().unwrap(),
                    owner.read_handle(),
                    0,
                    "from_value",
                    ConstructorInput::Values(vec![24, 0]),
                    InterpreterLimits::default(),
                );
                match result {
                    Ok(candidate) => {
                        succeeded = true;
                        drop(candidate);
                    }
                    Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
                }
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
                assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
                assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
            }
            assert!(succeeded);
            vm.set_domain_limits(Default::default()).unwrap();
            let error = construct(
                &vm.domains,
                vm.current.clone().unwrap(),
                owner.read_handle(),
                0,
                "broken_value",
                ConstructorInput::Values(vec![24, 0]),
                InterpreterLimits::default(),
            )
            .err()
            .unwrap();
            assert!(error.message().contains("Division by zero"));
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
            read(&mut vm, &owner, 7);
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }

    #[test]
    fn state_domains_owned_candidate_budget_boundaries_preserve_old_state() {
        owned_candidate_budgets(OWNED, [0, 1, 64, 128, 256, 512, 768, 1024, 2048, 4096]);
    }
    #[test]
    #[ignore = "exhaustive: every byte budget through successful String/Array candidate construction"]
    fn exhaustive_state_domains_owned_candidate_allocation_budgets() {
        owned_candidate_budgets(OWNED, 0..=4096);
    }
    const NESTED: &str = include_str!("../../../tests/fixtures/state_domain_nested.jk");
    #[test]
    fn state_domains_nested_candidate_budget_boundaries_preserve_old_state() {
        owned_candidate_budgets(
            NESTED,
            [0, 1, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384],
        );
    }
    #[test]
    #[ignore = "exhaustive: every byte budget through successful nested Array/Map candidate construction"]
    fn exhaustive_state_domains_nested_candidate_allocation_budgets() {
        owned_candidate_budgets(NESTED, 0..=16384);
    }
    #[test]
    fn state_domains_nested_code_publication_preserves_graph_and_checks_schema() {
        // Keep Player only behind buffers so schema checks cannot succeed by
        // inspecting an inline copy of the same type.
        let source = NESTED
            .replace("leader: Player", "leader: Int32")
            .replace("leader: player(value)", "leader: value")
            .replace(
                "valid(&state.leader, state.value)",
                "state.leader == state.value",
            )
            .replace("var leader = player(value)", "var leader = value")
            .replace(
                "mem.swap(&mut state.leader, &mut leader)",
                "state.leader = leader",
            );
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            let stale = candidate(&vm, &owner, 12);
            vm.load_source(
                &source.replace("-> Int32 { state.value }", "-> Int32 { state.value + 100 }"),
            )
            .unwrap();
            assert_eq!(vm.publish_domain_code().unwrap(), 1);
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
            let receipt = admit(&vm.domains, token(&vm, &owner), stale);
            vm.poll_tasks(100).unwrap();
            assert_eq!(receipt.result(), Some((Status::Conflict, 0)));
            drop(receipt);
            read(&mut vm, &owner, 107);
            let mut inspection = vm
                .queue_domain_call(owner.read_handle(), "inspect", &[])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                inspection.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Bool(true)))
            ));
            drop(inspection);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            // Same-size changes behind buffers are schema changes too.
            for changed in [
                source.replace("score", "rank"),
                source.replace("Ready", "Active"),
            ] {
                vm.load_source(&changed).unwrap();
                assert_eq!(
                    vm.publish_domain_code().unwrap_err().code(),
                    ErrorCode::TypeMismatch
                );
                read(&mut vm, &owner, 107);
            }
            vm.load_source(&source).unwrap();
            assert_eq!(vm.publish_domain_code().unwrap(), 1);
            let mut update = vm
                .queue_domain_call(owner.write_handle(), "edit", &[InstanceValue::Int32(19)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                update.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(19)))
            ));
            drop(update);
            read(&mut vm, &owner, 19);
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_nested_failed_write_releases_changed_graph() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&format!("{NESTED}\nfn broken(state: &mut State, zero: Int32) -> Int32 {{ edit(state, 19); 1 / zero }}")).unwrap();
            let before = crate::runtime::arena::LIVE.with(|n| n.get());
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let other = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get() / 2;
            let mut request = vm
                .queue_domain_call(owner.write_handle(), "broken", &[InstanceValue::Int32(0)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                request.take_result(),
                Some(DomainOutcome::Failed(_))
            ));
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            read(&mut vm, &other, 7);
            drop(request);
            owner.close();
            other.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), before);
        }
    }
    fn joint_vm(mode: VmMode) -> (VmEngine, DomainOwner, DomainOwner) {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&format!(
            "{SOURCE}\nfn total(a: &State, b: &State) -> Int32 {{ a.count + b.count }}"
        ))
        .unwrap();
        let a = vm
            .create_domain("make", &[InstanceValue::Int32(7)])
            .unwrap();
        let b = vm
            .create_domain("make", &[InstanceValue::Int32(10)])
            .unwrap();
        (vm, a, b)
    }
    fn joint_candidate(vm: &mut VmEngine, a: &DomainOwner, b: &DomainOwner) -> Candidate {
        let mut snapshot = vm
            .queue_domain_pair([a.read_handle(), b.read_handle()], "total", &[])
            .unwrap();
        vm.poll_tasks(100).unwrap();
        let (outcome, _, reads) = snapshot.take_snapshot_result().unwrap();
        assert!(matches!(
            outcome,
            DomainOutcome::Value(InstanceValue::Int32(_))
        ));
        let mut next = candidate(vm, a, 99);
        next.reads = reads.unwrap();
        next
    }
    #[test]
    fn state_domains_joint_snapshot_captures_all_revisions_before_collection() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, a, b) = joint_vm(mode);
            let mut snapshot = vm
                .queue_domain_pair([a.read_handle(), b.read_handle()], "total", &[])
                .unwrap();
            let mut write = vm
                .queue_domain_call(b.write_handle(), "add", &[InstanceValue::Int32(1)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            let (outcome, _, reads) = snapshot.take_snapshot_result().unwrap();
            assert!(matches!(
                outcome,
                DomainOutcome::Value(InstanceValue::Int32(17))
            ));
            let reads = reads.unwrap();
            assert_eq!(reads.validate(&vm.domains.borrow()), Err(Status::Conflict));
            let mut next = candidate(&vm, &a, 99);
            next.reads = reads;
            let receipt = admit(&vm.domains, token(&vm, &a), next);
            vm.poll_tasks(100).unwrap();
            assert_eq!(receipt.result(), Some((Status::Conflict, 0)));
            read(&mut vm, &a, 7);
            write.take_result();
            drop(receipt);
            a.close();
            b.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_candidate_versions_conflict_for_targets_and_readonly_dependencies() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for target in [false, true] {
                for version in ["data", "code", "schema"] {
                    let mut vm = VmEngine::with_mode(mode);
                    vm.load_source(&format!("{SOURCE} struct Input {{n:Int32}} fn from_value(p:Input)->State{{make(p.n)}} fn total(a:&State,b:&State)->Int32{{a.count+b.count}}" )).unwrap();
                    let a = vm
                        .create_domain("make", &[InstanceValue::Int32(7)])
                        .unwrap();
                    let b = vm
                        .create_domain("make", &[InstanceValue::Int32(10)])
                        .unwrap();
                    let original = joint_candidate(&mut vm, &a, &b);
                    let mut next = construct(
                        &vm.domains,
                        vm.current.clone().unwrap(),
                        a.read_handle(),
                        0,
                        "from_value",
                        ConstructorInput::Values(vec![99]),
                        InterpreterLimits::default(),
                    )
                    .unwrap();
                    next.reads = original.reads.clone();
                    drop(original);
                    let receipt = admit(&vm.domains, token(&vm, &a), next);
                    {
                        // Independently exercise each credential without changing
                        // the root: publication/migration have their own tests.
                        let mut domains = vm.domains.borrow_mut();
                        let domain = domains
                            .entries
                            .get_mut(&if target {
                                a.handle.sequence
                            } else {
                                b.handle.sequence
                            })
                            .unwrap();
                        match version {
                            "data" => domain.revision += 1,
                            "code" => domain.code += 1,
                            _ => domain.schema += 1,
                        }
                    }
                    vm.poll_tasks(100).unwrap();
                    assert_eq!(
                        receipt.result(),
                        Some((Status::Conflict, 0)),
                        "{mode:?}, target={target}, {version}"
                    );
                    read(&mut vm, &a, 7);
                    a.close();
                    b.close();
                    assert_eq!(receipt.result(), Some((Status::Conflict, 0)));
                    drop(receipt);
                    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                }
            }
        }
    }

    #[test]
    fn state_domains_joint_snapshot_complete_dependencies_control_pending_commits() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            // The read-only participant changes, closes, fails, or belongs to another executor.
            for action in 0..7 {
                let (mut vm, a, b) = joint_vm(mode);
                let baseline = vm.domains.borrow().budget.bytes.get();
                let mut next = joint_candidate(&mut vm, &a, &b);
                if action == 6 {
                    let (other, x, y) = joint_vm(mode);
                    next.reads = reads::ReadSet::capture(
                        &other.domains.borrow(),
                        [x.read_handle(), y.read_handle()],
                    );
                }
                let mut write = if action == 0 || action == 2 {
                    Some(
                        vm.queue_domain_call(
                            b.write_handle(),
                            if action == 0 { "add" } else { "fail_write" },
                            &[InstanceValue::Int32(0)],
                        )
                        .unwrap(),
                    )
                } else {
                    None
                };
                let receipt = admit(&vm.domains, token(&vm, &a), next);
                let status = match action {
                    0 => Status::Conflict,
                    1 => {
                        b.close();
                        Status::Closed
                    }
                    2 => Status::DomainFailed,
                    3 => {
                        receipt.request.as_ref().unwrap().cancel();
                        Status::Cancelled
                    }
                    4 => {
                        drop(receipt);
                        vm.poll_tasks(100).unwrap();
                        read(&mut vm, &a, 7);
                        assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
                        a.close();
                        b.close();
                        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                        continue;
                    }
                    5 => Status::Committed,
                    _ => Status::InvalidAccess,
                };
                vm.poll_tasks(100).unwrap();
                assert_eq!(receipt.result(), Some((status, u64::from(action == 5))));
                read(&mut vm, &a, if action == 5 { 99 } else { 7 });
                if let Some(write) = &mut write {
                    write.take_result();
                }
                if action == 5 {
                    b.close();
                    assert_eq!(receipt.result(), Some((Status::Committed, 1)));
                }
                drop(receipt);
                a.close();
                b.close();
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
}
