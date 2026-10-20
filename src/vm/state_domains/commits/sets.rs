//! Bounded heterogeneous snapshots and atomic candidate sets on the shared FIFO.
use super::*;
const MAX_MEMBERS: usize = 64;

pub(crate) struct Accesses {
    registry: u64,
    handles: Vec<DomainHandle>,
    _charge: reads::Charge,
}
impl Accesses {
    pub(crate) fn touches(&self, domain: u64) -> bool {
        self.handles.iter().any(|h| h.sequence == domain)
    }
}
struct Frozen {
    token: u64,
    module: LoadedModule,
    module_key: usize,
    ty: Type,
    root: Option<Root>,
}
pub(crate) struct Snapshot {
    reads: reads::ReadSet,
    roots: Vec<Frozen>,
    _charge: reads::Charge,
}
pub(crate) struct Proposal {
    outbox: Option<Box<outbox::Batch>>,
    frozen: Option<Snapshot>,
    reads: reads::ReadSet,
    candidates: Vec<Candidate>,
    tokens: Vec<u64>,
    deadline: Option<Deadline>,
    _charge: reads::Charge,
}
impl Proposal {
    pub(super) fn attach_outbox(&mut self, batch: &mut Option<outbox::Batch>) -> Result<(), Error> {
        attach(&mut self.outbox, batch)
    }
    pub(crate) fn touches(&self, domain: u64) -> bool {
        self.reads.touches(domain)
    }
    pub(crate) fn timed(&self) -> bool {
        self.deadline.is_some()
    }
}
fn vector<T>(capacity: usize) -> Result<Vec<T>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| limited("Domain set allocation failed"))?;
    Ok(values)
}
fn charge(domains: &Domains, bytes: usize) -> Result<reads::Charge, Error> {
    reads::Charge::new(domains.budget.clone(), bytes)
}
fn accesses(registry: &Rc<RefCell<Domains>>) -> Result<Accesses, Error> {
    let domains = registry.borrow();
    let handles = vector(MAX_MEMBERS)?;
    Ok(Accesses {
        registry: domains.identity,
        _charge: charge(
            &domains,
            handles.capacity() * std::mem::size_of::<DomainHandle>(),
        )?,
        handles,
    })
}
fn include(
    registry: &Rc<RefCell<Domains>>,
    set: &mut Accesses,
    handle: DomainHandle,
) -> Result<(), Error> {
    let domains = registry.borrow();
    if set.registry != domains.identity {
        return Err(invalid("Access set belongs to another executor"));
    }
    domains.get(handle)?;
    if set.touches(handle.sequence) {
        return Ok(());
    }
    if set.handles.len() == MAX_MEMBERS {
        return Err(limited("Domain access set member limit exceeded"));
    }
    set.handles.push(handle.read_only());
    Ok(())
}
fn submit_snapshot(registry: &Rc<RefCell<Domains>>, set: Accesses) -> Result<DomainRequest, Error> {
    {
        let domains = registry.borrow();
        if set.registry != domains.identity {
            return Err(invalid("Access set belongs to another executor"));
        }
        for handle in &set.handles {
            domains.get(*handle)?;
        }
    }
    let primary = set
        .handles
        .first()
        .ok_or_else(|| invalid("Empty domain access set"))?
        .sequence;
    let bytes = std::mem::size_of::<Accesses>()
        + set.handles.capacity() * std::mem::size_of::<DomainHandle>();
    enqueue(registry, primary, Work::SnapshotSet(Box::new(set)), bytes)
}
fn capture(domains: &Domains, set: &Accesses) -> Result<Snapshot, Error> {
    let reads = reads::ReadSet::capture_many(domains, &set.handles)?;
    let mut roots = vector(set.handles.len())?;
    let charge = charge(
        domains,
        std::mem::size_of::<Snapshot>() + roots.capacity() * std::mem::size_of::<Frozen>(),
    )?;
    for handle in &set.handles {
        let domain = domains.get(*handle)?;
        roots.push(Frozen {
            token: domain.source_token * 2,
            module: domain.module.clone(),
            module_key: domain.source_module,
            ty: domain.ty.clone(),
            root: Some(
                domain
                    .root
                    .as_ref()
                    .unwrap()
                    .snapshot(&domain.ty, domains.budget.clone())?,
            ),
        });
    }
    Ok(Snapshot {
        reads,
        roots,
        _charge: charge,
    })
}
pub(crate) fn execute_snapshot(domains: &mut Domains, request: &mut Request) {
    let Work::SnapshotSet(set) = &request.work else {
        unreachable!()
    };
    let result = capture(domains, set);
    let outcome = match result {
        Ok(snapshot) => {
            request.set_snapshot = Some(Box::new(snapshot));
            DomainOutcome::Value(InstanceValue::Unit)
        }
        Err(error) => DomainOutcome::Failed(error),
    };
    request.finish(outcome);
}
fn proposal(registry: &Rc<RefCell<Domains>>, reads: reads::ReadSet) -> Result<Proposal, Error> {
    let candidates = vector(MAX_MEMBERS)?;
    let tokens = vector(MAX_MEMBERS)?;
    let charge = charge(
        &registry.borrow(),
        candidates.capacity() * std::mem::size_of::<Candidate>()
            + tokens.capacity() * std::mem::size_of::<u64>(),
    )?;
    Ok(Proposal {
        outbox: None,
        frozen: None,
        reads,
        candidates,
        tokens,
        deadline: None,
        _charge: charge,
    })
}
fn add(
    registry: &Rc<RefCell<Domains>>,
    set: &mut Proposal,
    token: u64,
    module: LoadedModule,
    name: &str,
    input: impl Into<ConstructorInput>,
    limits: InterpreterLimits,
) -> Result<(), Error> {
    let handle = source::handle(registry, token)?;
    if !handle.writable {
        return Err(invalid("Candidate set requires write access"));
    }
    let version = set
        .reads
        .version(handle)
        .ok_or_else(|| invalid("Candidate target is absent from the complete read set"))?;
    if set
        .candidates
        .iter()
        .any(|c| c.base.sequence == handle.sequence)
    {
        return Err(invalid("Candidate set cannot repeat a writable domain"));
    }
    if set.candidates.len() == MAX_MEMBERS {
        return Err(limited("Candidate set member limit exceeded"));
    }
    let mut candidate = construct(
        registry,
        module,
        handle,
        version.revision,
        name,
        input,
        limits,
    )?;
    candidate.reads = set.reads.clone();
    set.candidates.push(candidate);
    set.tokens.push(token);
    Ok(())
}
/// # Safety
/// The packet must be a live compiler-packed UInt64 buffer on the current
/// task thread. Its initialized lanes are validated against the editor schema.
unsafe fn edit(
    registry: &Rc<RefCell<Domains>>,
    set: &mut Proposal,
    token: u64,
    module_key: usize,
    name: &str,
    packet: &Buffer,
    limits: InterpreterLimits,
) -> Result<(), Error> {
    let version = target_version(registry, &set.reads, token)?;
    let index = if let Some(index) = set
        .candidates
        .iter()
        .position(|c| c.base.sequence == version.handle.sequence)
    {
        index
    } else {
        let frozen = set
            .frozen
            .as_mut()
            .and_then(|s| s.roots.iter_mut().find(|f| f.token / 2 == token / 2))
            .ok_or_else(|| invalid("Candidate target has no retained frozen state"))?;
        if frozen.module_key != module_key {
            return Err(invalid(
                "Candidate editor belongs to another module version",
            ));
        }
        let root = frozen
            .root
            .take()
            .ok_or_else(|| invalid("Frozen root was consumed"))?;
        set.candidates.push(Candidate {
            outbox: None,
            root,
            module: frozen.module.clone(),
            ty: frozen.ty.clone(),
            base: DomainHandle {
                writable: true,
                ..version.handle
            },
            reads: set.reads.clone(),
            deadline: None,
        });
        set.tokens.push(token);
        set.candidates.len() - 1
    };
    let frozen = set
        .frozen
        .as_ref()
        .and_then(|s| s.roots.iter().find(|f| f.token / 2 == token / 2))
        .ok_or_else(|| invalid("Candidate editor requires a frozen candidate"))?;
    if frozen.module_key != module_key {
        return Err(invalid(
            "Candidate editor belongs to another module version",
        ));
    }
    let candidate = &mut set.candidates[index];
    if !source::same_module(&candidate.module, &frozen.module) || candidate.ty != frozen.ty {
        return Err(invalid(
            "Candidate editor requires the frozen module and state type",
        ));
    }
    let info = candidate.module.instance_info(name)?;
    if !info.domain_entry
        || !crate::core::state_domain::value_eligible(&info)
        || info.signature.params.len() != 2
        || info.signature.ret != Type::Unit
        || info.signature.params[0]
            .reference_type()
            .is_none_or(|r| !r.mutable || r.target != candidate.ty)
    {
        return Err(invalid("Candidate editor requires a proved synchronous mutable entry with pure-value input and Unit result"));
    }
    let input = values::input_words(&info.signature.params[1..], packet)?;
    let mut words = Vec::new();
    words
        .try_reserve_exact(input.len() + 1)
        .map_err(|_| limited("Candidate input allocation failed"))?;
    words.push(0);
    words.extend(input);
    candidate.root.invoke_values(
        &candidate.module,
        &candidate.ty,
        name,
        &info,
        (&mut words, None),
        limits,
    )?;
    Ok(())
}
fn admit(registry: &Rc<RefCell<Domains>>, mut set: Proposal) -> Receipt {
    let accepted = (|| {
        let domains = registry.borrow();
        if set.candidates.is_empty() {
            return Err(Status::InvalidAccess);
        }
        for (candidate, token) in set.candidates.iter_mut().zip(&set.tokens) {
            candidate.deadline = set.deadline;
            access(&domains, *token, candidate)?;
        }
        Ok(())
    })();
    set.frozen = None;
    let result = accepted.and_then(|()| {
        let bytes = std::mem::size_of::<Proposal>()
            + set.candidates.capacity() * std::mem::size_of::<Candidate>()
            + set.tokens.capacity() * std::mem::size_of::<u64>()
            + set.candidates.len()
                * (std::mem::size_of::<Root>()
                    + std::mem::size_of::<(u64, u64)>()
                    + std::mem::size_of::<u64>());
        enqueue(
            registry,
            set.candidates[0].base.sequence,
            Work::CommitSet(Some(Box::new(set))),
            bytes,
        )
        .map_err(|_| Status::ResourceLimit)
    });
    match result {
        Ok(request) => Receipt {
            registry: registry.borrow().identity,
            request: Some(request),
            rejected: None,
        },
        Err(status) => Receipt {
            registry: registry.borrow().identity,
            request: None,
            rejected: Some(status),
        },
    }
}
pub(crate) fn execute_commit(domains: &mut Domains, request: &mut Request, now: Option<Instant>) {
    let Work::CommitSet(set) = &mut request.work else {
        unreachable!()
    };
    let mut set = *set.take().unwrap();
    let prepared = (|| {
        let mut next = vector(set.candidates.len()).map_err(|_| Status::ResourceLimit)?;
        let retired =
            vector::<Option<Root>>(set.candidates.len()).map_err(|_| Status::ResourceLimit)?;
        let revisions =
            vector::<(u64, u64)>(set.candidates.len()).map_err(|_| Status::ResourceLimit)?;
        for candidate in &set.candidates {
            next.push(validate(domains, candidate, now)?);
        }
        if let Some(batch) = &mut set.outbox {
            batch.prepare(domains)?;
        }
        Ok::<_, Status>((next, retired, revisions))
    })();
    let (next, mut retired, mut revisions) = match prepared {
        Ok(prepared) => prepared,
        Err(status) => {
            request.commit_status = Some(status);
            request.finish(DomainOutcome::Value(InstanceValue::Unit));
            return;
        }
    };
    // All storage, checks and new revisions are ready. No fallible work or user
    // callback occurs until every root and the complete receipt are published.
    for ((candidate, token), revision) in set.candidates.drain(..).zip(&set.tokens).zip(next) {
        let domain = domains.entries.get_mut(&candidate.base.sequence).unwrap();
        retired.push(domain.root.replace(candidate.root));
        domain.revision = revision;
        revisions.push((*token / 2, revision));
    }
    if let Some(batch) = &mut set.outbox {
        batch.publish();
    }
    request.set_revisions = revisions;
    request.commit_status = Some(Status::Committed);
    request.outcome = Some(DomainOutcome::Value(InstanceValue::Unit));
    if let Some(batch) = &set.outbox {
        batch.wake();
    }
    drop(retired);
    request.finish(DomainOutcome::Value(InstanceValue::Unit));
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let request = (*frame).host_context.cast::<DomainRequest>();
    (*frame).host_context = std::ptr::null_mut();
    (*frame).host_stop = None;
    if !request.is_null() {
        drop(Box::from_raw(request));
    }
}
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<Option<u64>, Error> {
    let result = poll_inner(arena, frame, registry, op);
    if result.is_err() && op == Operation::SnapshotSet {
        stop(frame, 0);
    }
    result
}
unsafe fn poll_inner(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<Option<u64>, Error> {
    if op == Operation::AccessSet {
        return adopt(arena, Some(accesses(registry)?)).map(Some);
    }
    if op == Operation::AccessInclude {
        let lease = lease(frame.slots[0])?;
        let stored = &*lease.token().cast::<Stored<Option<Accesses>>>();
        let handle = source::handle(registry, frame.slots[1])?;
        include(
            registry,
            stored
                .value
                .borrow_mut()
                .as_mut()
                .ok_or_else(|| invalid("Access set was consumed"))?,
            handle,
        )?;
        return Ok(Some(0));
    }
    if op == Operation::SnapshotSet {
        if frame.state == 0 {
            let lease = lease(frame.slots[0])?;
            let stored = &*lease.token().cast::<Stored<Option<Accesses>>>();
            let set = stored
                .value
                .borrow_mut()
                .take()
                .ok_or_else(|| invalid("Access set was consumed"))?;
            let request = submit_snapshot(registry, set)?;
            request.register_current_wait()?;
            frame.host_context = Box::into_raw(Box::new(request)).cast();
            frame.host_stop = Some(stop);
            frame.state = 1;
            frame.wake = None;
            return Ok(None);
        }
        if frame.state != 1 || frame.host_context.is_null() {
            return Err(invalid("Invalid access set wait state"));
        }
        let request = &mut *frame.host_context.cast::<DomainRequest>();
        let sequence = request
            .sequence
            .ok_or_else(|| invalid("Snapshot set result was collected"))?;
        let record = {
            let mut domains = registry.borrow_mut();
            if domains
                .requests
                .get(&sequence)
                .is_none_or(|r| r.outcome.is_none())
            {
                drop(domains);
                request.register_current_wait()?;
                return Ok(None);
            }
            domains.remove_request(sequence).unwrap()
        };
        request.sequence = None;
        stop(frame, 0);
        return match record.outcome.unwrap() {
            DomainOutcome::Value(_) => adopt(
                arena,
                Some(
                    *record
                        .set_snapshot
                        .ok_or_else(|| invalid("Missing complete snapshot set"))?,
                ),
            )
            .map(Some),
            DomainOutcome::Failed(error) => Err(error),
            _ => Err(invalid("Snapshot set was cancelled or closed")),
        };
    }
    if matches!(
        op,
        Operation::SetRead | Operation::SetReadValues | Operation::SetCheck
    ) {
        let bits = frame.slots[usize::from(op != Operation::SetCheck)];
        let lease = lease(bits)?;
        let stored = &*lease.token().cast::<Stored<Option<Snapshot>>>();
        let mut snapshot = stored.value.borrow_mut();
        let snapshot = snapshot
            .as_mut()
            .ok_or_else(|| invalid("Snapshot set was consumed"))?;
        if snapshot.reads.first.handle.registry != registry.borrow().identity {
            return Err(invalid("Snapshot set belongs to another executor"));
        }
        if op == Operation::SetCheck {
            return Ok(Some(match snapshot.reads.validate(&registry.borrow()) {
                Ok(()) => 0,
                Err(Status::Conflict) => 1,
                Err(Status::Closed) => 2,
                Err(Status::DomainFailed) => 3,
                Err(_) => 4,
            }));
        }
        let (_, key) = source::module(registry, frame.slots[0])?;
        let frozen = snapshot
            .roots
            .iter_mut()
            .find(|f| f.token / 2 == frame.slots[2] / 2)
            .ok_or_else(|| invalid("Projection domain is absent from the snapshot set"))?;
        if frozen.module_key != key {
            return Err(invalid(
                "Snapshot projection belongs to another module version",
            ));
        }
        let name = crate::runtime::owned_text::borrow(frame.slots[3] as *const Buffer)?;
        let info = frozen.module.instance_info(name)?;
        if op == Operation::SetReadValues {
            if !info.domain_entry
                || !crate::core::state_domain::value_eligible(&info)
                || info.signature.params.len() != 1
                || info.signature.params[0]
                    .reference_type()
                    .is_none_or(|r| r.mutable || r.target != frozen.ty)
            {
                return Err(invalid("Snapshot projection requires a proved shared entry with a bounded pure-value result for the exact state type"));
            }
            let mut input = [frozen.root.as_ref().unwrap().scalar_pointer()];
            let words = frozen.root.as_mut().unwrap().invoke_values(
                &frozen.module,
                &frozen.ty,
                name,
                &info,
                (&mut input, None),
                crate::runtime::suspension::current_limits(),
            )?;
            let words = values::result_words(&info.signature.ret, words)?;
            values::write_packet(arena, &mut *(frame.slots[4] as *mut Buffer), &words)?;
            return Ok(Some(0));
        }
        if !info.domain_entry || instance::receiver(&info) != Some(&frozen.ty) {
            return Err(invalid(
                "Snapshot projection requires a proved entry for the exact state type",
            ));
        }
        source::turn_arguments(&info, None, false)?;
        let mut words = [frozen.root.as_ref().unwrap().scalar_pointer()];
        return frozen
            .root
            .as_mut()
            .unwrap()
            .invoke(
                &frozen.module,
                &frozen.ty,
                name,
                &info,
                &mut words,
                crate::runtime::suspension::current_limits(),
            )
            .map(|v| Some(v.bits()));
    }
    if matches!(op, Operation::SetEditSnapshot | Operation::SetEditCandidate) {
        let lease = lease(frame.slots[1])?;
        let mut set = if op == Operation::SetEditSnapshot {
            let stored = &*lease.token().cast::<Stored<Option<Snapshot>>>();
            let snapshot = stored
                .value
                .borrow_mut()
                .take()
                .ok_or_else(|| invalid("Snapshot set was consumed"))?;
            let mut set = proposal(registry, snapshot.reads.clone())?;
            set.frozen = Some(snapshot);
            set
        } else {
            let stored = &*lease.token().cast::<Stored<Option<Proposal>>>();
            stored
                .value
                .borrow_mut()
                .take()
                .ok_or_else(|| invalid("Candidate set was consumed"))?
        };
        let (_, key) = source::module(registry, frame.slots[0])?;
        let name = crate::runtime::owned_text::borrow(frame.slots[3] as *const Buffer)?;
        let packet = (frame.slots[4] as *const Buffer)
            .as_ref()
            .ok_or_else(|| invalid("Missing candidate input packet"))?;
        edit(
            registry,
            &mut set,
            frame.slots[2],
            key,
            name,
            packet,
            crate::runtime::suspension::current_limits(),
        )?;
        return adopt(arena, Some(set)).map(Some);
    }
    if matches!(
        op,
        Operation::SetCandidate
            | Operation::SetAdd
            | Operation::SetCandidateValues
            | Operation::SetAddValues
    ) {
        let lease = lease(frame.slots[1])?;
        let mut set = if matches!(op, Operation::SetCandidate | Operation::SetCandidateValues) {
            let stored = &*lease.token().cast::<Stored<Option<Snapshot>>>();
            let snapshot = stored
                .value
                .borrow_mut()
                .take()
                .ok_or_else(|| invalid("Snapshot set was consumed"))?;
            proposal(registry, snapshot.reads.clone())?
        } else {
            let stored = &*lease.token().cast::<Stored<Option<Proposal>>>();
            stored
                .value
                .borrow_mut()
                .take()
                .ok_or_else(|| invalid("Candidate set was consumed"))?
        };
        let (module, _) = source::module(registry, frame.slots[0])?;
        let name = crate::runtime::owned_text::borrow(frame.slots[3] as *const Buffer)?;
        let input = constructor_input(&module, name, frame.slots[4], op)?;
        add(
            registry,
            &mut set,
            frame.slots[2],
            module,
            name,
            input,
            crate::runtime::suspension::current_limits(),
        )?;
        return adopt(arena, Some(set)).map(Some);
    }
    if matches!(op, Operation::CommitSet | Operation::CommitSetBefore) {
        let lease = lease(frame.slots[0])?;
        let stored = &*lease.token().cast::<Stored<Option<Proposal>>>();
        let mut set = stored
            .value
            .borrow_mut()
            .take()
            .ok_or_else(|| invalid("Candidate set was consumed"))?;
        if op == Operation::CommitSetBefore {
            let lease = super::lease(frame.slots[1])?;
            let stored = &*lease.token().cast::<Stored<Deadline>>();
            set.deadline = Some(*stored.value.borrow());
        }
        let bits = adopt(
            arena,
            Receipt {
                registry: registry.borrow().identity,
                request: None,
                rejected: None,
            },
        )?;
        let lease = super::lease(bits)?;
        let stored = &*lease.token().cast::<Stored<Receipt>>();
        *stored.value.borrow_mut() = admit(registry, set);
        return Ok(Some(bits));
    }
    let lease = lease(frame.slots[0])?;
    let stored = &*lease.token().cast::<Stored<Receipt>>();
    let receipt = stored.value.borrow();
    if let Some(request) = &receipt.request {
        if request
            .domains
            .upgrade()
            .is_some_and(|d| !Rc::ptr_eq(&d, registry))
        {
            return Err(invalid("Commit receipt belongs to another executor"));
        }
        if let Some(sequence) = request.sequence {
            if let Some(record) = registry.borrow().requests.get(&sequence) {
                if record.commit_status == Some(Status::Committed) {
                    return Ok(Some(
                        record
                            .set_revisions
                            .iter()
                            .find(|(token, _)| *token == frame.slots[1] / 2)
                            .map_or(0, |(_, revision)| *revision),
                    ));
                }
            }
        }
    }
    Ok(Some(0))
}

impl Proposal {
    pub(crate) fn holds_module(&self, key: usize) -> bool {
        self.candidates.iter().any(|c| c.holds_module(key))
            || self.frozen.as_ref().is_some_and(|s| s.holds_module(key))
    }
}

impl Snapshot {
    pub(crate) fn holds_module(&self, key: usize) -> bool {
        self.roots
            .iter()
            .any(|r| code::module_key(&r.module) == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{state_domain_tests::SOURCE, VmMode};
    fn setup(mode: VmMode, n: usize) -> (VmEngine, Vec<DomainOwner>) {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&format!("{SOURCE}\nfn sum4(a:&State,b:&State,c:&State,d:&State)->Int32{{a.count+b.count+c.count+d.count}}\nfn write3(a:&mut State,b:&mut State,c:&mut State,d:&State,n:Int32)->Int32{{a.count=n b.count=n+1 c.count=n+2 d.count}}\nfn fail3(a:&mut State,b:&mut State,c:&mut State,d:&State,n:Int32)->Int32{{a.count=99 b.count=99 c.count=99 d.count/n}}\nfn broken_make(n:Int32)->State{{make(1/n)}}")).unwrap_or_else(|e|panic!("{e}"));
        let owners = (0..n)
            .map(|i| {
                vm.create_domain("make", &[InstanceValue::Int32(i as i32 + 1)])
                    .unwrap()
            })
            .collect();
        (vm, owners)
    }
    fn token(vm: &VmEngine, owner: &DomainOwner) -> u64 {
        vm.domains.borrow().entries[&owner.handle.sequence].source_token * 2 + 1
    }
    fn snapshot(vm: &mut VmEngine, owners: &[DomainOwner]) -> Snapshot {
        let mut set = accesses(&vm.domains).unwrap();
        for owner in owners.iter().rev() {
            include(&vm.domains, &mut set, owner.read_handle()).unwrap();
        }
        include(&vm.domains, &mut set, owners[0].read_handle()).unwrap();
        let mut request = submit_snapshot(&vm.domains, set).unwrap();
        vm.poll_tasks(100).unwrap();
        let mut record = vm
            .domains
            .borrow_mut()
            .remove_request(request.sequence.take().unwrap())
            .unwrap();
        assert!(matches!(record.outcome, Some(DomainOutcome::Value(_))));
        *record.set_snapshot.take().unwrap()
    }
    fn candidates(vm: &mut VmEngine, owners: &[DomainOwner]) -> Proposal {
        let snapshot = snapshot(vm, owners);
        let mut set = proposal(&vm.domains, snapshot.reads.clone()).unwrap();
        drop(snapshot);
        for (i, owner) in owners.iter().take(3).enumerate() {
            add(
                &vm.domains,
                &mut set,
                token(vm, owner),
                vm.current.clone().unwrap(),
                "make",
                InstanceValue::Int32(20 + i as i32),
                Default::default(),
            )
            .unwrap();
        }
        set
    }
    fn read(vm: &mut VmEngine, owner: &DomainOwner, expected: i32) {
        let mut request = vm
            .queue_domain_call(owner.read_handle(), "read", &[])
            .unwrap();
        vm.poll_tasks(100).unwrap();
        assert!(
            matches!(request.take_result(),Some(DomainOutcome::Value(InstanceValue::Int32(n))) if n==expected)
        );
    }
    #[test]
    fn state_domains_storage_frozen_edits_pin_code_and_move_roots_without_copying() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let source = include_str!("../../../../tests/fixtures/state_domain_storage.jk");
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
            let owners: Vec<_> = (1..=3)
                .map(|n| {
                    vm.create_domain("make", &[InstanceValue::Int32(n)])
                        .unwrap()
                })
                .collect();
            for domain in vm.domains.borrow_mut().entries.values_mut() {
                domain.source_module = code::module_key(&domain.module);
            }
            let snapshot = snapshot(&mut vm, &owners);
            let key = snapshot.roots[0].module_key;
            let original = snapshot
                .roots
                .iter()
                .find(|f| f.token / 2 == token(&vm, &owners[0]) / 2)
                .unwrap()
                .root
                .as_ref()
                .unwrap()
                .scalar_pointer();
            let mut set = proposal(&vm.domains, snapshot.reads.clone()).unwrap();
            set.frozen = Some(snapshot);
            let mut arena = Arena::default();
            let mut packet = Buffer::allocate(&mut arena, 2, 8, 8).unwrap();
            unsafe {
                for value in [7u64, 0] {
                    packet
                        .as_mut()
                        .initialize_next()
                        .unwrap()
                        .cast::<u64>()
                        .write(value);
                    packet.as_mut().commit_next();
                }
                edit(
                    &vm.domains,
                    &mut set,
                    token(&vm, &owners[0]),
                    key,
                    "update",
                    packet.as_ref(),
                    Default::default(),
                )
                .unwrap();
            }
            assert_eq!(set.candidates[0].root.scalar_pointer(), original);
            vm.load_source(
                &source.replace("edit(state,input.value)", "edit(state,input.value+100)"),
            )
            .unwrap();
            assert_eq!(vm.publish_domain_code().unwrap(), 3);
            let new_key = vm.domains.borrow().entries[&owners[0].handle.sequence].source_module;
            assert_ne!(key, new_key);
            unsafe {
                assert!(edit(
                    &vm.domains,
                    &mut set,
                    token(&vm, &owners[0]),
                    new_key,
                    "update",
                    packet.as_ref(),
                    Default::default()
                )
                .is_err());
                edit(
                    &vm.domains,
                    &mut set,
                    token(&vm, &owners[0]),
                    key,
                    "update",
                    packet.as_ref(),
                    Default::default(),
                )
                .unwrap();
            }
            let candidate = &mut set.candidates[0];
            let info = candidate.module.instance_info("read").unwrap();
            assert!(matches!(
                candidate
                    .root
                    .invoke(
                        &candidate.module,
                        &candidate.ty,
                        "read",
                        &info,
                        &mut [0],
                        Default::default()
                    )
                    .unwrap(),
                InstanceValue::Int32(7)
            ));
            let mut receipt = admit(&vm.domains, set);
            vm.poll_tasks(100).unwrap();
            let sequence = receipt.request.as_mut().unwrap().sequence.take().unwrap();
            let record = vm.domains.borrow_mut().remove_request(sequence).unwrap();
            assert_eq!(record.commit_status, Some(Status::Conflict));
            drop(record);
            drop(receipt);
            for (i, owner) in owners.iter().enumerate() {
                read(&mut vm, owner, i as i32 + 1);
            }
            unsafe {
                Buffer::release(&mut arena, packet.as_ptr()).unwrap();
            }
            for owner in owners {
                owner.close();
            }
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_storage_snapshot_sets_release_partial_copies_and_bound_members() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(
                "struct S{text:String,n:Int32} fn make(n:Int32)->S{S{text:\"frozen\",n:n}}",
            )
            .unwrap();
            let owners: Vec<_> = (0..65)
                .map(|n| {
                    vm.create_domain("make", &[InstanceValue::Int32(n)])
                        .unwrap()
                })
                .collect();
            for count in [1, 2, 3, 64] {
                let mut set = accesses(&vm.domains).unwrap();
                for owner in &owners[..count] {
                    include(&vm.domains, &mut set, owner.read_handle()).unwrap();
                }
                if count == 64 {
                    assert!(include(&vm.domains, &mut set, owners[64].read_handle()).is_err());
                }
                let domains = vm.domains.borrow();
                let baseline = domains.budget.bytes.get();
                let live = crate::runtime::arena::LIVE.with(Cell::get);
                let mut completed = false;
                for extra in (0..100_000).step_by(257) {
                    domains.budget.limit.set(baseline + extra);
                    let result = capture(&domains, &set);
                    completed = result.is_ok();
                    drop(result);
                    assert_eq!(domains.budget.bytes.get(), baseline);
                    assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), live);
                    if completed {
                        break;
                    }
                }
                assert!(completed);
                domains
                    .budget
                    .limit
                    .set(DomainLimits::default().max_state_bytes);
            }
            for owner in owners {
                owner.close();
            }
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    fn query_revision(
        arena: &mut Arena,
        registry: &Rc<RefCell<Domains>>,
        receipt: u64,
        token: u64,
    ) -> (u64, u64) {
        let mut frame = CpsFrame::new(4);
        frame.slots[0] = receipt;
        frame.slots[1] = token;
        assert_eq!(
            unsafe {
                super::super::poll(arena, &mut frame, registry, Operation::SetRevisionChecked)
            }
            .unwrap(),
            Some(0)
        );
        (frame.slots[2], frame.slots[3])
    }
    #[test]
    fn state_domains_submission_revision_distinguishes_pending_failed_and_non_targets() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, owners) = setup(mode, 4);
            vm.domains
                .borrow_mut()
                .entries
                .get_mut(&owners[0].handle.sequence)
                .unwrap()
                .revision = u64::MAX - 1;
            let set = candidates(&mut vm, &owners);
            let receipt = admit(&vm.domains, set);
            let mut arena = Arena::default();
            let bits = adopt(&mut arena, receipt).unwrap();
            let target = token(&vm, &owners[0]);
            let readonly = token(&vm, &owners[3]);
            assert_eq!(
                query_revision(&mut arena, &vm.domains, bits, target),
                (11, 0)
            );
            let foreign = VmEngine::new();
            assert_eq!(
                query_revision(&mut arena, &foreign.domains, bits, target),
                (12, 0)
            );
            vm.poll_tasks(100).unwrap();
            assert_eq!(
                query_revision(&mut arena, &vm.domains, bits, target),
                (0, u64::MAX)
            );
            assert_eq!(
                query_revision(&mut arena, &vm.domains, bits, readonly),
                (10, 0)
            );
            for owner in &owners {
                owner.close();
            }
            assert_eq!(
                query_revision(&mut arena, &vm.domains, bits, target),
                (0, u64::MAX)
            );
            unsafe {
                Buffer::release(&mut arena, bits as *mut Buffer).unwrap();
            }
            drop(arena);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            // Every rejection retains its exact CommitError, including cancellation.
            for status in [
                Status::Conflict,
                Status::Closed,
                Status::DomainFailed,
                Status::InvalidAccess,
                Status::Cancelled,
                Status::ResourceLimit,
                Status::Expired,
                Status::OutboxFull,
                Status::OutboxClosed,
            ] {
                let mut arena = Arena::default();
                let bits = adopt(
                    &mut arena,
                    Receipt {
                        registry: vm.domains.borrow().identity,
                        request: None,
                        rejected: Some(status),
                    },
                )
                .unwrap();
                assert_eq!(
                    query_revision(&mut arena, &vm.domains, bits, target),
                    (status as u64, 0)
                );
                assert_eq!(
                    query_revision(&mut arena, &foreign.domains, bits, target),
                    (12, 0)
                );
                unsafe {
                    Buffer::release(&mut arena, bits as *mut Buffer).unwrap();
                }
            }
        }
    }
    #[test]
    fn state_domains_many_snapshot_and_atomic_writes_share_one_fifo() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, owners) = setup(mode, 5);
            let baseline = vm.domains.borrow().budget.bytes.get();
            let candidates = candidates(&mut vm, &owners);
            let handles: Vec<_> = owners[..4].iter().map(DomainOwner::read_handle).collect();
            let mut before = vm.queue_domain_access(&handles, "sum4", &[]).unwrap();
            let receipt = admit(&vm.domains, candidates);
            let mut after = vm.queue_domain_access(&handles, "sum4", &[]).unwrap();
            vm.poll_tasks(1).unwrap();
            assert!(matches!(
                before.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(10)))
            ));
            assert!(receipt.result().is_none());
            vm.poll_tasks(1).unwrap();
            assert_eq!(receipt.result(), Some((Status::Committed, 0)));
            let id = receipt.request.as_ref().unwrap().sequence.unwrap();
            assert_eq!(vm.domains.borrow().requests[&id].set_revisions.len(), 3);
            for owner in &owners[..3] {
                assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
            }
            for owner in &owners[3..] {
                assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
            }
            vm.poll_tasks(1).unwrap();
            assert!(matches!(
                after.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(67)))
            ));
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            receipt.request.as_ref().unwrap().cancel();
            for owner in &owners {
                owner.close();
            }
            assert_eq!(receipt.result(), Some((Status::Committed, 0)));
            drop(receipt);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_many_readonly_fourth_and_fifth_control_whole_commit() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for action in 0..10 {
                let (mut vm, owners) = setup(mode, 5);
                let candidates = candidates(&mut vm, &owners);
                let mut write = if action == 0 || action == 1 || action == 4 {
                    Some(
                        vm.queue_domain_call(
                            owners[if action == 1 { 4 } else { 3 }].write_handle(),
                            if action == 4 { "fail_write" } else { "add" },
                            &[InstanceValue::Int32(0)],
                        )
                        .unwrap(),
                    )
                } else {
                    None
                };
                if action == 8 {
                    vm.set_domain_limits(DomainLimits {
                        max_request_bytes: 1,
                        ..Default::default()
                    })
                    .unwrap();
                }
                if action == 9 {
                    vm.domains
                        .borrow_mut()
                        .entries
                        .get_mut(&owners[2].handle.sequence)
                        .unwrap()
                        .revision = u64::MAX;
                }
                let receipt = admit(&vm.domains, candidates);
                let status = match action {
                    0 | 1 | 9 => Status::Conflict,
                    2 => {
                        owners[3].close();
                        Status::Closed
                    }
                    3 => {
                        owners[4].close();
                        Status::Closed
                    }
                    4 => Status::DomainFailed,
                    5 => {
                        receipt.request.as_ref().unwrap().cancel();
                        Status::Cancelled
                    }
                    6 => {
                        drop(receipt);
                        vm.poll_tasks(100).unwrap();
                        for (i, owner) in owners.iter().enumerate() {
                            read(&mut vm, owner, i as i32 + 1);
                            owner.close();
                        }
                        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                        continue;
                    }
                    7 => {
                        owners[2].close();
                        Status::Closed
                    }
                    _ => Status::ResourceLimit,
                };
                vm.poll_tasks(100).unwrap();
                assert_eq!(receipt.result(), Some((status, 0)), "action {action}");
                if let Some(write) = &mut write {
                    write.take_result();
                }
                vm.set_domain_limits(Default::default()).unwrap();
                for (i, owner) in owners[..3].iter().enumerate() {
                    if action == 7 && i == 2 {
                        continue;
                    }
                    read(&mut vm, owner, i as i32 + 1);
                    assert_eq!(
                        vm.domain_revision(owner.read_handle()).unwrap(),
                        if action == 9 && i == 2 { u64::MAX } else { 0 }
                    );
                }
                drop(receipt);
                for owner in &owners {
                    owner.close();
                }
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
    #[test]
    fn state_domains_many_short_access_checks_all_aliases_and_permissions() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, owners) = setup(mode, 5);
            let mut handles: Vec<_> = owners[..4].iter().map(DomainOwner::write_handle).collect();
            let mut wrong = handles.clone();
            wrong[2] = owners[2].read_handle();
            assert!(vm
                .queue_domain_access(&wrong, "write3", &[InstanceValue::Int32(7)])
                .is_err());
            wrong = handles.clone();
            wrong[3] = handles[2];
            assert!(vm
                .queue_domain_access(&wrong, "write3", &[InstanceValue::Int32(7)])
                .is_err());
            assert!(vm
                .queue_domain_access(&handles[..3], "write3", &[InstanceValue::Int32(7)])
                .is_err());
            let mut success = vm
                .queue_domain_access(&handles, "write3", &[InstanceValue::Int32(7)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                success.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(4)))
            ));
            for (i, owner) in owners[..3].iter().enumerate() {
                read(&mut vm, owner, 7 + i as i32);
            }
            handles[..3].reverse();
            let mut failed = vm
                .queue_domain_access(&handles, "fail3", &[InstanceValue::Int32(0)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                failed.take_result(),
                Some(DomainOutcome::Failed(_))
            ));
            for owner in &owners[..3] {
                assert!(vm.domain_revision(owner.read_handle()).is_err());
            }
            read(&mut vm, &owners[3], 4);
            read(&mut vm, &owners[4], 5);
            for owner in &owners {
                owner.close();
            }
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    struct Clock {
        time: Cell<Instant>,
        skip: Cell<usize>,
        hook: RefCell<Option<Box<dyn FnOnce()>>>,
    }
    impl crate::runtime::executor::Clock for Clock {
        fn now(&self) -> Instant {
            if self.skip.get() > 0 {
                self.skip.set(self.skip.get() - 1);
            } else if let Some(hook) = self.hook.borrow_mut().take() {
                hook();
            }
            self.time.get()
        }
    }
    #[test]
    fn state_domains_many_revalidate_after_final_clock_callback() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for close in [false, true] {
                let (mut vm, _) = setup(mode, 0);
                let clock = Rc::new(Clock {
                    time: Cell::new(Instant::now()),
                    skip: Cell::new(0),
                    hook: RefCell::new(None),
                });
                vm.configure_tasks(clock.clone(), 64).unwrap();
                let mut owners: Vec<_> = (1..=4)
                    .map(|n| {
                        vm.create_domain("make", &[InstanceValue::Int32(n)])
                            .unwrap()
                    })
                    .collect();
                let mut set = candidates(&mut vm, &owners);
                let deadline = deadline_after(&vm.domains, 100).unwrap();
                set.deadline = Some(deadline);
                let receipt = admit(&vm.domains, set);
                clock.skip.set(1);
                let fourth = if close { owners.pop() } else { None };
                let hook_clock = clock.clone();
                *clock.hook.borrow_mut() = Some(Box::new(move || {
                    if let Some(fourth) = fourth {
                        fourth.close();
                    } else {
                        hook_clock.time.set(deadline.at);
                    }
                }));
                vm.poll_tasks(100).unwrap();
                assert!(clock.hook.borrow().is_none());
                assert_eq!(
                    receipt.result(),
                    Some((
                        if close {
                            Status::Closed
                        } else {
                            Status::Expired
                        },
                        0
                    ))
                );
                for (i, owner) in owners.iter().enumerate() {
                    read(&mut vm, owner, i as i32 + 1);
                    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
                    owner.close();
                }
                drop(receipt);
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
    #[test]
    fn state_domains_many_all_sixty_four_roots_publish_together() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, owners) = setup(mode, MAX_MEMBERS);
            let baseline = vm.domains.borrow().budget.bytes.get();
            let saved = snapshot(&mut vm, &owners);
            assert_eq!(saved.roots.len(), MAX_MEMBERS);
            let mut set = proposal(&vm.domains, saved.reads.clone()).unwrap();
            drop(saved);
            for owner in &owners {
                add(
                    &vm.domains,
                    &mut set,
                    token(&vm, owner),
                    vm.current.clone().unwrap(),
                    "make",
                    InstanceValue::Int32(99),
                    Default::default(),
                )
                .unwrap();
            }
            let receipt = admit(&vm.domains, set);
            vm.poll_tasks(1).unwrap();
            assert_eq!(receipt.result(), Some((Status::Committed, 0)));
            let id = receipt.request.as_ref().unwrap().sequence.unwrap();
            assert_eq!(
                vm.domains.borrow().requests[&id].set_revisions.len(),
                MAX_MEMBERS
            );
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            for owner in &owners {
                read(&mut vm, owner, 99);
                assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
                owner.close();
            }
            drop(receipt);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_many_limits_and_late_candidate_failures_reclaim_storage() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, owners) = setup(mode, 65);
            let baseline = vm.domains.borrow().budget.bytes.get();
            let mut access = accesses(&vm.domains).unwrap();
            for owner in &owners[..64] {
                include(&vm.domains, &mut access, owner.read_handle()).unwrap();
            }
            assert!(include(&vm.domains, &mut access, owners[64].read_handle()).is_err());
            include(&vm.domains, &mut access, owners[0].read_handle()).unwrap();
            drop(access);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            let saved = snapshot(&mut vm, &owners[..5]);
            let mut set = proposal(&vm.domains, saved.reads.clone()).unwrap();
            drop(saved);
            for owner in &owners[..2] {
                add(
                    &vm.domains,
                    &mut set,
                    token(&vm, owner),
                    vm.current.clone().unwrap(),
                    "make",
                    InstanceValue::Int32(99),
                    Default::default(),
                )
                .unwrap();
            }
            assert!(add(
                &vm.domains,
                &mut set,
                token(&vm, &owners[2]),
                vm.current.clone().unwrap(),
                "broken_make",
                InstanceValue::Int32(0),
                Default::default()
            )
            .is_err());
            assert!(add(
                &vm.domains,
                &mut set,
                token(&vm, &owners[0]),
                vm.current.clone().unwrap(),
                "make",
                InstanceValue::Int32(99),
                Default::default()
            )
            .is_err());
            assert!(add(
                &vm.domains,
                &mut set,
                token(&vm, &owners[5]),
                vm.current.clone().unwrap(),
                "make",
                InstanceValue::Int32(99),
                Default::default()
            )
            .is_err());
            drop(set);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            let mut access = accesses(&vm.domains).unwrap();
            for owner in &owners[..5] {
                include(&vm.domains, &mut access, owner.read_handle()).unwrap();
            }
            let before = vm.domains.borrow().budget.bytes.get();
            let trial = capture(&vm.domains.borrow(), &access).unwrap();
            let snapshot_bytes = vm.domains.borrow().budget.bytes.get() - before;
            drop(trial);
            let limit = before + snapshot_bytes - 1;
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: limit,
                ..Default::default()
            })
            .unwrap();
            let mut request = submit_snapshot(&vm.domains, access).unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                request.take_result(),
                Some(DomainOutcome::Failed(_))
            ));
            vm.set_domain_limits(Default::default()).unwrap();
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            for (i, owner) in owners.iter().enumerate() {
                read(&mut vm, owner, i as i32 + 1);
                owner.close();
            }
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
