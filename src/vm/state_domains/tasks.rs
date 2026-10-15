//! One logical domain owner, with separate execution/failure roots per request.
use super::*;
use crate::runtime::domain::RequestStatus;

#[derive(Clone)]
struct Access {
    domains: Weak<RefCell<Domains>>,
    handle: DomainHandle,
    /// The module this request runs; operations may have newer published code.
    code: LoadedModule,
    limits: InterpreterLimits,
    group: TaskGroup,
    accepting: bool,
    root: *const (),
    completion: Option<Rc<RefCell<super::source::Completion>>>,
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
    fn report_completion(&self) -> bool {
        self.completion.is_none()
    }
    fn resume(&self, task: &mut Continuation) -> Result<(), Error> {
        let domains = self
            .domains
            .upgrade()
            .ok_or_else(|| invalid("Domain VM is closed"))?;
        domains.borrow().get(self.handle)?;
        let _guard = self.enter();
        // This request's failure closes only its own executor group. State becomes
        // failed only when a checked exclusive turn fails inside execute().
        let result = task.resume().map_err(Error::from).and_then(|()| {
            if let (Some(completion), Some(value)) = (&self.completion, task.cps_result()) {
                let mut completion = completion.borrow_mut();
                if let Some(output) = &mut completion.output {
                    let words = task
                        .flat_result(output.layout)
                        .ok_or_else(|| invalid("Missing domain request value result"))?;
                    let words = super::values::result_words(&output.ty, words)?;
                    output.words.copy_from_slice(&words);
                }
                completion.result = Some((RequestStatus::Completed, value));
            }
            Ok(())
        });
        if let (Some(completion), Err(error)) = (&self.completion, &result) {
            completion.borrow_mut().result = Some((RequestStatus::from_error(error), 0));
        }
        result
    }
    fn dispose(&self, task: Continuation) {
        let mut stopping = self.clone();
        stopping.accepting = false;
        let _guard = stopping.enter();
        drop(task);
        if let Some(completion) = &self.completion {
            let status = self
                .domains
                .upgrade()
                .map_or(Some(RequestStatus::Closed), |d| {
                    let d = d.borrow();
                    d.entries.get(&self.handle.sequence).map_or(
                        Some(RequestStatus::Closed),
                        |domain| {
                            if domain.closing || d.shutting_down {
                                Some(RequestStatus::Closed)
                            } else if domain.failure.is_some() {
                                Some(RequestStatus::DomainFailed)
                            } else {
                                None
                            }
                        },
                    )
                });
            let mut completion = completion.borrow_mut();
            if let Some(status) = status {
                completion.result = Some((status, 0));
            } else if completion.result.is_none() {
                completion.result = Some((RequestStatus::Cancelled, 0));
            }
            completion.published = true;
            completion.waiter.wake();
        }
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

pub(super) fn access_rejection(
    registry: &Rc<RefCell<Domains>>,
    error: &Error,
) -> access::Rejection {
    match current_access() {
        Ok(current) => access::classify(&registry.borrow(), &[current.handle], error),
        Err(_) => access::Rejection::InvalidAccess,
    }
}

pub(super) fn value_context(
    registry: &Rc<RefCell<Domains>>,
) -> Result<(DomainHandle, LoadedModule), Error> {
    let access = current_access()?;
    let domains = access
        .domains
        .upgrade()
        .ok_or_else(|| invalid("Domain VM is closed"))?;
    if !Rc::ptr_eq(&domains, registry) {
        return Err(invalid("Domain value call belongs to another executor"));
    }
    domains.borrow().get(access.handle)?;
    Ok((access.handle, access.code))
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
        admit(&access, domains.get(access.handle)?, method)?;
        let info = access.code.instance_info(method)?;
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

fn admit(access: &Access, domain: &Domain, method: &str) -> Result<(), Error> {
    if super::code::admits(&access.code, &domain.module, method, false) {
        Ok(())
    } else {
        Err(error(
            ErrorCode::TypeMismatch,
            "Domain operation does not match the code published for this state",
        ))
    }
}

pub(super) fn queue_current_scalar_call(
    method: &str,
    input: Option<u64>,
    write: bool,
) -> Result<DomainRequest, Error> {
    let access = current_access()?;
    let registry = access
        .domains
        .upgrade()
        .ok_or_else(|| invalid("Domain VM is closed"))?;
    let args = {
        let domains = registry.borrow();
        admit(&access, domains.get(access.handle)?, method)?;
        let info = access.code.instance_info(method)?;
        super::source::turn_arguments(&info, input, write)?
    };
    submit(&registry, access.handle, method, &args, access.limits)
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
        spawn(
            &self.domains,
            handle,
            name,
            args,
            self.interpreter_limits(),
            None,
        )
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
        super::admission::shutdown(&self.domains);
        super::outbox::shutdown(&self.domains);
        super::directory::shutdown(&self.domains);
        super::timers::shutdown(&self.domains);
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

pub(super) fn spawn(
    registry: &Rc<RefCell<Domains>>,
    handle: DomainHandle,
    name: &str,
    args: &[InstanceValue],
    limits: InterpreterLimits,
    completion: Option<Rc<RefCell<super::source::Completion>>>,
) -> Result<TaskId, Error> {
    spawn_with(registry, handle, limits, completion, |module| {
        Ok((module.prepare_async(name, args, limits)?, None))
    })
}

pub(super) fn spawn_values(
    registry: &Rc<RefCell<Domains>>,
    handle: DomainHandle,
    name: &str,
    words: &[u64],
    limits: InterpreterLimits,
    completion: Rc<RefCell<super::source::Completion>>,
) -> Result<TaskId, Error> {
    spawn_with(registry, handle, limits, Some(completion), |module| {
        let info = super::source::request_info(module, name)
            .ok_or_else(|| invalid("Unknown domain request entry"))?;
        let input = info
            .signature
            .params
            .first()
            .and_then(crate::core::script::schema)
            .ok_or_else(|| invalid("Invalid domain request input contract"))?;
        let output = crate::core::script::schema(&info.signature.ret)
            .ok_or_else(|| invalid("Invalid domain request result contract"))?;
        module.prepare_script_call(name, &input, &output, words, limits)
    })
}

fn spawn_with(
    registry: &Rc<RefCell<Domains>>,
    handle: DomainHandle,
    limits: InterpreterLimits,
    completion: Option<Rc<RefCell<super::source::Completion>>>,
    prepare: impl FnOnce(&LoadedModule) -> Result<(Continuation, Option<(usize, usize)>), Error>,
) -> Result<TaskId, Error> {
    reap(registry);
    let module = {
        let mut domains = registry.borrow_mut();
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
    let executor = registry
        .borrow()
        .executor
        .upgrade()
        .ok_or_else(|| invalid("Domain executor is closed"))?;
    let (task, layout) = prepare(&module)?;
    if let Some(completion) = &completion {
        if let Some(output) = &mut completion.borrow_mut().output {
            output.layout = layout;
        }
    }
    let group = TaskGroup::new();
    let root = task.root_identity();
    let key = code::module_key(&module);
    let id = executor.spawn_in(
        &group,
        task,
        Some(Rc::new(Access {
            domains: Rc::downgrade(registry),
            handle,
            code: module,
            limits,
            group: group.clone(),
            accepting: true,
            root,
            completion,
        })),
    )?;
    registry
        .borrow_mut()
        .entries
        .get_mut(&handle.sequence)
        .unwrap()
        .tasks
        .insert(id, BoundTask { group, module: key });
    Ok(id)
}
