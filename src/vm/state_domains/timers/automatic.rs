//! Domain-owned periodic methods, staged before any root or code publication.
use super::*;

pub(in crate::vm::state_domains) struct Registration {
    name: String,
    period: u64,
    state: Rc<RefCell<State>>,
}

pub(in crate::vm::state_domains) struct Prepared {
    registry: Weak<RefCell<Domains>>,
    next: Vec<Rc<Registration>>,
    added: Vec<Rc<Registration>>,
    retired: Vec<Rc<Registration>>,
    committed: bool,
}
impl Prepared {
    pub(in crate::vm::state_domains) fn commit(&mut self, domain: &mut Domain) {
        domain.automatic = std::mem::take(&mut self.next);
        self.committed = true;
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            if self.committed {
                for entry in &self.added {
                    let waiter = {
                        let mut state = entry.state.borrow_mut();
                        state.staged = false;
                        state.waiter.take()
                    };
                    if let Some(waiter) = waiter {
                        waiter.wake();
                    }
                }
            }
            let removed = if self.committed {
                &self.retired
            } else {
                &self.added
            };
            for entry in removed {
                cancel(&registry, &entry.state, Status::Cancelled);
            }
        }
    }
}

pub(in crate::vm::state_domains) fn prepare(
    registry: &Rc<RefCell<Domains>>,
    target: DomainHandle,
    module: &LoadedModule,
    ty: &Type,
    reset: bool,
) -> Result<Prepared, Error> {
    let mut declarations = Vec::new();
    for name in code::entry_names(module)
        .into_iter()
        .filter(|name| name.starts_with("$every/"))
    {
        let info = module.instance_info(&name)?;
        if let Some(period) = info.every {
            if info.domain_entry
                && info
                    .signature
                    .params
                    .first()
                    .and_then(Type::reference_type)
                    .is_some_and(|r| r.mutable && code::compatible(&r.target, ty))
            {
                declarations
                    .try_reserve(1)
                    .map_err(|_| limited("Periodic declaration allocation failed"))?;
                declarations.push((name, period));
            }
        }
    }
    declarations.sort_unstable();
    let (old, limits) = {
        let domains = registry.borrow();
        let domain = domains.get(target)?;
        (domain.automatic.clone(), domain.limits)
    };
    let mut plan = Prepared {
        registry: Rc::downgrade(registry),
        next: Vec::new(),
        added: Vec::new(),
        retired: old,
        committed: false,
    };
    for entries in [&mut plan.next, &mut plan.added] {
        entries
            .try_reserve_exact(declarations.len())
            .map_err(|_| limited("Periodic registration allocation failed"))?;
    }
    // Sample only when installing new schedules. Preserved registrations never
    // call the host clock, change phase, restart a failed task, or discard a tick.
    let needs_clock = declarations.iter().any(|(name, period)| {
        reset
            || !plan
                .retired
                .iter()
                .any(|old| old.name == *name && old.period == *period)
    });
    let now = if needs_clock {
        let executor = registry
            .borrow()
            .executor
            .upgrade()
            .ok_or_else(|| invalid("Periodic executor is closed"))?;
        Some(executor.now())
    } else {
        None
    };
    for (name, period) in declarations {
        if let Some(index) = plan
            .retired
            .iter()
            .position(|old| !reset && old.name == name && old.period == period)
        {
            plan.next.push(plan.retired.remove(index));
            continue;
        }
        let charge = reads::Charge::new(
            registry.borrow().budget.clone(),
            std::mem::size_of::<Registration>()
                + std::mem::size_of::<State>()
                + 8 * std::mem::size_of::<usize>()
                + name.len(),
        )?;
        let state = Rc::new(RefCell::new(State {
            status: Status::Idle,
            generation: 0,
            target: None,
            job: None,
            staged: true,
            waiter: None,
            _charge: charge,
        }));
        start_state(
            registry,
            state.clone(),
            Schedule {
                target,
                module,
                name: &name,
                input: Input::Scalar(0),
                nanoseconds: period,
                periodic: true,
                limits,
            },
            Some((ty, now.unwrap())),
        )
        .map_err(|status| {
            if status == 5 {
                limited("Periodic registration resource limit exceeded")
            } else {
                invalid("Periodic registration is no longer valid")
            }
        })?;
        let entry = Rc::new(Registration {
            name,
            period,
            state,
        });
        plan.added.push(entry.clone());
        plan.next.push(entry);
    }
    Ok(plan)
}

pub(in crate::vm::state_domains) fn status(
    domains: &Domains,
    target: DomainHandle,
    method: &str,
) -> Status {
    let Ok(domain) = domains.get(target) else {
        return target_status(domains, target);
    };
    domain
        .automatic
        .iter()
        .find(|entry| entry.name.rsplit('.').next() == Some(method))
        .map_or(Status::InvalidAccess, |entry| entry.state.borrow().status)
}

pub(in crate::vm::state_domains) fn has_declarations(module: &LoadedModule, ty: &Type) -> bool {
    code::entry_names(module)
        .into_iter()
        .filter(|name| name.starts_with("$every/"))
        .any(|name| {
            module.instance_info(&name).is_ok_and(|info| {
                info.every.is_some()
                    && info.domain_entry
                    && info
                        .signature
                        .params
                        .first()
                        .and_then(Type::reference_type)
                        .is_some_and(|r| r.mutable && code::compatible(&r.target, ty))
            })
        })
}

/// Prepare timers for an unpublished root under the complete directory interval.
/// Workers remain staged and cannot invoke user code until activation.
pub(in crate::vm::state_domains) fn prepare_new(
    domains: &mut Domains,
    registry: &Weak<RefCell<Domains>>,
    target: DomainHandle,
    now: Option<Instant>,
) -> Result<(), Error> {
    let domain = domains.get(target)?;
    let (module, ty, limits) = (domain.module.clone(), domain.ty.clone(), domain.limits);
    let mut declarations = code::entry_names(&module)
        .into_iter()
        .filter(|name| name.starts_with("$every/"))
        .filter_map(|name| {
            let info = module.instance_info(&name).ok()?;
            let period = info.every?;
            (info.domain_entry
                && info
                    .signature
                    .params
                    .first()
                    .and_then(Type::reference_type)
                    .is_some_and(|r| r.mutable && code::compatible(&r.target, &ty)))
            .then_some((name, period, info))
        })
        .collect::<Vec<_>>();
    declarations.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    domains
        .entries
        .get_mut(&target.sequence)
        .unwrap()
        .automatic
        .try_reserve_exact(declarations.len())
        .map_err(|_| limited("Periodic registration allocation failed"))?;
    for (name, period, info) in declarations {
        let now = now.ok_or_else(|| invalid("Missing staged periodic clock"))?;
        let executor = domains
            .executor
            .upgrade()
            .ok_or_else(|| invalid("Periodic executor is closed"))?;
        let delay = Duration::from_nanos(period);
        let due = now
            .checked_add(delay)
            .ok_or_else(|| limited("Periodic deadline overflow"))?;
        let state = Rc::new(RefCell::new(State {
            status: Status::Idle,
            generation: 0,
            target: None,
            job: None,
            staged: true,
            waiter: None,
            _charge: reads::Charge::new(
                domains.budget.clone(),
                std::mem::size_of::<Registration>()
                    + std::mem::size_of::<State>()
                    + 8 * std::mem::size_of::<usize>()
                    + name.len(),
            )?,
        }));
        let charge = reads::Charge::new(
            domains.budget.clone(),
            std::mem::size_of::<Worker>()
                + std::mem::size_of::<CpsFrame>()
                + name.len()
                + info.signature.params.len() * std::mem::size_of::<Type>(),
        )?;
        domains
            .entries
            .get_mut(&target.sequence)
            .unwrap()
            .tasks
            .try_reserve(1)
            .map_err(|_| limited("Periodic task registration failed"))?;
        let generation = source::next_token()?;
        let worker = Worker {
            registry: registry.clone(),
            state: state.clone(),
            generation,
            target,
            name: name.clone(),
            signature: info.signature,
            input: Payload::Scalar(InstanceValue::Unit),
            limits,
            due,
            period: Some(delay),
            request: None,
            _charge: charge,
        };
        let mut frame = CpsFrame::new(0);
        frame.host_context = Box::into_raw(Box::new(worker)).cast();
        frame.host_stop = Some(stop);
        // SAFETY: this zero-slot frame owns Worker until stop; tick and Environment
        // use the same layout as ordinary domain-owned periodic registrations.
        let task = unsafe { Continuation::new(tick, frame, ModuleId(0), Rc::new(())) };
        let group = TaskGroup::new();
        let job = executor
            .spawn_in(&group, task, Some(Rc::new(Environment)))
            .map_err(|_| limited("Periodic task allocation failed"))?;
        let domain = domains.entries.get_mut(&target.sequence).unwrap();
        domain.tasks.insert(job, BoundTask { group, module: 0 });
        {
            let mut state = state.borrow_mut();
            state.target = Some(target);
            state.generation = generation;
            state.status = Status::Active;
            state.job = Some(job);
        }
        domain.automatic.push(Rc::new(Registration {
            name,
            period,
            state,
        }));
    }
    Ok(())
}
pub(in crate::vm::state_domains) fn activate_new(domain: &Domain) {
    for entry in &domain.automatic {
        let waiter = {
            let mut state = entry.state.borrow_mut();
            state.staged = false;
            state.waiter.take()
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }
}
