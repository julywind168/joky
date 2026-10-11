//! Independently owned scalar candidates and bounded, retained commit receipts.
use super::*;
use crate::runtime::{
    arena::ExternalCharge, buffer::Buffer, domain::Operation, resource::OwnedResource,
};
use std::ffi::c_void;

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
}
pub(super) struct Candidate {
    root: Root,
    module: LoadedModule,
    ty: Type,
    base: DomainHandle,
    revision: u64,
}
struct Receipt {
    request: Option<DomainRequest>,
    rejected: Option<Status>,
}
struct Stored<T> {
    value: RefCell<T>,
    _charge: ExternalCharge,
}
unsafe extern "C" fn release<T>(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Stored<T>>()));
}
fn adopt<T>(arena: &mut Arena, value: T) -> Result<u64, Error> {
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
unsafe fn lease(bits: u64) -> Result<crate::runtime::resource::ResourceLease, Error> {
    (bits as *mut Buffer)
        .as_mut()
        .ok_or_else(|| invalid("Null domain resource"))?
        .resource_mut()?
        .lease()
}
fn construct(
    registry: &Rc<RefCell<Domains>>,
    module: LoadedModule,
    base: DomainHandle,
    revision: u64,
    name: &str,
    input: InstanceValue,
    limits: InterpreterLimits,
) -> Result<Candidate, Error> {
    let info = module.instance_info(name)?;
    let budget = {
        let domains = registry.borrow();
        let domain = domains.get(base)?;
        if !source::same_module(&module, &domain.module)
            || info.signature.ret != domain.ty
            || !info.domain_entry
            || !instance::constructor(&info)
        {
            return Err(invalid("Candidate requires a proved constructor for the snapshot's exact module and state type"));
        }
        domains.budget.clone()
    };
    let words = arguments(&info.signature.params, &[input])?;
    let mut arena = Arena::with_budget(budget, None);
    let layout = info
        .signature
        .ret
        .storage_layout()
        .map_err(|e| invalid(&e.to_string()))?;
    let pointer = arena.allocate(layout.size, layout.align)?;
    unsafe {
        pointer.as_ptr().write_bytes(0, layout.size.max(1));
    }
    // No state address is passed to user code. Failure drops only this arena.
    let result = module.invoke_instance(name, &words, limits, &mut arena)?;
    super::super::instances::initialize(pointer, &info.signature.ret, &result)?;
    Ok(Candidate {
        root: Root {
            _arena: arena,
            pointer,
        },
        module,
        ty: info.signature.ret,
        base,
        revision,
    })
}
fn validate(domains: &Domains, candidate: &Candidate) -> Result<u64, Status> {
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
    if domain.ty != candidate.ty || !source::same_module(&domain.module, &candidate.module) {
        return Err(Status::InvalidAccess);
    }
    if domain.revision != candidate.revision {
        return Err(Status::Conflict);
    }
    domain.revision.checked_add(1).ok_or(Status::ResourceLimit)
}
pub(super) fn execute(domains: &mut Domains, request: &mut Request) {
    let Work::Commit(candidate) = &mut request.work else {
        unreachable!()
    };
    let candidate = candidate.take().unwrap();
    let next = match validate(domains, &candidate) {
        Ok(next) => next,
        Err(status) => {
            request.commit_status = Some(status);
            request.finish(DomainOutcome::Value(InstanceValue::Unit));
            return;
        }
    };
    let domain = domains.entries.get_mut(&candidate.base.sequence).unwrap();
    // Publication: only ownership moves and scalar stores; no user code,
    // allocation, cancellation point or fallible operation in this interval.
    let retired = domain.root.replace(candidate.root);
    domain.revision = next;
    request.observed_revision = next;
    request.commit_status = Some(Status::Committed);
    request.outcome = Some(DomainOutcome::Value(InstanceValue::Unit));
    // Retired storage is released only after the receipt records the fact.
    drop(retired);
    request.finish(DomainOutcome::Value(InstanceValue::Unit));
}
fn admit(registry: &Rc<RefCell<Domains>>, token: u64, candidate: Candidate) -> Receipt {
    let access = {
        let domains = registry.borrow();
        if candidate.base.registry != domains.identity {
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
    };
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
    if op == Operation::Candidate {
        let snapshot_lease = lease(frame.slots[1])?;
        let snapshot = &*snapshot_lease.token().cast::<source::Snapshot>();
        let (module, _) = source::module(registry, frame.slots[0])?;
        let name = crate::runtime::owned_text::borrow(frame.slots[2] as *const Buffer)?;
        let input = source::scalar_argument(
            &module.instance_info(name)?.signature.params,
            frame.slots[3],
        )?;
        let candidate = construct(
            registry,
            module,
            snapshot.handle,
            snapshot.revision,
            name,
            input,
            crate::runtime::suspension::current_limits(),
        )?;
        return adopt(arena, Some(candidate)).map(Some);
    }
    if op == Operation::Commit {
        let candidate_lease = lease(frame.slots[1])?;
        let stored = &*candidate_lease.token().cast::<Stored<Option<Candidate>>>();
        let candidate = stored
            .value
            .borrow_mut()
            .take()
            .ok_or_else(|| invalid("Candidate was already consumed"))?;
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
                    next.revision = u64::MAX;
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
}
