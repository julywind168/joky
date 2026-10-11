//! One logical domain owner, with separate execution/failure roots per request.
use super::*;

#[derive(Clone)]
struct Access {
    domains: Weak<RefCell<Domains>>,
    handle: DomainHandle,
    limits: InterpreterLimits,
    group: TaskGroup,
    accepting: bool,
    root: *const (),
}
thread_local! {
    static CURRENT: RefCell<Option<Access>> = const { RefCell::new(None) };
}
struct Guard(Option<Access>);
impl Drop for Guard {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            current.replace(self.0.take());
        });
    }
}
impl Access {
    fn enter(&self) -> Guard {
        Guard(CURRENT.with(|current| current.replace(Some(self.clone()))))
    }
}
impl TaskEnvironment for Access {
    fn resume(&self, task: &mut Continuation) -> Result<(), Error> {
        let domains = self
            .domains
            .upgrade()
            .ok_or_else(|| invalid("Domain VM is closed"))?;
        domains.borrow().get(self.handle)?;
        let _guard = self.enter();
        // This request's failure closes only its own executor group. State becomes
        // failed only when a checked exclusive turn fails inside execute().
        task.resume().map_err(Error::from)
    }
    fn dispose(&self, task: Continuation) {
        let mut stopping = self.clone();
        stopping.accepting = false;
        let _guard = stopping.enter();
        drop(task);
    }
}

fn current_access() -> Result<Access, Error> {
    let access = CURRENT
        .with(|current| current.borrow().clone())
        .ok_or_else(|| invalid("No active domain request"))?;
    if crate::runtime::suspension::current_root() != access.root {
        return Err(invalid("Domain access requires its supervised root"));
    }
    if !access.accepting || access.group.is_closed() {
        return Err(invalid("Domain request is stopping"));
    }
    Ok(access)
}

/// Submit from a suspending host adapter in a domain-owned request. The caller
/// must retain the returned token in frame-owned cleanup and register its waiter.
/// No receiver borrow is held between polls; access cannot exceed the root's grant.
pub fn queue_current_domain_call(
    method: &str,
    args: &[InstanceValue],
) -> Result<DomainRequest, Error> {
    let access = current_access()?;
    let domains = access
        .domains
        .upgrade()
        .ok_or_else(|| invalid("Domain VM is closed"))?;
    submit(&domains, access.handle, method, args, access.limits)
}

pub(crate) fn queue_current_i32_call(
    method: &str,
    args: &[InstanceValue],
    write: bool,
) -> Result<DomainRequest, Error> {
    let access = current_access()?;
    let registry = access
        .domains
        .upgrade()
        .ok_or_else(|| invalid("Domain VM is closed"))?;
    {
        let domains = registry.borrow();
        let info = domains.get(access.handle)?.module.instance_info(method)?;
        if info.signature.ret != Type::Int32
            || info
                .signature
                .params
                .first()
                .and_then(Type::reference_type)
                .is_none_or(|r| r.mutable != write)
        {
            return Err(invalid(
                "Domain operation signature does not match the access adapter",
            ));
        }
    }
    submit(&registry, access.handle, method, args, access.limits)
}

impl VmEngine {
    /// All live request roots, instance roots and queued short turns.
    pub fn task_count(&self) -> usize {
        self.executor.task_count()
    }

    /// Register an unpolled scalar request in the domain's pinned module. Every
    /// request has its own fail-fast group but must stop before its owner closes.
    pub fn spawn_domain_task(
        &self,
        handle: DomainHandle,
        name: &str,
        args: &[InstanceValue],
    ) -> Result<TaskId, Error> {
        self.finish_domain_cleanup();
        let module = {
            let mut domains = self.domains.borrow_mut();
            let module = domains.get(handle)?.module.clone();
            domains
                .entries
                .get_mut(&handle.sequence)
                .unwrap()
                .tasks
                .try_reserve(1)
                .map_err(|_| limited("Domain task registry allocation failed"))?;
            module
        };
        let limits = self.interpreter_limits();
        let task = module.prepare_async(name, args, limits)?;
        let group = TaskGroup::new();
        let root = task.root_identity();
        let id = self.executor.spawn_in(
            &group,
            task,
            Some(Rc::new(Access {
                domains: Rc::downgrade(&self.domains),
                handle,
                limits,
                group: group.clone(),
                accepting: true,
                root,
            })),
        )?;
        self.domains
            .borrow_mut()
            .entries
            .get_mut(&handle.sequence)
            .unwrap()
            .tasks
            .insert(id, group);
        Ok(id)
    }
    pub fn cancel_domain_task(&self, id: TaskId) -> bool {
        let owns = self
            .domains
            .borrow()
            .entries
            .values()
            .any(|domain| domain.tasks.contains_key(&id));
        if !owns {
            return false;
        }
        let cancelled = self.executor.cancel(id);
        self.finish_domain_cleanup();
        cancelled
    }
    pub fn domain_task_count(&self, handle: DomainHandle) -> Result<usize, Error> {
        self.finish_domain_cleanup();
        Ok(self.domains.borrow().get(handle)?.tasks.len())
    }
    pub(in crate::vm) fn finish_domain_cleanup(&self) {
        reap(&self.domains);
    }
    pub(in crate::vm) fn shutdown_domains(&self) {
        let ids: Vec<_> = {
            let mut domains = self.domains.borrow_mut();
            domains.shutting_down = true;
            domains.entries.keys().copied().collect()
        };
        for id in ids {
            close(&self.domains, id);
        }
    }
}
