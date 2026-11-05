//! Control-thread grants and publication; workers own checked computation only.
pub(crate) mod create;
use super::*;
use crate::runtime::parallel::pool;
use std::{collections::BTreeMap, sync::Arc, task::Poll};
use storage::parallel::{Computation, Creation, InputRoot};

pub(super) enum Code {
    Native(crate::backends::jit::FrozenEntry),
    Interpreted {
        entry: crate::backends::interp::FrozenEntry,
        name: String,
    },
}
impl Code {
    fn freeze(module: &LoadedModule, name: &str) -> Result<Self, Error> {
        match module {
            LoadedModule::Jit(module) => Ok(Self::Native(module.freeze_domain(name)?)),
            LoadedModule::Interpreter(module) => Ok(Self::Interpreted {
                entry: module.freeze_domain(name)?,
                name: name.into(),
            }),
        }
    }
    pub(super) fn invoke(
        &self,
        input: &[u64],
        limits: InterpreterLimits,
        arena: &mut Arena,
    ) -> Result<Vec<u64>, Error> {
        pool::check_cancelled()?;
        match self {
            Self::Interpreted { entry, name } => {
                let module = entry.instantiate()?;
                crate::backends::interp::call_instance(&module, name, input, limits, arena)
            }
            Self::Native(entry) => {
                let mut output = vec![0; entry.info.signature.ret.scalar_types().len().max(1)];
                let call: unsafe extern "C" fn(*const u64, *mut u64, *mut Arena) =
                    unsafe { std::mem::transmute(entry.entry) };
                crate::runtime::error::checked(|| unsafe {
                    call(input.as_ptr(), output.as_mut_ptr(), arena)
                })
                .map_err(|error| error.with_sources(&entry.sources))?;
                if entry.info.signature.ret.is_aggregate() {
                    output.truncate(entry.info.signature.ret.scalar_types().len());
                }
                Ok(output)
            }
        }
    }
}
struct Context {
    spawner: pool::DomainSpawner,
    wake: TaskWaker,
    registries: Vec<Rc<RefCell<Domains>>>,
    barriers: usize,
    waiters: Vec<TaskWaker>,
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }
pub(crate) struct Scope;
impl Scope {
    pub(crate) fn enter(spawner: pool::DomainSpawner, wake: TaskWaker) -> Self {
        CONTEXT.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Context {
                spawner,
                wake,
                registries: Vec::new(),
                barriers: 0,
                waiters: Vec::new(),
            });
        });
        Self
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let context = CONTEXT.with(|slot| slot.borrow_mut().take().unwrap());
        // Normal shutdown drains asynchronously. If the control driver unwinds,
        // cancel pure computations and retain their code until worker cleanup.
        for registry in &context.registries {
            for flight in registry.borrow().parallel.flights.values() {
                flight.handle.cancel();
            }
        }
        for registry in &context.registries {
            let flights = std::mem::take(&mut registry.borrow_mut().parallel.flights);
            for flight in flights.into_values() {
                let _ = flight.handle.wait();
            }
        }
    }
}
#[derive(Default)]
pub(super) struct Coordinator {
    pending: BTreeMap<u64, Attempt>,
    next_attempt: u64,
    flights: BTreeMap<u64, Flight>,
}
struct Attempt {
    order: u64,
    retries: usize,
}
type Access = Vec<(u64, bool, bool)>;
struct Flight {
    access: Access,
    versions: Vec<(DomainHandle, u64, u64, u64)>,
    handle: pool::Handle,
    staged: Option<pairs::paths::Staged>,
    // Includes local dependencies of statically embedded/AOT module entries.
    _module: LoadedModule,
    _charge: reads::Charge,
}
struct Computed {
    roots: Vec<(u64, Arc<Root>)>,
    output: Vec<u64>,
    batch: Option<outbox::PendingBatch>,
    accepted: bool,
    _charge: reads::Charge,
}
struct Job {
    compute: Option<Computation>,
    identity: u64,
    limits: DomainLimits,
    #[cfg(test)]
    method: String,
}
impl pool::Job for Job {
    fn poll(&mut self) -> Poll<Result<pool::Output, Error>> {
        let mut compute = self.compute.take().unwrap();
        let session =
            outbox::Session::enter_worker(self.identity, compute.budget.clone(), self.limits);
        let result = compute.invoke().and_then(|output| {
            #[cfg(test)]
            tests::completed_computation(&self.method)?;
            pool::check_cancelled()?;
            let accepted = pairs::transactions::accepts(&compute.info.signature.ret, &output);
            let output = values::result_words(&compute.info.signature.ret, output)?;
            compute.charge.resize(
                std::mem::size_of::<Computed>()
                    + compute.roots.len() * std::mem::size_of::<(u64, Arc<Root>)>()
                    + output.capacity() * 8,
            )?;
            let roots = compute
                .roots
                .into_iter()
                .filter_map(|(id, _, root)| match root {
                    InputRoot::Write(root) => Some((id, root)),
                    InputRoot::Read(_) | InputRoot::Alias(_) => None,
                    InputRoot::New => unreachable!(),
                })
                .collect();
            Ok(Box::new(Computed {
                roots,
                output,
                batch: session.finish_worker(),
                accepted,
                _charge: compute.charge,
            }) as pool::Output)
        });
        Poll::Ready(result)
    }
}
fn accesses(domains: &Domains, request: &Request) -> Result<Access, access::Rejection> {
    match &request.work {
        Work::Pair(pair) if pair.method.starts_with("$when/") => {
            if let Some(paths) = &pair.paths {
                return paths.accesses(domains);
            }
            Ok(pair
                .handles
                .iter()
                .zip(&pair.info.signature.params)
                .map(|(h, t)| (h.sequence, false, t.reference_type().unwrap().mutable))
                .collect())
        }
        // Legacy turns, snapshots and proposals share a conservative global gate.
        _ => Ok(vec![(0, false, true)]),
    }
}
fn conflicts(a: &Access, b: &Access) -> bool {
    a.iter().any(|&(id, dir, write)| {
        b.iter().any(|&(other, other_dir, other_write)| {
            id == 0 || other == 0 || (id == other && dir == other_dir && (write || other_write))
        })
    })
}
pub(super) fn dispatch(registry: &Rc<RefCell<Domains>>, sequence: u64) -> bool {
    let enabled = CONTEXT.with(|slot| {
        let mut context = slot.borrow_mut();
        let Some(context) = context.as_mut() else {
            return false;
        };
        if !context.registries.iter().any(|r| Rc::ptr_eq(r, registry)) {
            context.registries.push(registry.clone());
        }
        true
    });
    if !enabled {
        return false;
    }
    let mut domains = registry.borrow_mut();
    if let Some(request) = domains.requests.get_mut(&sequence) {
        request.job = None;
        if request.outcome.is_none() {
            match domains.parallel.next_attempt.checked_add(1) {
                Some(order) => {
                    domains.parallel.next_attempt = order;
                    domains
                        .parallel
                        .pending
                        .insert(sequence, Attempt { order, retries: 0 });
                }
                None => domains
                    .requests
                    .get_mut(&sequence)
                    .unwrap()
                    .finish(DomainOutcome::Failed(limited(
                        "Domain attempt identities exhausted",
                    ))),
            }
        }
    }
    drop(domains);
    CONTEXT.with(|slot| slot.borrow().as_ref().unwrap().wake.wake());
    true
}
/// Synchronous host APIs cannot suspend for a grant. Reject before changing
/// anything; the asynchronous Script source path drains through gate().
pub(super) fn ensure_idle(domains: &Domains) -> Result<(), Error> {
    if domains.parallel.flights.is_empty() {
        Ok(())
    } else {
        Err(error(ErrorCode::InvalidState, "Domain computations must drain before synchronous publication, migration or limit changes"))
    }
}
pub(crate) fn busy() -> bool {
    CONTEXT.with(|slot| {
        slot.borrow().as_ref().is_some_and(|c| {
            c.registries
                .iter()
                .any(|r| !r.borrow().parallel.flights.is_empty())
        })
    })
}
pub(crate) fn drive() {
    let registries = CONTEXT.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|c| c.registries.clone())
            .unwrap_or_default()
    });
    for registry in &registries {
        drive_registry(registry);
    }
    if !busy() {
        let waiters = CONTEXT.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .map(|c| std::mem::take(&mut c.waiters))
                .unwrap_or_default()
        });
        for waker in waiters {
            waker.wake();
        }
    }
    CONTEXT.with(|slot| {
        if let Some(c) = slot.borrow_mut().as_mut() {
            c.registries.retain(|r| {
                let d = r.borrow();
                !d.parallel.flights.is_empty() || !d.parallel.pending.is_empty()
            });
        }
    });
}
fn drive_registry(registry: &Rc<RefCell<Domains>>) {
    let (spawner, wake, barrier) = CONTEXT.with(|slot| {
        let c = slot.borrow();
        let c = c.as_ref().unwrap();
        (c.spawner.clone(), c.wake.clone(), c.barriers != 0)
    });
    let executor = { registry.borrow().executor.upgrade() };
    let now = executor.map(|executor| executor.now());
    let mut domains = registry.borrow_mut();
    let cancelled = spawner.cancelled();
    let completed: Vec<_> = domains
        .parallel
        .flights
        .iter()
        .filter_map(|(&id, flight)| {
            if domains
                .requests
                .get(&id)
                .is_none_or(|r| r.outcome.is_some())
                || domains.shutting_down
                || cancelled
            {
                flight.handle.cancel();
            }
            flight.handle.cleaned().then_some(id)
        })
        .collect();
    let mut cancel_jobs = Vec::new();
    for id in completed {
        let mut flight = domains.parallel.flights.remove(&id).unwrap();
        let result = flight.handle.take().unwrap().and_then(|output| {
            output
                .downcast::<Computed>()
                .map(|value| *value)
                .map_err(|_| invalid("Invalid domain worker result"))
        });
        if let Some(mut request) = domains.requests.remove(&id) {
            if request.outcome.is_none() {
                let result = if cancelled {
                    DomainOutcome::Cancelled
                } else {
                    finish(&mut domains, &mut request, &mut flight, result)
                };
                request.finish(result);
            }
            domains.requests.insert(id, request);
        }
        if let Some(staged) = flight.staged.take() {
            cancel_jobs.extend(staged.discard(&mut domains));
        }
    }
    let mut pending: Vec<_> = domains
        .parallel
        .pending
        .iter()
        .map(|(&id, attempt)| (attempt.order, id))
        .collect();
    pending.sort_unstable();
    let mut older = Vec::new();
    let mut local = Vec::new();
    for (_, id) in pending {
        let Some(mut request) = domains.requests.remove(&id) else {
            domains.parallel.pending.remove(&id);
            continue;
        };
        if request.outcome.is_some() || domains.shutting_down || cancelled {
            domains.parallel.pending.remove(&id);
            if request.outcome.is_none() {
                request.finish(if cancelled {
                    DomainOutcome::Cancelled
                } else {
                    DomainOutcome::Closed
                });
            }
            domains.requests.insert(id, request);
            continue;
        }
        let access = match accesses(&domains, &request) {
            Ok(access) => access,
            Err(reason) => {
                request.access_error = Some(reason);
                request.finish(DomainOutcome::Failed(invalid(
                    "Dynamic domain path rejected",
                )));
                domains.parallel.pending.remove(&id);
                domains.requests.insert(id, request);
                continue;
            }
        };
        if let Work::Pair(pair) = &mut request.work {
            if let Some(paths) = &mut pair.paths {
                if paths.changed(&domains).unwrap_or(false) {
                    let attempt = &domains.parallel.pending[&id];
                    if attempt.retries >= domains.limits.max_path_retries {
                        request.access_error = Some(access::Rejection::RetryLimit);
                        request.finish(DomainOutcome::Failed(invalid(
                            "Domain path retry limit exceeded",
                        )));
                        domains.parallel.pending.remove(&id);
                    } else if let Some(order) = domains.parallel.next_attempt.checked_add(1) {
                        paths
                            .retry(&domains)
                            .expect("path resolution is stable inside the control turn");
                        domains.parallel.next_attempt = order;
                        let attempt = domains.parallel.pending.get_mut(&id).unwrap();
                        attempt.order = order;
                        attempt.retries += 1;
                        wake.wake();
                    } else {
                        request.access_error = Some(access::Rejection::ResourceLimit);
                        request.finish(DomainOutcome::Failed(limited(
                            "Domain attempt identities exhausted",
                        )));
                        domains.parallel.pending.remove(&id);
                    }
                    domains.requests.insert(id, request);
                    continue;
                }
            }
        }
        if barrier
            || older.iter().any(|a| conflicts(a, &access))
            || domains
                .parallel
                .flights
                .values()
                .any(|f| conflicts(&f.access, &access))
        {
            older.push(access);
            domains.requests.insert(id, request);
            continue;
        }
        domains.parallel.pending.remove(&id);
        if !matches!(&request.work, Work::Pair(pair) if pair.method.starts_with("$when/")) {
            // Do not pass later work across an admitted legacy operation.
            domains.requests.insert(id, request);
            local.push(id);
            break;
        }
        let Work::Pair(pair) = &mut request.work else {
            unreachable!()
        };
        match prepare(&mut domains, pair, access, &spawner, &wake, now) {
            Ok(flight) => {
                domains.parallel.flights.insert(id, flight);
            }
            Err((error, reason, jobs)) => {
                cancel_jobs.extend(jobs);
                request.access_error = reason;
                request.finish(DomainOutcome::Failed(*error));
            }
        }
        domains.requests.insert(id, request);
    }
    let executor = domains.executor.upgrade();
    drop(domains);
    if let Some(executor) = executor {
        for job in cancel_jobs {
            executor.cancel(job);
        }
    }
    for id in local {
        super::execute_local(registry, id);
    }
}
type PreparationFailure = (Box<Error>, Option<access::Rejection>, Vec<TaskId>);

fn prepare(
    domains: &mut Domains,
    pair: &mut pairs::Pair,
    access: Access,
    spawner: &pool::DomainSpawner,
    wake: &TaskWaker,
    now: Option<std::time::Instant>,
) -> Result<Flight, PreparationFailure> {
    let mut staged = None;
    let mut rejection = None;
    let mut creations = Vec::new();
    let result = (|| {
        if pair.paths.is_some() {
            staged = Some(
                pairs::paths::refresh_with(domains, pair, |id, module, info, name, input, _, _| {
                    creations.push(Creation {
                        id,
                        code: Code::freeze(module, name)?,
                        info: info.clone(),
                        words: input.to_vec(),
                    });
                    Ok(None)
                })
                .map_err(|failure| match failure {
                    pairs::paths::Failure::Constructor(error) => error,
                    pairs::paths::Failure::Rejected(reason) => {
                        rejection = Some(reason);
                        invalid("Domain path rejected")
                    }
                })?,
            );
            // Clock sampling and automatic registrations are prepared before worker entry.
            staged.as_ref().unwrap().prepare_automatic(domains, now)?;
        }
        let module = pairs::resolve(domains, &pair.handles, &pair.method, &pair.info)?;
        if pair
            .anonymous_module
            .is_some_and(|key| key != code::module_key(&module))
        {
            return Err(error(
                ErrorCode::TypeMismatch,
                "Anonymous when module was replaced",
            ));
        }
        let code = Code::freeze(&module, &pair.method)?;
        #[cfg(test)]
        for handle in &pair.handles {
            if let Some(root) = domains
                .entries
                .get_mut(&handle.sequence)
                .and_then(|d| d.root.as_mut())
                .and_then(Arc::get_mut)
            {
                root.untrack();
            }
        }
        let mut versions = Vec::new();
        let mut roots = Vec::new();
        for (handle, ty) in pair.handles.iter().zip(&pair.info.signature.params) {
            let domain = domains.get(*handle)?;
            let write = ty.reference_type().unwrap().mutable;
            domain
                .revision
                .checked_add(u64::from(write))
                .ok_or_else(|| limited("Domain revision exhausted"))?;
            versions.push((*handle, domain.revision, domain.code, domain.schema));
            let Some(root) = domain.root.as_ref() else {
                roots.push((handle.sequence, domain.ty.clone(), InputRoot::New));
                continue;
            };
            roots.push((
                handle.sequence,
                domain.ty.clone(),
                if write {
                    InputRoot::Write({
                        let candidate = root.snapshot(&domain.ty, domains.budget.clone())?;
                        #[cfg(test)]
                        let candidate = {
                            let mut candidate = candidate;
                            candidate.untrack();
                            candidate
                        };
                        Arc::new(candidate)
                    })
                } else {
                    InputRoot::Read(root.clone())
                },
            ));
        }
        let packet_bytes = std::mem::size_of::<Computation>()
            + roots.capacity() * std::mem::size_of::<(u64, Type, InputRoot)>()
            + pair.words.len() * 8
            + creations
                .iter()
                .map(|c| std::mem::size_of::<Creation>() + c.words.capacity() * 8)
                .sum::<usize>();
        let packet_charge = reads::Charge::new(domains.budget.clone(), packet_bytes)?;
        let flight_charge = reads::Charge::new(
            domains.budget.clone(),
            std::mem::size_of::<Flight>()
                + versions.capacity() * std::mem::size_of::<(DomainHandle, u64, u64, u64)>()
                + access.capacity() * std::mem::size_of::<(u64, bool, bool)>(),
        )?;
        let compute = Computation {
            charge: packet_charge,
            creations,
            roots,
            words: pair.words.clone(),
            info: pair.info.clone(),
            limits: pair.limits,
            code,
            budget: domains.budget.clone(),
        };
        let identity = domains.identity;
        let limits = domains.limits;
        #[cfg(test)]
        let method = pair.method.clone();
        let handle = spawner.submit(Box::new(move || {
            Ok(Box::new(Job {
                compute: Some(compute),
                identity,
                limits,
                #[cfg(test)]
                method,
            }))
        }))?;
        handle.observe(wake.clone());
        Ok((handle, module, versions, flight_charge))
    })();
    match result {
        Ok((handle, module, versions, flight_charge)) => Ok(Flight {
            access,
            versions,
            handle,
            staged,
            _module: module,
            _charge: flight_charge,
        }),
        Err(error) => {
            let reason =
                rejection.or_else(|| Some(access::classify(domains, &pair.handles, &error)));
            let jobs = staged.map_or_else(Vec::new, |staged| staged.discard(domains));
            Err((Box::new(error), reason, jobs))
        }
    }
}
fn finish(
    domains: &mut Domains,
    request: &mut Request,
    flight: &mut Flight,
    result: Result<Computed, Error>,
) -> DomainOutcome {
    let result = (|| {
        let computed = result?;
        for (handle, revision, code, schema) in &flight.versions {
            let domain = domains.get(*handle).inspect_err(|error| {
                request.access_error = Some(access::classify(domains, &[*handle], error));
            })?;
            if (domain.revision, domain.code, domain.schema) != (*revision, *code, *schema) {
                return Err(invalid("Domain grant changed before commit"));
            }
        }
        if let Some(staged) = &flight.staged {
            staged.validate(domains).map_err(|reason| {
                request.access_error = Some(reason);
                invalid("Staged directory publication revoked")
            })?;
        }
        let Work::Pair(pair) = &request.work else {
            unreachable!()
        };
        let output = pair.output.as_ref();
        let value = if output.is_some() {
            InstanceValue::Unit
        } else {
            InstanceValue::from_bits(
                &pair.info.signature.ret,
                computed.output.first().copied().unwrap_or(0),
            )
            .ok_or_else(|| invalid("Invalid domain scalar result"))?
        };
        let reads = if pair.readonly() && output.is_none_or(|output| output.snapshot) {
            Some(reads::ReadSet::capture_many(domains, &pair.handles)?)
        } else {
            None
        };
        let extra = computed
            .output
            .capacity()
            .saturating_sub(output.map_or(0, |output| output.words.borrow().capacity()))
            .checked_mul(8)
            .ok_or_else(|| limited("Domain result size overflow"))?;
        let bytes = domains
            .request_bytes
            .checked_add(extra)
            .filter(|n| *n <= domains.limits.max_request_bytes)
            .ok_or_else(|| limited("Domain request byte limit exceeded"))?;
        let mut batch = computed.batch.map(outbox::PendingBatch::into_local);
        if computed.accepted {
            if let Some(batch) = &mut batch {
                batch.prepare(domains).map_err(|status| {
                    let token = batch.failed_target.unwrap_or(0);
                    request.access_error = Some(match status {
                        commits::Status::OutboxFull => access::Rejection::OutboxFull(token),
                        commits::Status::OutboxClosed => access::Rejection::OutboxClosed(token),
                        _ => access::Rejection::InvalidAccess,
                    });
                    invalid("Operation outbox publication refused")
                })?;
            }
        }
        let mut retired = Vec::new();
        retired
            .try_reserve_exact(computed.roots.len())
            .map_err(|_| limited("Domain retirement allocation failed"))?;
        // Control-thread publication is the sole commit point. All encoding and
        // capacity checks precede it; cancellation is serialized by this driver.
        domains.request_bytes = bytes;
        request.charge += extra;
        request.parallel_output = Some(computed._charge);
        if let Some(output) = output {
            *output.words.borrow_mut() = computed.output;
        }
        request.observed_reads = reads;
        if computed.accepted {
            for (id, root) in computed.roots {
                let domain = domains.entries.get_mut(&id).unwrap();
                retired.push(domain.root.replace(root));
                if pair
                    .handles
                    .iter()
                    .zip(&pair.info.signature.params)
                    .any(|(h, ty)| h.sequence == id && ty.reference_type().unwrap().mutable)
                {
                    domain.revision += 1;
                }
            }
            if let Some(staged) = &mut flight.staged {
                staged.publish(domains);
            }
            if let Some(batch) = &mut batch {
                batch.publish();
                batch.wake();
            }
        }
        Ok(value)
    })();
    match result {
        Ok(value) => DomainOutcome::Value(value),
        Err(error) => {
            if request.access_error.is_none() && error.code() == ErrorCode::ResourceLimit {
                request.access_error = Some(access::Rejection::ResourceLimit);
            }
            DomainOutcome::Failed(error)
        }
    }
}

struct Gate;
impl Drop for Gate {
    fn drop(&mut self) {
        CONTEXT.with(|slot| {
            if let Some(c) = slot.borrow_mut().as_mut() {
                c.barriers -= 1;
                c.wake.wake();
            }
        });
    }
}
unsafe extern "C" fn stop_gate(frame: *mut CpsFrame, _: u32) {
    let pointer = (*frame).host_context.cast::<Gate>();
    (*frame).host_context = std::ptr::null_mut();
    (*frame).host_stop = None;
    if !pointer.is_null() {
        drop(Box::from_raw(pointer));
    }
}
/// A publication/migration or legacy storage operation drains granted attempts,
/// without waiting for ordinary VM flows. Cancellation drops its gate as well.
pub(crate) unsafe fn gate(frame: &mut CpsFrame) -> bool {
    if !CONTEXT.with(|slot| slot.borrow().is_some()) {
        return true;
    }
    if frame.host_context.is_null() {
        CONTEXT.with(|slot| slot.borrow_mut().as_mut().unwrap().barriers += 1);
        frame.host_context = Box::into_raw(Box::new(Gate)).cast();
        frame.host_stop = Some(stop_gate);
    }
    if busy() {
        if let Some(waker) = current_waker() {
            CONTEXT.with(|slot| {
                let mut c = slot.borrow_mut();
                let c = c.as_mut().unwrap();
                if !c.waiters.iter().any(|w| w.same_task(&waker)) {
                    c.waiters.push(waker);
                }
            });
        }
        frame.wake = None;
        false
    } else {
        stop_gate(frame, 0);
        true
    }
}

#[cfg(test)]
mod tests;
