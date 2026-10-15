//! Scalar state domains and independently supervised requests on the host executor.
use super::{engine::LoadedModule, InstanceValue, InterpreterLimits, VmEngine};
use crate::{
    core::{instance, ir::FunctionInfo},
    runtime::{
        arena::{Arena, Budget},
        cps::{CpsFrame, DONE},
        executor::{current_waker, TaskEnvironment, TaskGroup, TaskId, TaskWaker, WeakExecutor},
        suspension::{Continuation, ModuleId},
    },
    Error, ErrorCode, ErrorPhase, Type,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::atomic::{AtomicU64, Ordering},
};

mod access;
mod admission;
mod code;
mod commits;
pub(crate) mod diagnostics;
mod directory;
pub(crate) mod migration;
mod outbox;
mod pairs;
mod reads;
mod storage;
mod timers;
mod values;
use storage::Root;
pub(crate) mod source;
mod tasks;
pub use tasks::queue_current_domain_call;
pub(crate) use tasks::queue_current_i32_call;

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
    /// Invalidate immediately; false means an active poll/stop still must unwind.
    pub fn close(&self) -> bool {
        self.domains
            .upgrade()
            .is_none_or(|domains| close(&domains, self.handle.sequence))
    }
    pub fn is_closed(&self) -> bool {
        self.domains
            .upgrade()
            .is_none_or(|domains| !domains.borrow().entries.contains_key(&self.handle.sequence))
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
            let job = {
                let mut domains = domains.borrow_mut();
                domains.requests.get_mut(&sequence).and_then(|request| {
                    if request.outcome.is_some() {
                        return None;
                    }
                    request.finish(DomainOutcome::Cancelled);
                    request.job.take()
                })
            };
            cancel_job(&domains, job);
        }
    }
    /// Register the current root for an external wake. There is one waiter per
    /// request; a second root or an unrelated executor cannot steal its result.
    pub fn register_current_wait(&self) -> Result<(), Error> {
        let waker =
            current_waker().ok_or_else(|| invalid("Domain wait requires an executor task"))?;
        let domains = self
            .domains
            .upgrade()
            .ok_or_else(|| invalid("Domain VM is closed"))?;
        let mut domains = domains.borrow_mut();
        let executor = domains
            .executor
            .upgrade()
            .ok_or_else(|| invalid("Domain executor is closed"))?;
        if !waker.belongs_to(&executor) {
            return Err(invalid("Domain wait belongs to another executor"));
        }
        let request = self
            .sequence
            .and_then(|id| domains.requests.get_mut(&id))
            .ok_or_else(|| invalid("Domain result was already collected"))?;
        if request
            .waiter
            .as_ref()
            .is_some_and(|old| !old.same_task(&waker))
        {
            return Err(invalid("Domain request already has another waiter"));
        }
        if request.outcome.is_some() {
            waker.wake();
        } else {
            request.waiter = Some(waker);
        }
        Ok(())
    }
    /// None means pending or already collected. VM destruction yields Closed once.
    pub fn take_result(&mut self) -> Option<DomainOutcome> {
        self.take_observed_result().map(|(outcome, _)| outcome)
    }
    fn take_access_result(&mut self) -> Option<(DomainOutcome, Option<access::Rejection>)> {
        let rejection = self.domains.upgrade().and_then(|domains| {
            domains
                .borrow()
                .requests
                .get(&self.sequence?)
                .and_then(|r| r.access_error)
        });
        self.take_result().map(|outcome| {
            let rejection = if matches!(outcome, DomainOutcome::Closed) {
                Some(access::Rejection::Closed)
            } else {
                rejection
            };
            (outcome, rejection)
        })
    }
    // The revision is captured with the value by execute(), never when collected.
    fn take_observed_result(&mut self) -> Option<(DomainOutcome, u64)> {
        self.take_snapshot_result()
            .map(|(outcome, revision, _)| (outcome, revision))
    }
    fn take_snapshot_result(&mut self) -> Option<(DomainOutcome, u64, Option<reads::ReadSet>)> {
        let sequence = self.sequence?;
        let Some(domains) = self.domains.upgrade() else {
            self.sequence = None;
            return Some((DomainOutcome::Closed, 0, None));
        };
        let mut domains = domains.borrow_mut();
        domains.requests.get(&sequence)?.outcome.as_ref()?;
        let request = domains.remove_request(sequence)?;
        self.sequence = None;
        request
            .outcome
            .map(|outcome| (outcome, request.observed_revision, request.observed_reads))
    }
}
impl Drop for DomainRequest {
    fn drop(&mut self) {
        if let (Some(domains), Some(sequence)) = (self.domains.upgrade(), self.sequence) {
            let job = domains
                .borrow_mut()
                .remove_request(sequence)
                .and_then(|request| request.job);
            cancel_job(&domains, job);
        }
    }
}

struct Domain {
    root: Option<Root>,
    module: LoadedModule,
    source_module: usize,
    source_token: u64,
    ty: Type,
    /// Incremented whenever compatible code is published for this root.
    code: u64,
    revision: u64,
    schema: u64,
    failure: Option<Error>,
    closing: bool,
    tasks: HashMap<TaskId, BoundTask>,
}
struct BoundTask {
    group: TaskGroup,
    module: usize,
}
struct Request {
    domain: u64,
    work: Work,
    commit_status: Option<commits::Status>,
    access_error: Option<access::Rejection>,
    charge: usize,
    outcome: Option<DomainOutcome>,
    observed_revision: u64,
    second_revision: u64,
    observed_reads: Option<reads::ReadSet>,
    set_snapshot: Option<Box<commits::sets::Snapshot>>,
    set_revisions: Vec<(u64, u64)>,
    job: Option<TaskId>,
    waiter: Option<TaskWaker>,
}
enum Work {
    Turn {
        method: String,
        info: FunctionInfo,
        code: u64,
        words: Vec<u64>,
        limits: InterpreterLimits,
    },
    Commit(Option<commits::Candidate>),
    CommitPair(Option<Box<commits::Bundle>>),
    Pair(pairs::Pair),
    SnapshotSet(Box<commits::sets::Accesses>),
    CommitSet(Option<Box<commits::sets::Proposal>>),
}
impl Request {
    fn touches(&self, domain: u64) -> bool {
        self.domain == domain
            || matches!(&self.work, Work::SnapshotSet(set) if set.touches(domain))
            || matches!(&self.work, Work::CommitSet(Some(set)) if set.touches(domain))
            || matches!(&self.work, Work::Pair(pair) if pair.handles.iter().any(|h| h.sequence == domain))
            || matches!(&self.work, Work::Commit(Some(candidate)) if candidate.reads.touches(domain))
            || matches!(&self.work, Work::CommitPair(Some(bundle)) if bundle.touches(domain))
    }
    fn finish(&mut self, outcome: DomainOutcome) {
        if matches!(
            self.work,
            Work::Commit(_) | Work::CommitPair(_) | Work::CommitSet(_)
        ) {
            if self.commit_status.is_none() {
                self.commit_status = Some(match &outcome {
                    DomainOutcome::Cancelled => commits::Status::Cancelled,
                    DomainOutcome::Closed => commits::Status::Closed,
                    _ => commits::Status::DomainFailed,
                });
            }
            match &mut self.work {
                Work::Commit(candidate) => {
                    candidate.take();
                }
                Work::CommitPair(bundle) => {
                    bundle.take();
                }
                Work::CommitSet(set) => {
                    set.take();
                }
                _ => unreachable!(),
            }
        }
        self.outcome = Some(outcome);
        if let Some(waiter) = self.waiter.take() {
            waiter.wake();
        }
    }
}
pub(crate) struct Domains {
    identity: u64,
    timers: HashMap<u64, Weak<RefCell<timers::State>>>,
    directories: HashMap<u64, Weak<RefCell<directory::Table>>>,
    outboxes: HashMap<u64, Weak<RefCell<outbox::Mailbox>>>,
    admissions: HashMap<u64, Weak<RefCell<admission::Pool>>>,
    next_domain: u64,
    next_request: u64,
    entries: HashMap<u64, Domain>,
    requests: HashMap<u64, Request>,
    executor: WeakExecutor,
    shutting_down: bool,
    request_bytes: usize,
    source_requests: usize,
    budget: Rc<Budget>,
    limits: DomainLimits,
}
impl Domains {
    pub(crate) fn new(executor: WeakExecutor) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let limits = DomainLimits::default();
        Self {
            identity: NEXT
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect("State domain registry identities exhausted"),
            timers: HashMap::new(),
            directories: HashMap::new(),
            outboxes: HashMap::new(),
            admissions: HashMap::new(),
            next_domain: 1,
            next_request: 1,
            entries: HashMap::new(),
            requests: HashMap::new(),
            executor,
            shutting_down: false,
            request_bytes: 0,
            source_requests: 0,
            budget: Rc::new(Budget {
                bytes: Cell::new(0),
                limit: Cell::new(limits.max_state_bytes),
            }),
            limits,
        }
    }
}
impl Domains {
    #[cfg(test)]
    pub(crate) fn request_tasks(&self) -> Vec<TaskId> {
        self.entries
            .values()
            .flat_map(|domain| domain.tasks.keys().copied())
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn usage(&self) -> (usize, usize, usize, usize) {
        (
            self.entries.len(),
            self.requests.len() + self.source_requests,
            self.request_bytes,
            self.budget.bytes.get(),
        )
    }
    pub(crate) fn limits(&self) -> DomainLimits {
        self.limits
    }
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
            && self.requests.is_empty()
            && self.source_requests == 0
            && self.admissions.is_empty()
            && self.outboxes.is_empty()
            && self.directories.is_empty()
            && self.timers.is_empty()
    }
    fn get(&self, handle: DomainHandle) -> Result<&Domain, Error> {
        if self.shutting_down {
            return Err(invalid("Domain VM is shutting down"));
        }
        if handle.registry != self.identity {
            return Err(invalid("State handle belongs to another VM"));
        }
        let domain = self
            .entries
            .get(&handle.sequence)
            .ok_or_else(|| invalid("State domain is closed"))?;
        if domain.closing {
            return Err(invalid("State domain is closing"));
        }
        if let Some(failure) = &domain.failure {
            return Err(failure.clone());
        }
        Ok(domain)
    }
    fn remove_request(&mut self, sequence: u64) -> Option<Request> {
        let request = self.requests.remove(&sequence)?;
        self.request_bytes -= request.charge;
        Some(request)
    }
    fn finish_pending(&mut self, domain: u64, outcome: DomainOutcome) -> Vec<TaskId> {
        self.requests
            .values_mut()
            .filter_map(|request| {
                if !request.touches(domain) || request.outcome.is_some() {
                    return None;
                }
                request.access_error = match outcome {
                    DomainOutcome::Closed => Some(access::Rejection::Closed),
                    DomainOutcome::Failed(_) => Some(access::Rejection::DomainFailed),
                    _ => None,
                };
                request.finish(outcome.clone());
                request.job.take()
            })
            .collect()
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
            || domains.requests.len() + domains.source_requests > limits.max_requests
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

    /// Construct an eligible flat record through a compiler-proved host-free entry.
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
        create(
            &self.domains,
            module,
            constructor,
            args,
            self.interpreter_limits(),
        )
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
        submit(
            &self.domains,
            handle,
            method,
            args,
            self.interpreter_limits(),
        )
    }
}

fn submit(
    registry: &Rc<RefCell<Domains>>,
    handle: DomainHandle,
    method: &str,
    args: &[InstanceValue],
    limits: InterpreterLimits,
) -> Result<DomainRequest, Error> {
    let domains = registry.borrow();
    let domain = domains.get(handle)?;
    let info = domain.module.instance_info(method)?;
    if !info.domain_entry || instance::receiver(&info) != Some(&domain.ty) {
        return Err(Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::TypeMismatch,
            "Domain operation requires a proved entry for this exact state type",
        ));
    }
    let mutable = info.signature.params[0].reference_type().unwrap().mutable;
    if mutable && !handle.writable {
        return Err(Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::InvalidCall,
            "Read-only state handle cannot authorize a write",
        ));
    }
    let code = domain.code;
    let mut words = arguments(&info.signature.params[1..], args)?;
    words
        .try_reserve(1)
        .map_err(|_| limited("Domain input allocation failed"))?;
    words.insert(0, 0);
    let extra = words
        .len()
        .checked_mul(std::mem::size_of::<u64>())
        .and_then(|n| n.checked_add(method.len()))
        .ok_or_else(|| limited("Domain request size overflow"))?;
    drop(domains);
    enqueue(
        registry,
        handle.sequence,
        Work::Turn {
            method: method.to_owned(),
            info,
            code,
            words,
            limits,
        },
        extra,
    )
}

fn enqueue(
    registry: &Rc<RefCell<Domains>>,
    domain: u64,
    work: Work,
    extra: usize,
) -> Result<DomainRequest, Error> {
    let mut domains = registry.borrow_mut();
    if domains.requests.len() + domains.source_requests >= domains.limits.max_requests {
        return Err(limited("Domain request count limit exceeded"));
    }
    let charge = std::mem::size_of::<Request>()
        .checked_add(extra)
        .ok_or_else(|| limited("Domain request size overflow"))?;
    let bytes = domains
        .request_bytes
        .checked_add(charge)
        .filter(|n| *n <= domains.limits.max_request_bytes)
        .ok_or_else(|| limited("Domain request byte limit exceeded"))?;
    let sequence = domains.next_request;
    let next = sequence
        .checked_add(1)
        .ok_or_else(|| limited("Domain request identities exhausted"))?;
    domains
        .requests
        .try_reserve(1)
        .map_err(|_| limited("Domain request registry allocation failed"))?;
    let executor = domains
        .executor
        .upgrade()
        .ok_or_else(|| invalid("Domain executor is closed"))?;
    // A native trampoline runs one already-checked turn. The environment holds
    // only a weak registry reference, and no source receiver is captured.
    let task =
        unsafe { Continuation::new(complete_turn, CpsFrame::new(0), ModuleId(0), Rc::new(())) };
    let job = executor.spawn_in(
        &TaskGroup::new(),
        task,
        Some(Rc::new(Turn {
            domains: Rc::downgrade(registry),
            sequence,
        })),
    )?;
    domains.requests.insert(
        sequence,
        Request {
            domain,
            work,
            commit_status: None,
            access_error: None,
            charge,
            outcome: None,
            observed_revision: 0,
            second_revision: 0,
            observed_reads: None,
            set_snapshot: None,
            set_revisions: Vec::new(),
            job: Some(job),
            waiter: None,
        },
    );
    domains.request_bytes = bytes;
    domains.next_request = next;
    Ok(DomainRequest {
        domains: Rc::downgrade(registry),
        sequence: Some(sequence),
    })
}

struct Turn {
    domains: Weak<RefCell<Domains>>,
    sequence: u64,
}
// Private zero-slot continuation; no foreign frame pointers or code owners escape.
unsafe extern "C" fn complete_turn(frame: *mut CpsFrame) -> i32 {
    (*frame).state = DONE;
    (*frame).result = 0;
    0
}
impl TaskEnvironment for Turn {
    fn report_completion(&self) -> bool {
        false
    }
    fn resume(&self, task: &mut Continuation) -> Result<(), Error> {
        if let Some(registry) = self.domains.upgrade() {
            execute(&registry, self.sequence);
        }
        task.resume().map_err(Error::from)
    }
    fn dispose(&self, task: Continuation) {
        drop(task);
    }
}
fn execute(registry: &Rc<RefCell<Domains>>, sequence: u64) {
    // Sample the actual clock at the final publication boundary, not the
    // executor's earlier timer scan. A reentrant clock may cancel or close the
    // request; inspect all control state only after the callback returns.
    let clock = {
        let domains = registry.borrow();
        domains
            .requests
            .get(&sequence)
            .filter(|r| r.outcome.is_none())
            .and_then(|r| match &r.work {
                Work::Commit(Some(candidate)) if candidate.deadline.is_some() => {
                    domains.executor.upgrade()
                }
                Work::CommitPair(Some(bundle)) if bundle.timed() => domains.executor.upgrade(),
                Work::CommitSet(Some(set)) if set.timed() => domains.executor.upgrade(),
                _ => None,
            })
    };
    let now = clock.map(|executor| executor.now());
    let mut domains = registry.borrow_mut();
    let Some(mut request) = domains.requests.remove(&sequence) else {
        return;
    };
    if request.outcome.is_some() {
        domains.requests.insert(sequence, request);
        return;
    }
    if matches!(
        request.work,
        Work::Commit(_) | Work::CommitPair(_) | Work::CommitSet(_)
    ) {
        commits::execute(&mut domains, &mut request, now);
        request.job = None;
        domains.requests.insert(sequence, request);
        return;
    }
    if matches!(request.work, Work::SnapshotSet(_)) {
        commits::sets::execute_snapshot(&mut domains, &mut request);
        request.job = None;
        domains.requests.insert(sequence, request);
        return;
    }
    if let Work::Pair(pair) = &mut request.work {
        let reads = if pair.readonly() && pair.output.as_ref().is_none_or(|output| output.snapshot)
        {
            reads::ReadSet::capture_many(&domains, &pair.handles).map(Some)
        } else {
            Ok(None)
        };
        let (outcome, jobs, groups) = match reads {
            Ok(reads) => {
                let result = pairs::execute(&mut domains, pair, &mut request.access_error);
                if matches!(result.0, DomainOutcome::Value(_)) {
                    request.observed_reads = reads;
                }
                result
            }
            Err(error) => {
                request.access_error = Some(access::classify(&domains, &pair.handles, &error));
                (DomainOutcome::Failed(error), Vec::new(), Vec::new())
            }
        };
        request.job = None;
        request.finish(outcome);
        domains.requests.insert(sequence, request);
        let executor = domains.executor.upgrade();
        drop(domains);
        if let Some(executor) = executor {
            for job in jobs {
                executor.cancel(job);
            }
            for group in groups {
                executor.cancel_group(&group);
            }
        }
        return;
    }
    let Work::Turn {
        method,
        info,
        code: generation,
        words,
        limits,
    } = &mut request.work
    else {
        unreachable!()
    };
    let identity = domains.identity;
    let domain = domains.entries.get_mut(&request.domain).unwrap();
    // A turn queued before a code publication runs the published code only if
    // its already-encoded input and expected result keep the same meaning.
    let rebound = if *generation == domain.code {
        Ok(())
    } else {
        match domain.module.instance_info(method) {
            Ok(current)
                if current.domain_entry
                    && (instance::receiver(&current) == Some(&domain.ty)
                        || (crate::core::state_domain::value_eligible(&current)
                            && instance::scalar(&current.signature.ret)
                            && current.signature.params[0]
                                .reference_type()
                                .is_some_and(|r| r.target == domain.ty)))
                    && code::compatible_entry(info, &current) =>
            {
                *info = current;
                *generation = domain.code;
                Ok(())
            }
            _ => Err(error(
                ErrorCode::TypeMismatch,
                "Domain operation changed its signature after code publication",
            )),
        }
    };
    let mutable = info.signature.params[0].reference_type().unwrap().mutable;
    let next = domain.revision.checked_add(u64::from(mutable));
    let outcome = if let Err(error) = rebound {
        request.access_error = Some(access::Rejection::Incompatible);
        DomainOutcome::Failed(error)
    } else if let Some(next) = next {
        let result = domain.root.as_mut().unwrap().invoke(
            &domain.module,
            &domain.ty,
            method,
            info,
            words,
            *limits,
        );
        match result {
            Ok(value) => {
                domain.revision = next;
                request.observed_revision = next;
                request.observed_reads = Some(reads::ReadSet::single(
                    DomainHandle {
                        registry: identity,
                        sequence: request.domain,
                        writable: false,
                    },
                    next,
                    domain.code,
                    domain.schema,
                ));
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
        request.access_error = Some(access::Rejection::ResourceLimit);
        DomainOutcome::Failed(limited("State domain revision exhausted"))
    };
    let failed = domain.failure.is_some();
    let groups: Vec<_> = if failed {
        domain
            .tasks
            .values()
            .map(|task| task.group.clone())
            .collect()
    } else {
        Vec::new()
    };
    let jobs = if failed {
        domains.finish_pending(request.domain, outcome.clone())
    } else {
        Vec::new()
    };
    words[0] = 0;
    request.job = None;
    request.finish(outcome);
    domains.requests.insert(sequence, request);
    let executor = domains.executor.upgrade();
    drop(domains);
    if let Some(executor) = executor {
        for id in jobs {
            executor.cancel(id);
        }
        for group in groups {
            executor.cancel_group(&group);
        }
    }
}
fn cancel_job(registry: &Rc<RefCell<Domains>>, job: Option<TaskId>) {
    let executor = registry.borrow().executor.upgrade();
    if let (Some(executor), Some(job)) = (executor, job) {
        executor.cancel(job);
    }
}
fn close(registry: &Rc<RefCell<Domains>>, sequence: u64) -> bool {
    let (executor, jobs, groups) = {
        let mut domains = registry.borrow_mut();
        let Some(domain) = domains.entries.get_mut(&sequence) else {
            return true;
        };
        domain.closing = true;
        let groups: Vec<_> = domain
            .tasks
            .values()
            .map(|task| task.group.clone())
            .collect();
        let jobs = domains.finish_pending(sequence, DomainOutcome::Closed);
        (domains.executor.upgrade(), jobs, groups)
    };
    if let Some(executor) = executor {
        for job in jobs {
            executor.cancel(job);
        }
        for group in groups {
            executor.cancel_group(&group);
        }
    }
    reap(registry);
    !registry.borrow().entries.contains_key(&sequence)
}
pub(crate) fn reap(registry: &Rc<RefCell<Domains>>) {
    let mut domains = registry.borrow_mut();
    let executor = domains.executor.upgrade();
    domains.entries.retain(|_, domain| {
        domain.tasks.retain(|id, task| {
            task.group.running() != 0 || executor.as_ref().is_some_and(|e| e.contains(*id))
        });
        !domain.closing || !domain.tasks.is_empty()
    });
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

fn create(
    registry: &Rc<RefCell<Domains>>,
    module: LoadedModule,
    constructor: &str,
    args: &[InstanceValue],
    limits: InterpreterLimits,
) -> Result<DomainOwner, Error> {
    let info = module.instance_info(constructor)?;
    if !info.domain_entry || !instance::constructor(&info) {
        return Err(invalid(
            "Domain constructor requires a proved flat state entry",
        ));
    }
    let words = arguments(&info.signature.params, args)?;
    create_words(registry, module, constructor, info, &words, limits)
}

fn create_words(
    registry: &Rc<RefCell<Domains>>,
    module: LoadedModule,
    constructor: &str,
    info: FunctionInfo,
    words: &[u64],
    limits: InterpreterLimits,
) -> Result<DomainOwner, Error> {
    if !info.domain_entry
        || !(instance::constructor(&info) || crate::core::state_domain::constructor_eligible(&info))
    {
        return Err(invalid("Domain constructor requires a proved state entry"));
    }
    let mut domains = registry.borrow_mut();
    if domains.shutting_down {
        return Err(invalid("Domain VM is shutting down"));
    }
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
    let root = Root::construct(
        &module,
        &info,
        constructor,
        words,
        limits,
        domains.budget.clone(),
    )?;
    domains.entries.insert(
        sequence,
        Domain {
            root: Some(root),
            module,
            source_module: 0,
            source_token: source::next_token()?,
            ty: info.signature.ret,
            code: 0,
            revision: 0,
            schema: source::next_token()?,
            failure: None,
            closing: false,
            tasks: HashMap::new(),
        },
    );
    domains.next_domain = next;
    Ok(DomainOwner {
        domains: Rc::downgrade(registry),
        handle: DomainHandle {
            registry: domains.identity,
            sequence,
            writable: true,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{state_domain_tests::SOURCE, VmMode};

    #[test]
    fn state_domains_snapshot_revision_is_captured_before_later_queued_writes() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let mut read = vm
                .queue_domain_call(owner.read_handle(), "read", &[])
                .unwrap();
            let mut write = vm
                .queue_domain_call(owner.write_handle(), "add", &[InstanceValue::Int32(5)])
                .unwrap();
            vm.poll_tasks(2).unwrap();
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
            owner.close();
            assert!(matches!(
                read.take_observed_result(),
                Some((DomainOutcome::Value(InstanceValue::Int32(7)), 0))
            ));
            assert!(matches!(
                write.take_observed_result(),
                Some((DomainOutcome::Value(InstanceValue::Int32(12)), 1))
            ));
            assert!(read.take_observed_result().is_none());
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }

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
            assert!(vm.domains.borrow().requests.is_empty());
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
            vm.poll_tasks(10).unwrap();
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
                            1 => {
                                owner.close();
                            }
                            2 => {
                                vm.poll_tasks(1).unwrap();
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
                    assert!(domains.requests.values().all(|r| r.job.is_none()));
                    assert_eq!(domains.request_bytes, 0);
                    assert_eq!(domains.budget.bytes.get(), 0);
                }
            }
        }
    }
}
