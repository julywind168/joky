//! Two candidates from one joint snapshot, published after complete preflight.
use super::*;

pub(crate) struct Bundle {
    candidates: [Candidate; 2],
}
impl Bundle {
    pub(super) fn attach_outbox(&mut self, batch: &mut Option<outbox::Batch>) -> Result<(), Error> {
        attach(&mut self.candidates[0].outbox, batch)
    }
    pub(crate) fn touches(&self, domain: u64) -> bool {
        self.candidates.iter().any(|c| c.reads.touches(domain))
    }
    pub(crate) fn timed(&self) -> bool {
        self.candidates.iter().any(|c| c.deadline.is_some())
    }
}
fn extend(
    registry: &Rc<RefCell<Domains>>,
    first: Candidate,
    module: LoadedModule,
    name: &str,
    input: InstanceValue,
    limits: InterpreterLimits,
) -> Result<Bundle, Error> {
    let version = first.reads.second().ok_or_else(|| {
        invalid("Candidate pair requires a joint snapshot of two distinct domains")
    })?;
    let mut second = construct(
        registry,
        module,
        version.handle,
        version.revision,
        name,
        input,
        limits,
    )?;
    // Both candidates retain the complete original read set; no snapshot merging.
    second.reads = first.reads.clone();
    Ok(Bundle {
        candidates: [first, second],
    })
}
fn admit(registry: &Rc<RefCell<Domains>>, tokens: [u64; 2], bundle: Bundle) -> Receipt {
    let accepted = (|| {
        let domains = registry.borrow();
        let [first, second] = &bundle.candidates;
        if first.base.registry == second.base.registry
            && first.base.sequence == second.base.sequence
        {
            return Err(Status::InvalidAccess);
        }
        for (candidate, token) in bundle.candidates.iter().zip(tokens) {
            access(&domains, token, candidate)?;
        }
        Ok(())
    })();
    let result = accepted.and_then(|()| {
        enqueue(
            registry,
            bundle.candidates[0].base.sequence,
            Work::CommitPair(Some(Box::new(bundle))),
            std::mem::size_of::<Bundle>(),
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
pub(super) fn execute(domains: &mut Domains, request: &mut Request, now: Option<Instant>) {
    let Work::CommitPair(bundle) = &mut request.work else {
        unreachable!()
    };
    let mut bundle = *bundle.take().unwrap();
    let next = (|| {
        Ok::<_, Status>([
            validate(domains, &bundle.candidates[0], now)?,
            validate(domains, &bundle.candidates[1], now)?,
        ])
    })();
    let next = match next {
        Ok(next) => next,
        Err(status) => {
            request.commit_status = Some(status);
            request.finish(DomainOutcome::Value(InstanceValue::Unit));
            return;
        }
    };
    if let Some(batch) = &mut bundle.candidates[0].outbox {
        if let Err(status) = batch.prepare(domains) {
            request.commit_status = Some(status);
            request.finish(DomainOutcome::Value(InstanceValue::Unit));
            return;
        }
    }
    // Preflight is complete. Keep both retired roots until the full result is
    // recorded. This interval performs no allocation, user code, or suspension.
    let [mut first, second] = bundle.candidates;
    let domain = domains.entries.get_mut(&first.base.sequence).unwrap();
    let retired_first = domain.root.replace(first.root);
    domain.revision = next[0];
    let domain = domains.entries.get_mut(&second.base.sequence).unwrap();
    let retired_second = domain.root.replace(second.root);
    domain.revision = next[1];
    if let Some(batch) = &mut first.outbox {
        batch.publish();
    }
    request.observed_revision = next[0];
    request.second_revision = next[1];
    request.commit_status = Some(Status::Committed);
    request.outcome = Some(DomainOutcome::Value(InstanceValue::Unit));
    if let Some(batch) = &first.outbox {
        batch.wake();
    }
    drop((retired_first, retired_second));
    request.finish(DomainOutcome::Value(InstanceValue::Unit));
}
pub(super) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<Option<u64>, Error> {
    if op == Operation::CandidatePair {
        let candidate_lease = lease(frame.slots[1])?;
        let stored = &*candidate_lease.token().cast::<Stored<Option<Candidate>>>();
        let first = stored
            .value
            .borrow_mut()
            .take()
            .ok_or_else(|| invalid("Candidate was already consumed"))?;
        let (module, _) = source::module(registry, frame.slots[0])?;
        let name = crate::runtime::owned_text::borrow(frame.slots[2] as *const Buffer)?;
        let input = source::scalar_argument(
            &module.instance_info(name)?.signature.params,
            frame.slots[3],
        )?;
        let bundle = extend(
            registry,
            first,
            module,
            name,
            input,
            crate::runtime::suspension::current_limits(),
        )?;
        return adopt(arena, Some(bundle)).map(Some);
    }
    let candidate_lease = lease(frame.slots[2])?;
    let stored = &*candidate_lease.token().cast::<Stored<Option<Bundle>>>();
    let mut bundle = stored
        .value
        .borrow_mut()
        .take()
        .ok_or_else(|| invalid("Candidate pair was already consumed"))?;
    if op == Operation::CommitPairBefore {
        let deadline_lease = lease(frame.slots[3])?;
        let deadline = &*deadline_lease.token().cast::<Stored<Deadline>>();
        let deadline = *deadline.value.borrow();
        for candidate in &mut bundle.candidates {
            candidate.deadline = Some(deadline);
        }
    }
    let bits = adopt(
        arena,
        Receipt {
            request: None,
            rejected: None,
        },
    )?;
    let receipt_lease = lease(bits)?;
    let receipt = &*receipt_lease.token().cast::<Stored<Receipt>>();
    *receipt.value.borrow_mut() = admit(registry, [frame.slots[0], frame.slots[1]], bundle);
    Ok(Some(bits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{state_domain_tests::SOURCE, VmMode};
    fn setup(mode: VmMode) -> (VmEngine, DomainOwner, DomainOwner) {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&format!("{SOURCE}\nfn total(a:&State,b:&State)->Int32{{a.count+b.count}}\nfn broken_make(n:Int32)->State{{make(1/n)}}")).unwrap();
        let a = vm
            .create_domain("make", &[InstanceValue::Int32(7)])
            .unwrap();
        let b = vm
            .create_domain("make", &[InstanceValue::Int32(10)])
            .unwrap();
        (vm, a, b)
    }
    fn first(vm: &mut VmEngine, a: &DomainOwner, b: &DomainOwner) -> Candidate {
        let mut snapshot = vm
            .queue_domain_pair([a.read_handle(), b.read_handle()], "total", &[])
            .unwrap();
        vm.poll_tasks(100).unwrap();
        let (outcome, _, reads) = snapshot.take_snapshot_result().unwrap();
        assert!(matches!(outcome, DomainOutcome::Value(_)));
        let reads = reads.unwrap();
        let mut candidate = construct(
            &vm.domains,
            vm.current.clone().unwrap(),
            a.read_handle(),
            reads.first.revision,
            "make",
            InstanceValue::Int32(20),
            Default::default(),
        )
        .unwrap();
        candidate.reads = reads;
        candidate
    }
    fn bundle(vm: &mut VmEngine, a: &DomainOwner, b: &DomainOwner) -> Bundle {
        let first = first(vm, a, b);
        extend(
            &vm.domains,
            first,
            vm.current.clone().unwrap(),
            "make",
            InstanceValue::Int32(30),
            Default::default(),
        )
        .unwrap()
    }
    fn token(vm: &VmEngine, owner: &DomainOwner) -> u64 {
        vm.domains.borrow().entries[&owner.handle.sequence].source_token * 2 + 1
    }
    fn tokens(vm: &VmEngine, a: &DomainOwner, b: &DomainOwner) -> [u64; 2] {
        [token(vm, a), token(vm, b)]
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
    fn state_domains_pair_commits_publish_both_roots_between_joint_reads() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, a, b) = setup(mode);
            let baseline = vm.domains.borrow().budget.bytes.get();
            let candidates = bundle(&mut vm, &a, &b);
            let mut before = vm
                .queue_domain_pair([a.read_handle(), b.read_handle()], "total", &[])
                .unwrap();
            let receipt = admit(&vm.domains, tokens(&vm, &a, &b), candidates);
            let mut after = vm
                .queue_domain_pair([a.read_handle(), b.read_handle()], "total", &[])
                .unwrap();
            vm.poll_tasks(1).unwrap();
            assert!(matches!(
                before.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(17)))
            ));
            assert!(receipt.result().is_none());
            vm.poll_tasks(1).unwrap();
            assert_eq!(receipt.result(), Some((Status::Committed, 1)));
            let sequence = receipt.request.as_ref().unwrap().sequence.unwrap();
            assert_eq!(vm.domains.borrow().requests[&sequence].second_revision, 1);
            assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 1);
            assert_eq!(vm.domain_revision(b.read_handle()).unwrap(), 1);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            vm.poll_tasks(1).unwrap();
            assert!(matches!(
                after.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(50)))
            ));
            receipt.request.as_ref().unwrap().cancel();
            a.close();
            b.close();
            assert_eq!(receipt.result(), Some((Status::Committed, 1)));
            drop(receipt);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn state_domains_pair_commits_preflight_and_cancellation_never_publish_one_root() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for action in 0..12 {
                let (mut vm, a, b) = setup(mode);
                if action == 2 {
                    vm.domains
                        .borrow_mut()
                        .entries
                        .get_mut(&b.handle.sequence)
                        .unwrap()
                        .revision = u64::MAX;
                }
                let mut candidates = bundle(&mut vm, &a, &b);
                let mut tokens = tokens(&vm, &a, &b);
                if action == 3 {
                    tokens[1] = tokens[0];
                }
                if action == 4 {
                    tokens[1] &= !1;
                }
                if action == 10 {
                    vm.set_domain_limits(DomainLimits {
                        max_request_bytes: 1,
                        ..Default::default()
                    })
                    .unwrap();
                }
                if action == 11 {
                    for c in &mut candidates.candidates {
                        c.deadline = Some(deadline_after(&vm.domains, 0).unwrap());
                    }
                }
                let mut write = if action == 0 || action == 1 || action == 7 {
                    Some(
                        vm.queue_domain_call(
                            if action == 0 {
                                a.write_handle()
                            } else {
                                b.write_handle()
                            },
                            if action == 7 { "fail_write" } else { "add" },
                            &[InstanceValue::Int32(0)],
                        )
                        .unwrap(),
                    )
                } else {
                    None
                };
                let receipt = admit(&vm.domains, tokens, candidates);
                let expected = match action {
                    0 | 1 => Status::Conflict,
                    2 | 10 => Status::ResourceLimit,
                    3 | 4 => Status::InvalidAccess,
                    5 => {
                        b.close();
                        Status::Closed
                    }
                    6 => {
                        a.close();
                        Status::Closed
                    }
                    7 => Status::DomainFailed,
                    8 => {
                        receipt.request.as_ref().unwrap().cancel();
                        Status::Cancelled
                    }
                    9 => {
                        drop(receipt);
                        vm.poll_tasks(100).unwrap();
                        read(&mut vm, &a, 7);
                        read(&mut vm, &b, 10);
                        a.close();
                        b.close();
                        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                        continue;
                    }
                    _ => Status::Expired,
                };
                vm.poll_tasks(100).unwrap();
                assert_eq!(receipt.result(), Some((expected, 0)), "action {action}");
                if let Some(write) = &mut write {
                    write.take_result();
                }
                vm.set_domain_limits(Default::default()).unwrap();
                if action != 6 {
                    read(&mut vm, &a, 7);
                    assert_eq!(
                        vm.domain_revision(a.read_handle()).unwrap(),
                        u64::from(action == 0)
                    );
                }
                if action != 5 && action != 7 {
                    read(&mut vm, &b, 10);
                    assert_eq!(
                        vm.domain_revision(b.read_handle()).unwrap(),
                        if action == 2 {
                            u64::MAX
                        } else {
                            u64::from(action == 1)
                        }
                    );
                }
                drop(receipt);
                a.close();
                b.close();
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
    #[test]
    fn state_domains_pair_commits_second_construction_and_adoption_failures_release_both() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (mut vm, a, b) = setup(mode);
            let baseline = vm.domains.borrow().budget.bytes.get();
            for action in 0..4 {
                let first = first(&mut vm, &a, &b);
                if action == 1 {
                    let current_bytes = vm.domains.borrow().budget.bytes.get();
                    vm.set_domain_limits(DomainLimits {
                        max_state_bytes: current_bytes,
                        ..Default::default()
                    })
                    .unwrap();
                }
                if action < 2 {
                    assert!(extend(
                        &vm.domains,
                        first,
                        vm.current.clone().unwrap(),
                        if action == 0 { "broken_make" } else { "make" },
                        InstanceValue::Int32(0),
                        Default::default()
                    )
                    .is_err());
                } else {
                    let pair = extend(
                        &vm.domains,
                        first,
                        vm.current.clone().unwrap(),
                        "make",
                        InstanceValue::Int32(30),
                        Default::default(),
                    )
                    .unwrap();
                    let budget = Rc::new(Budget {
                        limit: Cell::new(if action == 2 {
                            0
                        } else {
                            std::mem::size_of::<Stored<Option<Bundle>>>()
                        }),
                        ..Default::default()
                    });
                    let mut arena = Arena::with_budget(budget.clone(), None);
                    assert!(adopt(&mut arena, Some(pair)).is_err());
                    assert_eq!(budget.bytes.get(), 0);
                }
                vm.set_domain_limits(Default::default()).unwrap();
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
                read(&mut vm, &a, 7);
                read(&mut vm, &b, 10);
            }
            a.close();
            b.close();
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
    fn state_domains_pair_commits_recheck_after_clock_closes_second_participant() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let clock = Rc::new(Clock {
                time: Cell::new(Instant::now()),
                skip: Cell::new(0),
                hook: RefCell::new(None),
            });
            vm.configure_tasks(clock.clone(), 64).unwrap();
            vm.load_source(&format!(
                "{SOURCE}\nfn total(a:&State,b:&State)->Int32{{a.count+b.count}}"
            ))
            .unwrap();
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let mut candidates = bundle(&mut vm, &a, &b);
            let deadline = deadline_after(&vm.domains, 100).unwrap();
            for c in &mut candidates.candidates {
                c.deadline = Some(deadline);
            }
            let receipt = admit(&vm.domains, tokens(&vm, &a, &b), candidates);
            clock.skip.set(1);
            *clock.hook.borrow_mut() = Some(Box::new(move || {
                b.close();
            }));
            vm.poll_tasks(100).unwrap();
            assert!(clock.hook.borrow().is_none());
            assert_eq!(receipt.result(), Some((Status::Closed, 0)));
            read(&mut vm, &a, 7);
            assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
            drop(receipt);
            a.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
