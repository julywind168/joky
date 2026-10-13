//! Complete bounded domain access sets on the existing FIFO executor.
use super::*;

pub(super) struct Pair {
    pub(super) handles: Vec<DomainHandle>,
    module: LoadedModule,
    method: String,
    info: FunctionInfo,
    words: Vec<u64>,
    limits: InterpreterLimits,
}
impl VmEngine {
    /// Queue one synchronous operation over two scalar state records. The first
    /// two parameters declare read/write access; remaining inputs are scalars.
    /// Duplicate shared reads are allowed; any alias involving a write is rejected.
    pub fn queue_domain_pair(
        &self,
        handles: [DomainHandle; 2],
        method: &str,
        args: &[InstanceValue],
    ) -> Result<DomainRequest, Error> {
        let module = self.domains.borrow().get(handles[0])?.module.clone();
        submit(
            &self.domains,
            &handles,
            module,
            method,
            args,
            self.interpreter_limits(),
        )
    }
}
impl Pair {
    pub(super) fn module_key(&self) -> usize {
        code::module_key(&self.module)
    }
    pub(super) fn readonly(&self) -> bool {
        self.info.signature.params[..self.handles.len()]
            .iter()
            .all(|ty| !ty.reference_type().unwrap().mutable)
    }
}
impl VmEngine {
    /// Queue a complete 2..=64 domain access set. Leading references declare
    /// permissions, trailing scalar parameters are captured before admission.
    pub fn queue_domain_access(
        &self,
        handles: &[DomainHandle],
        method: &str,
        args: &[InstanceValue],
    ) -> Result<DomainRequest, Error> {
        let first = *handles
            .first()
            .ok_or_else(|| invalid("Empty domain access set"))?;
        let module = self.domains.borrow().get(first)?.module.clone();
        submit(
            &self.domains,
            handles,
            module,
            method,
            args,
            self.interpreter_limits(),
        )
    }
}
pub(super) fn submit(
    registry: &Rc<RefCell<Domains>>,
    handles: &[DomainHandle],
    module: LoadedModule,
    method: &str,
    args: &[InstanceValue],
    limits: InterpreterLimits,
) -> Result<DomainRequest, Error> {
    if !(2..=64).contains(&handles.len()) {
        return Err(limited("Domain access requires 2 to 64 participants"));
    }
    let info = module.instance_info(method)?;
    if !info.domain_entry || !crate::core::state_domain::pair_eligible(&info) {
        return Err(invalid(
            "Domain pair requires a proved synchronous scalar-record entry",
        ));
    }
    let count = crate::core::state_domain::access_count(&info);
    if count != handles.len() {
        return Err(invalid(
            "Domain access count does not match entry references",
        ));
    }
    let modes: Vec<_> = info.signature.params[..count]
        .iter()
        .map(|ty| ty.reference_type().unwrap())
        .collect();
    for i in 0..count {
        if modes[i].mutable && !handles[i].writable {
            return Err(invalid(
                "Read-only state handle cannot authorize a pair write",
            ));
        }
        for j in 0..i {
            if handles[i].registry == handles[j].registry
                && handles[i].sequence == handles[j].sequence
                && (modes[i].mutable || modes[j].mutable)
            {
                return Err(invalid(
                    "Domain pair cannot alias a writable state reference",
                ));
            }
        }
    }
    let module = resolve(&registry.borrow(), handles, method, &info)?;
    let inputs = arguments(&info.signature.params[count..], args)?;
    let mut words = Vec::new();
    words
        .try_reserve_exact(inputs.len() + count)
        .map_err(|_| limited("Domain pair input allocation failed"))?;
    // State addresses are resolved only on execution, never captured in the queue.
    words.resize(count, 0);
    words.extend(inputs);
    let extra = words
        .len()
        .checked_mul(std::mem::size_of::<u64>())
        .and_then(|n| n.checked_add(method.len()))
        .and_then(|n| n.checked_add(count * std::mem::size_of::<DomainHandle>()))
        .ok_or_else(|| limited("Domain pair size overflow"))?;
    enqueue(
        registry,
        handles[0].sequence,
        Work::Pair(Pair {
            handles: handles.to_vec(),
            module,
            method: method.to_owned(),
            info,
            words,
            limits,
        }),
        extra,
    )
}

/// The first participant owns the named operation. Other participants supply
/// checked state, not competing implementations of the same operation name.
fn resolve(
    domains: &Domains,
    handles: &[DomainHandle],
    method: &str,
    caller: &FunctionInfo,
) -> Result<LoadedModule, Error> {
    let module = &domains.get(handles[0])?.module;
    let mismatch = || {
        error(
            ErrorCode::TypeMismatch,
            "Domain access operation or state schema changed",
        )
    };
    let current = module.instance_info(method).map_err(|_| mismatch())?;
    if !current.domain_entry
        || !crate::core::state_domain::pair_eligible(&current)
        || !code::compatible_entry(caller, &current)
    {
        return Err(mismatch());
    }
    for (handle, ty) in handles.iter().zip(&current.signature.params) {
        let domain = domains.get(*handle)?;
        let reference = ty.reference_type().ok_or_else(mismatch)?;
        if !code::compatible(&domain.ty, &reference.target)
            || (reference.mutable && !handle.writable)
        {
            return Err(mismatch());
        }
    }
    Ok(module.clone())
}

pub(super) fn execute(
    domains: &mut Domains,
    pair: &mut Pair,
) -> (DomainOutcome, Vec<TaskId>, Vec<TaskGroup>) {
    let count = pair.handles.len();
    let mut mutable = [false; 64];
    for (i, ty) in pair.info.signature.params[..count].iter().enumerate() {
        mutable[i] = ty.reference_type().unwrap().mutable;
    }
    // No permission is held while waiting. Validate the entire set before any
    // root address is exposed or any user instruction can modify either state.
    let module = match resolve(domains, &pair.handles, &pair.method, &pair.info) {
        Ok(module) => module,
        Err(error) => return (DomainOutcome::Failed(error), Vec::new(), Vec::new()),
    };
    let mut revisions = [0; 64];
    for (index, handle) in pair.handles.iter().enumerate() {
        let domain = match domains.get(*handle) {
            Ok(domain) => domain,
            Err(error) => return (DomainOutcome::Failed(error), Vec::new(), Vec::new()),
        };
        let Some(next) = domain.revision.checked_add(u64::from(mutable[index])) else {
            return (
                DomainOutcome::Failed(limited("State domain revision exhausted")),
                Vec::new(),
                Vec::new(),
            );
        };
        revisions[index] = next;
    }
    for (index, handle) in pair.handles.iter().enumerate() {
        pair.words[index] = domains.entries[&handle.sequence]
            .root
            .as_ref()
            .unwrap()
            .scalar_pointer();
    }
    // Scalar roots cannot retain invocation storage or move owners between arenas.
    let mut arena = Arena::with_budget(domains.budget.clone(), None);
    let result = module
        .invoke_instance(&pair.method, &pair.words, pair.limits, &mut arena)
        .and_then(|words| {
            InstanceValue::from_bits(
                &pair.info.signature.ret,
                words.first().copied().unwrap_or(0),
            )
            .ok_or_else(|| invalid("Invalid domain pair scalar result"))
        });
    pair.words[..count].fill(0);
    drop(arena);
    let mut jobs = Vec::new();
    let mut groups = Vec::new();
    let outcome = match result {
        Ok(value) => {
            for (index, handle) in pair.handles.iter().enumerate() {
                if mutable[index] {
                    domains.entries.get_mut(&handle.sequence).unwrap().revision = revisions[index];
                }
            }
            DomainOutcome::Value(value)
        }
        Err(error) => {
            // A failed short operation is not rollback. Every writable participant
            // is isolated; shared-read participants and unrelated domains survive.
            for (index, handle) in pair.handles.iter().enumerate() {
                if mutable[index] {
                    let domain = domains.entries.get_mut(&handle.sequence).unwrap();
                    domain.root = None;
                    domain.failure = Some(error.clone());
                    groups.extend(domain.tasks.values().map(|task| task.group.clone()));
                    jobs.extend(
                        domains
                            .finish_pending(handle.sequence, DomainOutcome::Failed(error.clone())),
                    );
                }
            }
            DomainOutcome::Failed(error)
        }
    };
    (outcome, jobs, groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::VmMode;
    const SOURCE: &str = include_str!("../../../tests/fixtures/state_domain_pairs.jk");
    fn owner(vm: &VmEngine, value: i32) -> DomainOwner {
        vm.create_domain("make", &[InstanceValue::Int32(value)])
            .unwrap()
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
    fn state_domains_pairs_share_fifo_and_normalize_duplicate_reads() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let a = owner(&vm, 10);
            let b = owner(&vm, 20);
            let c = owner(&vm, 30);
            let mut first = vm
                .queue_domain_pair(
                    [a.write_handle(), b.write_handle()],
                    "transfer",
                    &[InstanceValue::Int32(3)],
                )
                .unwrap();
            let mut single = vm
                .queue_domain_call(b.write_handle(), "add", &[InstanceValue::Int32(4)])
                .unwrap();
            let mut reverse = vm
                .queue_domain_pair(
                    [b.write_handle(), a.write_handle()],
                    "transfer",
                    &[InstanceValue::Int32(2)],
                )
                .unwrap();
            {
                let domains = vm.domains.borrow();
                for request in domains.requests.values() {
                    if let Work::Pair(pair) = &request.work {
                        assert_eq!(pair.words[..2], [0, 0]);
                    }
                }
            }
            vm.poll_tasks(1).unwrap();
            assert!(matches!(
                first.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(7)))
            ));
            assert!(single.take_result().is_none());
            vm.poll_tasks(1).unwrap();
            assert!(matches!(
                single.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(27)))
            ));
            vm.poll_tasks(1).unwrap();
            assert!(matches!(
                reverse.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(25)))
            ));
            read(&mut vm, &a, 9);
            read(&mut vm, &b, 25);
            read(&mut vm, &c, 30);
            assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 2);
            assert_eq!(vm.domain_revision(b.read_handle()).unwrap(), 3);
            let mut duplicate = vm
                .queue_domain_pair([a.read_handle(), a.read_handle()], "sum", &[])
                .unwrap();
            vm.poll_tasks(10).unwrap();
            assert!(matches!(
                duplicate.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(18)))
            ));
            assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 2);
            a.close();
            b.close();
            c.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_pairs_cancel_drop_or_close_either_participant_before_execution() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let live = crate::runtime::arena::LIVE.with(|n| n.get());
            for action in 0..4 {
                let a = owner(&vm, 10);
                let b = owner(&vm, 20);
                let request = vm
                    .queue_domain_pair(
                        [a.write_handle(), b.write_handle()],
                        "transfer",
                        &[InstanceValue::Int32(3)],
                    )
                    .unwrap();
                let mut request = Some(request);
                match action {
                    0 => {
                        request.as_ref().unwrap().cancel();
                    }
                    1 => {
                        request.take();
                    }
                    2 => {
                        a.close();
                    }
                    _ => {
                        b.close();
                    }
                }
                vm.poll_tasks(100).unwrap();
                if let Some(mut request) = request {
                    assert!(matches!(
                        (action, request.take_result()),
                        (0, Some(DomainOutcome::Cancelled)) | (2 | 3, Some(DomainOutcome::Closed))
                    ));
                }
                if action != 2 {
                    read(&mut vm, &a, 10);
                    assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
                }
                if action != 3 {
                    read(&mut vm, &b, 20);
                    assert_eq!(vm.domain_revision(b.read_handle()).unwrap(), 0);
                }
                a.close();
                b.close();
                assert_eq!(vm.executor.task_count(), 0);
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), live);
            }
        }
    }
    #[test]
    fn state_domains_pairs_preflight_rejects_aliases_permissions_and_limits() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let a = owner(&vm, 10);
            let b = owner(&vm, 20);
            for handles in [
                [a.write_handle(), a.write_handle()],
                [a.read_handle(), b.write_handle()],
            ] {
                assert!(vm
                    .queue_domain_pair(handles, "transfer", &[InstanceValue::Int32(1)])
                    .is_err());
            }
            assert!(vm
                .queue_domain_pair(
                    [a.read_handle(), a.write_handle()],
                    "fail_mixed",
                    &[InstanceValue::Int32(0)]
                )
                .is_err());
            let foreign = VmEngine::with_mode(mode);
            assert!(foreign
                .queue_domain_pair([a.read_handle(), b.read_handle()], "sum", &[])
                .is_err());
            vm.set_domain_limits(DomainLimits {
                max_requests: 1,
                ..Default::default()
            })
            .unwrap();
            let occupied = vm
                .queue_domain_pair([a.read_handle(), b.read_handle()], "sum", &[])
                .unwrap();
            assert!(vm
                .queue_domain_pair(
                    [a.write_handle(), b.write_handle()],
                    "transfer",
                    &[InstanceValue::Int32(1)]
                )
                .is_err());
            drop(occupied);
            vm.set_domain_limits(DomainLimits {
                max_request_bytes: 1,
                ..Default::default()
            })
            .unwrap();
            assert!(vm
                .queue_domain_pair(
                    [a.write_handle(), b.write_handle()],
                    "transfer",
                    &[InstanceValue::Int32(1)]
                )
                .is_err());
            vm.set_domain_limits(Default::default()).unwrap();
            assert_eq!(vm.executor.task_count(), 0);
            assert_eq!(vm.domains.borrow().usage().1, 0);
            vm.domains
                .borrow_mut()
                .entries
                .get_mut(&b.handle.sequence)
                .unwrap()
                .revision = u64::MAX;
            let mut exhausted = vm
                .queue_domain_pair(
                    [a.write_handle(), b.write_handle()],
                    "transfer",
                    &[InstanceValue::Int32(1)],
                )
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(
                matches!(exhausted.take_result(),Some(DomainOutcome::Failed(e)) if e.code()==ErrorCode::ResourceLimit)
            );
            read(&mut vm, &a, 10);
            read(&mut vm, &b, 20);
            assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
            vm.load_source(SOURCE).unwrap();
            let fresh = owner(&vm, 30);
            let mut mixed = vm
                .queue_domain_pair([a.read_handle(), fresh.read_handle()], "sum", &[])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(
                mixed.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(40)))
            ));
            a.close();
            b.close();
            fresh.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_pairs_failure_isolates_only_writable_participants() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            for (method, mut_a, mut_b) in [
                ("fail_both", true, true),
                ("fail_mixed", false, true),
                ("fail_read", false, false),
            ] {
                let a = owner(&vm, 10);
                let b = owner(&vm, 20);
                let c = owner(&vm, 30);
                let mut request = vm
                    .queue_domain_pair(
                        [a.write_handle(), b.write_handle()],
                        method,
                        &[InstanceValue::Int32(0)],
                    )
                    .unwrap();
                let mut later = vm
                    .queue_domain_pair([c.read_handle(), b.read_handle()], "sum", &[])
                    .unwrap();
                vm.poll_tasks(100).unwrap();
                assert!(matches!(
                    request.take_result(),
                    Some(DomainOutcome::Failed(_))
                ));
                if mut_b {
                    assert!(matches!(
                        later.take_result(),
                        Some(DomainOutcome::Failed(_))
                    ));
                } else {
                    assert!(matches!(
                        later.take_result(),
                        Some(DomainOutcome::Value(InstanceValue::Int32(50)))
                    ));
                }
                if mut_a {
                    assert!(vm.queue_domain_call(a.read_handle(), "read", &[]).is_err());
                } else {
                    read(&mut vm, &a, 10);
                }
                if mut_b {
                    assert!(vm.queue_domain_call(b.read_handle(), "read", &[]).is_err());
                } else {
                    read(&mut vm, &b, 20);
                }
                read(&mut vm, &c, 30);
                a.close();
                b.close();
                c.close();
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
}
