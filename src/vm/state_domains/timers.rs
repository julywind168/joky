//! Named timer descriptors retain type metadata, never a root or a code module.
use super::commits::{adopt, lease, Stored};
use super::*;
use crate::runtime::{buffer::Buffer, domain::Operation, driver::Wake};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum Status {
    Idle,
    Active,
    Completed,
    Cancelled,
    Closed,
    InvalidAccess,
    ResourceLimit,
    Failed,
}
pub(super) struct State {
    status: Status,
    generation: u64,
    target: Option<DomainHandle>,
    job: Option<TaskId>,
    _charge: reads::Charge,
}
struct Owner {
    registry: Weak<RefCell<Domains>>,
    token: u64,
    state: Rc<RefCell<State>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.borrow_mut().timers.remove(&self.token);
            cancel(&registry, &self.state, Status::Closed);
        }
    }
}
fn create(registry: &Rc<RefCell<Domains>>) -> Result<Owner, Error> {
    let mut domains = registry.borrow_mut();
    if domains.shutting_down {
        return Err(invalid("Timer executor is closed"));
    }
    let charge = reads::Charge::new(
        domains.budget.clone(),
        std::mem::size_of::<State>() + 4 * std::mem::size_of::<usize>(),
    )?;
    domains
        .timers
        .try_reserve(1)
        .map_err(|_| limited("Timer registry allocation failed"))?;
    let token = source::next_token()?;
    let state = Rc::new(RefCell::new(State {
        status: Status::Idle,
        generation: 0,
        target: None,
        job: None,
        _charge: charge,
    }));
    domains.timers.insert(token, Rc::downgrade(&state));
    Ok(Owner {
        registry: Rc::downgrade(registry),
        token,
        state,
    })
}
fn state(registry: &Rc<RefCell<Domains>>, token: u64) -> Option<Rc<RefCell<State>>> {
    let domains = registry.borrow();
    if domains.shutting_down {
        return None;
    }
    domains.timers.get(&token).and_then(Weak::upgrade)
}
fn cancel(registry: &Rc<RefCell<Domains>>, state: &Rc<RefCell<State>>, status: Status) {
    let job = {
        let mut state = state.borrow_mut();
        if state.status == Status::Active || status == Status::Closed {
            state.status = status;
        }
        state.job
    };
    let executor = registry.borrow().executor.upgrade();
    if let (Some(executor), Some(job)) = (executor, job) {
        executor.cancel(job);
    }
    reap(registry);
}
pub(super) fn shutdown(registry: &Rc<RefCell<Domains>>) {
    let timers = std::mem::take(&mut registry.borrow_mut().timers);
    for state in timers.into_values().filter_map(|state| state.upgrade()) {
        cancel(registry, &state, Status::Closed);
    }
}
pub(super) fn schema_dependents(
    registry: &Rc<RefCell<Domains>>,
    target: DomainHandle,
) -> Result<Vec<Rc<RefCell<State>>>, Error> {
    let domains = registry.borrow();
    let mut timers = Vec::new();
    for state in domains.timers.values().filter_map(Weak::upgrade) {
        let selected = {
            let s = state.borrow();
            s.status == Status::Active && s.target == Some(target)
        };
        if selected {
            timers
                .try_reserve(1)
                .map_err(|_| limited("Timer invalidation allocation failed"))?;
            timers.push(state);
        }
    }
    Ok(timers)
}
pub(super) fn invalidate_schema(registry: &Rc<RefCell<Domains>>, timers: Vec<Rc<RefCell<State>>>) {
    for state in timers {
        cancel(registry, &state, Status::InvalidAccess);
    }
}
enum Input {
    Scalar(u64),
    Values(Vec<u64>),
}
enum Payload {
    Scalar(InstanceValue),
    Values(Vec<u64>),
}
struct Worker {
    registry: Weak<RefCell<Domains>>,
    state: Rc<RefCell<State>>,
    generation: u64,
    target: DomainHandle,
    name: String,
    signature: crate::FunctionSignature,
    input: Payload,
    limits: InterpreterLimits,
    due: Instant,
    period: Option<Duration>,
    request: Option<DomainRequest>,
    _charge: reads::Charge,
}
impl Worker {
    fn active(&self) -> bool {
        let state = self.state.borrow();
        state.generation == self.generation && state.status == Status::Active
    }
    fn finish(&self, status: Status) {
        let mut state = self.state.borrow_mut();
        if state.generation == self.generation && state.status == Status::Active {
            state.status = status;
        }
    }
    fn step(&mut self) -> Result<Option<Wake>, Status> {
        if !self.active() {
            return Err(Status::Cancelled);
        }
        let registry = self.registry.upgrade().ok_or(Status::Closed)?;
        let executor = registry.borrow().executor.upgrade().ok_or(Status::Closed)?;
        // A host clock may reenter lifecycle operations. Never hold a registry or
        // timer borrow across it, and recheck cancellation before admitting work.
        let now = executor.now();
        if !self.active() {
            return Err(Status::Cancelled);
        }
        if let Some(request) = &mut self.request {
            let Some(outcome) = request.take_result() else {
                request
                    .register_current_wait()
                    .map_err(|e| status_error(&e))?;
                return Ok(None);
            };
            self.request = None;
            match outcome {
                DomainOutcome::Value(_) => {}
                DomainOutcome::Closed => return Err(Status::Closed),
                DomainOutcome::Cancelled => return Err(Status::Cancelled),
                DomainOutcome::Failed(error) => return Err(status_error(&error)),
            }
            let Some(period) = self.period else {
                return Err(Status::Completed);
            };
            // Preserve the initial cadence, but schedule strictly after now.
            // A delayed host never produces a burst of missed ticks.
            let remainder = now.saturating_duration_since(self.due).as_nanos() % period.as_nanos();
            self.due = now
                .checked_add(period - Duration::from_nanos(remainder as u64))
                .ok_or(Status::ResourceLimit)?;
        }
        if now < self.due {
            return Ok(Some(Wake::Deadline(self.due)));
        }
        {
            let domains = registry.borrow();
            let domain = domains
                .get(self.target)
                .map_err(|_| target_status(&domains, self.target))?;
            let info = domain
                .module
                .instance_info(&self.name)
                .map_err(|_| Status::InvalidAccess)?;
            if !matches_contract(&info, &self.signature, &domain.ty) {
                return Err(Status::InvalidAccess);
            }
        }
        let request = match &self.input {
            Payload::Scalar(input) => submit(
                &registry,
                self.target,
                &self.name,
                std::slice::from_ref(input),
                self.limits,
            ),
            Payload::Values(words) => {
                submit_values(&registry, self.target, &self.name, words, self.limits)
            }
        }
        .map_err(|e| status_error(&e))?;
        request
            .register_current_wait()
            .map_err(|e| status_error(&e))?;
        self.request = Some(request);
        Ok(None)
    }
}
// Keep only signature metadata and lanes in the FIFO, so queued timers do not
// retain the module that happened to be current when they became due.
fn submit_values(
    registry: &Rc<RefCell<Domains>>,
    target: DomainHandle,
    method: &str,
    input: &[u64],
    limits: InterpreterLimits,
) -> Result<DomainRequest, Error> {
    let domains = registry.borrow();
    let domain = domains.get(target)?;
    let info = domain.module.instance_info(method)?;
    if !target.writable || !matches_contract(&info, &info.signature, &domain.ty) {
        return Err(error(ErrorCode::InvalidCall, "Invalid timer value entry"));
    }
    let mut words = Vec::new();
    words
        .try_reserve_exact(input.len() + 1)
        .map_err(|_| limited("Timer input allocation failed"))?;
    words.push(0);
    words.extend_from_slice(input);
    let extra = words.len() * std::mem::size_of::<u64>() + method.len();
    let work = Work::Turn {
        method: method.to_owned(),
        info,
        code: domain.code,
        words,
        limits,
    };
    drop(domains);
    enqueue(registry, target.sequence, work, extra)
}

fn target_status(domains: &Domains, target: DomainHandle) -> Status {
    if domains
        .entries
        .get(&target.sequence)
        .is_some_and(|d| d.failure.is_some() && !d.closing)
    {
        Status::Failed
    } else {
        Status::Closed
    }
}
fn status_error(error: &Error) -> Status {
    match error.code() {
        ErrorCode::ResourceLimit => Status::ResourceLimit,
        ErrorCode::InvalidCall | ErrorCode::NotFound | ErrorCode::TypeMismatch => {
            Status::InvalidAccess
        }
        _ => Status::Failed,
    }
}
fn matches_contract(info: &FunctionInfo, signature: &crate::FunctionSignature, ty: &Type) -> bool {
    info.domain_entry
        && crate::core::state_domain::value_eligible(info)
        && info.signature.params.len() == 2
        && info.signature.ret == Type::Unit
        && info.signature.params[0]
            .reference_type()
            .is_some_and(|r| r.mutable && code::compatible(&r.target, ty))
        && info.signature.params.len() == signature.params.len()
        && info
            .signature
            .params
            .iter()
            .zip(&signature.params)
            .all(|(a, b)| code::compatible(a, b))
        && code::compatible(&info.signature.ret, &signature.ret)
}
impl Drop for Worker {
    fn drop(&mut self) {
        // Dispose the queued turn before allowing this owner to be armed again.
        drop(self.request.take());
        let status = self
            .registry
            .upgrade()
            .map(|r| target_status(&r.borrow(), self.target))
            .unwrap_or(Status::Closed);
        let mut state = self.state.borrow_mut();
        if state.generation == self.generation {
            state.job = None;
            if state.status == Status::Active {
                state.status = status;
            }
        }
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let pointer = (*frame).host_context.cast::<Worker>();
    (*frame).host_context = std::ptr::null_mut();
    (*frame).host_stop = None;
    if !pointer.is_null() {
        drop(Box::from_raw(pointer));
    }
}
unsafe extern "C" fn tick(frame: *mut CpsFrame) -> i32 {
    let worker = &mut *(*frame).host_context.cast::<Worker>();
    match worker.step() {
        Ok(wake) => {
            (*frame).wake = wake;
            1
        }
        Err(status) => {
            worker.finish(status);
            (*frame).state = DONE;
            (*frame).result = 0;
            0
        }
    }
}
struct Environment;
impl TaskEnvironment for Environment {
    fn report_completion(&self) -> bool {
        false
    }
    fn resume(&self, task: &mut Continuation) -> Result<(), Error> {
        task.resume().map_err(Error::from)
    }
    fn dispose(&self, task: Continuation) {
        drop(task);
    }
}
struct Schedule<'a> {
    target: DomainHandle,
    module: &'a LoadedModule,
    name: &'a str,
    input: Input,
    milliseconds: i32,
    periodic: bool,
    limits: InterpreterLimits,
}
// Start error codes are deliberately distinct from terminal status codes.
fn start(registry: &Rc<RefCell<Domains>>, token: u64, schedule: Schedule<'_>) -> Result<(), u64> {
    let state = state(registry, token).ok_or(1u64)?;
    if state.borrow().status == Status::Closed {
        return Err(1);
    }
    if state.borrow().job.is_some() || state.borrow().status == Status::Active {
        return Err(2);
    }
    if schedule.milliseconds < 0 || (schedule.periodic && schedule.milliseconds == 0) {
        return Err(3);
    }
    let executor = registry.borrow().executor.upgrade().ok_or(1u64)?;
    let delay = Duration::from_millis(schedule.milliseconds as u64);
    let due = executor.now().checked_add(delay).ok_or(3u64)?;
    // Recheck owner and target after the external clock callback.
    if state.borrow().status == Status::Closed || registry.borrow().shutting_down {
        return Err(1);
    }
    if state.borrow().job.is_some() || state.borrow().status == Status::Active {
        return Err(2);
    }
    reap(registry);
    let info = schedule
        .module
        .instance_info(schedule.name)
        .map_err(|_| 4u64)?;
    if !schedule.target.writable || info.signature.ret != Type::Unit {
        return Err(4);
    }
    let input = match schedule.input {
        Input::Scalar(bits) => {
            Payload::Scalar(source::turn_arguments(&info, Some(bits), true).map_err(|_| 4u64)?[0])
        }
        Input::Values(words) => {
            if !crate::core::state_domain::value_eligible(&info)
                || info.signature.params.len() != 2
                || info.signature.params[0]
                    .reference_type()
                    .is_none_or(|r| !r.mutable)
            {
                return Err(4);
            }
            let mut words =
                values::result_words(&info.signature.params[1], words).map_err(|_| 4u64)?;
            if info.signature.params[1] == Type::Unit {
                words.push(0);
            }
            Payload::Values(words)
        }
    };
    let input_bytes = match &input {
        Payload::Scalar(_) => 0,
        Payload::Values(words) => words.capacity() * std::mem::size_of::<u64>(),
    };
    let charge = {
        let mut domains = registry.borrow_mut();
        let domain = domains.get(schedule.target).map_err(|_| 1u64)?;
        let current = domain
            .module
            .instance_info(schedule.name)
            .map_err(|_| 4u64)?;
        if !info.domain_entry || !matches_contract(&current, &info.signature, &domain.ty) {
            return Err(4);
        }
        let bytes = input_bytes
            + std::mem::size_of::<Worker>()
            + std::mem::size_of::<CpsFrame>()
            + schedule.name.len()
            + info.signature.params.len() * std::mem::size_of::<Type>();
        let charge = reads::Charge::new(domains.budget.clone(), bytes).map_err(|_| 5u64)?;
        domains
            .entries
            .get_mut(&schedule.target.sequence)
            .unwrap()
            .tasks
            .try_reserve(1)
            .map_err(|_| 5u64)?;
        charge
    };
    let generation = source::next_token().map_err(|_| 5u64)?;
    // No code owner or state reference crosses the registration boundary.
    let worker = Worker {
        registry: Rc::downgrade(registry),
        state: state.clone(),
        generation,
        target: schedule.target,
        name: schedule.name.into(),
        signature: info.signature,
        input,
        limits: schedule.limits,
        due,
        period: schedule.periodic.then_some(delay),
        request: None,
        _charge: charge,
    };
    let mut frame = CpsFrame::new(0);
    frame.host_context = Box::into_raw(Box::new(worker)).cast();
    frame.host_stop = Some(stop);
    let task = unsafe { Continuation::new(tick, frame, ModuleId(0), Rc::new(())) };
    let group = TaskGroup::new();
    let job = executor
        .spawn_in(&group, task, Some(Rc::new(Environment)))
        .map_err(|_| 5u64)?;
    registry
        .borrow_mut()
        .entries
        .get_mut(&schedule.target.sequence)
        .unwrap()
        .tasks
        .insert(job, BoundTask { group, module: 0 });
    let mut state = state.borrow_mut();
    state.target = Some(schedule.target);
    state.generation = generation;
    state.status = Status::Active;
    state.job = Some(job);
    Ok(())
}
/// # Safety
/// Slots match compiler-owned signatures and resources/text remain live for this poll.
pub(super) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<u64, Error> {
    let slots = &frame.slots;
    match op {
        Operation::TimerCreate => adopt(arena, create(registry)?),
        Operation::TimerHandle => {
            let lease = lease(slots[0])?;
            let owner = (&*lease.token().cast::<Stored<Owner>>()).value.borrow();
            if !owner.registry.ptr_eq(&Rc::downgrade(registry)) {
                return Err(invalid("Timer belongs to another executor"));
            }
            Ok(owner.token)
        }
        Operation::TimerCancel | Operation::TimerClose => {
            if let Some(state) = state(registry, slots[0]) {
                cancel(
                    registry,
                    &state,
                    if op == Operation::TimerClose {
                        Status::Closed
                    } else {
                        Status::Cancelled
                    },
                );
            }
            Ok(0)
        }
        Operation::TimerStatus => {
            Ok(state(registry, slots[0]).map_or(Status::Closed, |s| s.borrow().status) as u64)
        }
        Operation::TimerStart | Operation::TimerStartValues => {
            let Ok(target) = source::handle(registry, slots[2]) else {
                return Ok(1);
            };
            let (module, _) = source::module(registry, slots[0])?;
            let name = crate::runtime::owned_text::borrow(slots[4] as *const Buffer)?;
            let input = if op == Operation::TimerStartValues {
                let info = module.instance_info(name)?;
                if !info.domain_entry
                    || !crate::core::state_domain::value_eligible(&info)
                    || info.signature.params.len() != 2
                    || info.signature.ret != Type::Unit
                {
                    return Ok(4);
                }
                let packet = &*(slots[5] as *const Buffer);
                match values::input_words(&info.signature.params[1..], packet) {
                    Ok(words) => Input::Values(words),
                    Err(error) => {
                        return Ok(if error.code() == ErrorCode::ResourceLimit {
                            5
                        } else {
                            4
                        })
                    }
                }
            } else {
                Input::Scalar(slots[5])
            };
            Ok(start(
                registry,
                slots[1],
                Schedule {
                    target,
                    module: &module,
                    name,
                    input,
                    milliseconds: slots[3] as i32,
                    periodic: slots[6] != 0,
                    limits: crate::runtime::suspension::current_limits(),
                },
            )
            .err()
            .unwrap_or(0))
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
