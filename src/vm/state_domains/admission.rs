//! Executor-local admission with bounded FIFO waits and unique permit ownership.
use super::commits::{adopt, lease, Stored};
use super::*;
use crate::runtime::domain::Operation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum Status {
    Granted,
    Full,
    Closed,
    WaitLimit,
    Pending,
}
struct Waiter {
    id: u64,
    status: Status,
    waker: TaskWaker,
}
pub(super) struct Pool {
    registry: u64,
    capacity: usize,
    in_use: usize,
    max_waiters: usize,
    next_waiter: u64,
    closed: bool,
    waiters: Vec<Waiter>,
    _charge: reads::Charge,
}
impl Pool {
    fn promote(&mut self) {
        if self.closed {
            return;
        }
        for waiter in &mut self.waiters {
            if self.in_use == self.capacity {
                break;
            }
            if waiter.status == Status::Pending {
                // Reserve before waking: later try_acquire calls cannot barge.
                self.in_use += 1;
                waiter.status = Status::Granted;
                waiter.waker.wake();
            }
        }
    }
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        for waiter in &mut self.waiters {
            if waiter.status == Status::Granted {
                self.in_use -= 1;
            }
            waiter.status = Status::Closed;
            waiter.waker.wake();
        }
    }
}
struct Owner {
    registry: Weak<RefCell<Domains>>,
    token: u64,
    pool: Rc<RefCell<Pool>>,
}
impl Owner {
    fn close(&self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.borrow_mut().admissions.remove(&self.token);
        }
        self.pool.borrow_mut().close();
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.close();
    }
}
struct Permit {
    pool: Option<Rc<RefCell<Pool>>>,
    status: Status,
}
impl Permit {
    fn rejected(status: Status) -> Self {
        Self { pool: None, status }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            let mut pool = pool.borrow_mut();
            pool.in_use -= 1;
            pool.promote();
        }
    }
}
struct Wait {
    pool: Rc<RefCell<Pool>>,
    id: Option<u64>,
}
impl Wait {
    fn take(&mut self, waker: TaskWaker) -> Option<Permit> {
        let mut pool = self.pool.borrow_mut();
        let index = pool
            .waiters
            .iter()
            .position(|w| Some(w.id) == self.id)
            .unwrap();
        if pool.waiters[index].status == Status::Pending {
            pool.waiters[index].waker = waker;
            return None;
        }
        let status = pool.waiters.remove(index).status;
        self.id = None;
        Some(Permit {
            pool: (status == Status::Granted).then(|| self.pool.clone()),
            status,
        })
    }
}
impl Drop for Wait {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let mut pool = self.pool.borrow_mut();
        let index = pool.waiters.iter().position(|w| w.id == id).unwrap();
        if pool.waiters.remove(index).status == Status::Granted {
            pool.in_use -= 1;
        }
        pool.promote();
    }
}
fn create(
    registry: &Rc<RefCell<Domains>>,
    capacity: u64,
    max_waiters: u64,
) -> Result<Owner, Error> {
    let capacity = usize::try_from(capacity).map_err(|_| limited("Admission capacity overflow"))?;
    let max_waiters =
        usize::try_from(max_waiters).map_err(|_| limited("Admission wait limit overflow"))?;
    if capacity == 0 {
        return Err(invalid("Admission capacity must be positive"));
    }
    let mut domains = registry.borrow_mut();
    if domains.shutting_down {
        return Err(invalid("Admission executor is closed"));
    }
    // Account for both the preallocated queue and one suspended frame context per
    // slot. Waiting does not create additional tasks or unbounded registrations.
    let bytes = max_waiters
        .checked_mul(std::mem::size_of::<Waiter>() + std::mem::size_of::<Wait>())
        .and_then(|n| n.checked_add(std::mem::size_of::<Pool>() + 4 * std::mem::size_of::<usize>()))
        .ok_or_else(|| limited("Admission queue size overflow"))?;
    let charge = reads::Charge::new(domains.budget.clone(), bytes)?;
    let mut waiters = Vec::new();
    waiters
        .try_reserve_exact(max_waiters)
        .map_err(|_| limited("Admission queue allocation failed"))?;
    domains
        .admissions
        .try_reserve(1)
        .map_err(|_| limited("Admission registry allocation failed"))?;
    let token = source::next_token()?;
    let pool = Rc::new(RefCell::new(Pool {
        registry: domains.identity,
        capacity,
        in_use: 0,
        max_waiters,
        next_waiter: 1,
        closed: false,
        waiters,
        _charge: charge,
    }));
    domains.admissions.insert(token, Rc::downgrade(&pool));
    Ok(Owner {
        registry: Rc::downgrade(registry),
        token,
        pool,
    })
}
enum Acquisition {
    Ready(Permit),
    Waiting(Wait),
}
fn acquire(
    registry: &Rc<RefCell<Domains>>,
    token: u64,
    wait: Option<TaskWaker>,
) -> Result<Acquisition, Error> {
    let pool = {
        let domains = registry.borrow();
        if domains.shutting_down {
            None
        } else {
            domains.admissions.get(&token).and_then(Weak::upgrade)
        }
    };
    let Some(pool) = pool else {
        return Ok(Acquisition::Ready(Permit::rejected(Status::Closed)));
    };
    let mut state = pool.borrow_mut();
    if let Some(waker) = &wait {
        let executor = registry
            .borrow()
            .executor
            .upgrade()
            .ok_or_else(|| invalid("Admission executor is closed"))?;
        if !waker.belongs_to(&executor) {
            return Err(invalid("Admission waiter belongs to another executor"));
        }
    }
    if state.closed {
        return Ok(Acquisition::Ready(Permit::rejected(Status::Closed)));
    }
    if state.in_use < state.capacity {
        debug_assert!(!state.waiters.iter().any(|w| w.status == Status::Pending));
        state.in_use += 1;
        return Ok(Acquisition::Ready(Permit {
            pool: Some(pool.clone()),
            status: Status::Granted,
        }));
    }
    let Some(waker) = wait else {
        return Ok(Acquisition::Ready(Permit::rejected(Status::Full)));
    };
    if state.waiters.len() == state.max_waiters {
        return Ok(Acquisition::Ready(Permit::rejected(Status::WaitLimit)));
    }
    let id = state.next_waiter;
    state.next_waiter = id
        .checked_add(1)
        .ok_or_else(|| limited("Admission wait identities exhausted"))?;
    state.waiters.push(Waiter {
        id,
        status: Status::Pending,
        waker,
    });
    Ok(Acquisition::Waiting(Wait {
        pool: pool.clone(),
        id: Some(id),
    }))
}
pub(super) fn shutdown(registry: &Rc<RefCell<Domains>>) {
    // Wake only after releasing the registry borrow. Handles never retain pools.
    let pools: Vec<_> = registry
        .borrow_mut()
        .admissions
        .drain()
        .filter_map(|(_, p)| p.upgrade())
        .collect();
    for pool in pools {
        pool.borrow_mut().close();
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let wait = (*frame).host_context.cast::<Wait>();
    (*frame).host_context = std::ptr::null_mut();
    (*frame).host_stop = None;
    if !wait.is_null() {
        drop(Box::from_raw(wait));
    }
}
/// # Safety
/// The frame slots must match the checked admission operation signature. Resource
/// arguments and the frame's arena remain live until completion or synchronous stop.
pub(super) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<Option<u64>, Error> {
    let result = poll_inner(arena, frame, registry, op);
    if result.is_err() && op == Operation::AdmissionAcquire {
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
    match op {
        Operation::AdmissionCreate => {
            adopt(arena, create(registry, frame.slots[0], frame.slots[1])?).map(Some)
        }
        Operation::AdmissionHandle | Operation::AdmissionClose => {
            let lease = lease(frame.slots[0])?;
            let stored = &*lease.token().cast::<Stored<Owner>>();
            let owner = stored.value.borrow();
            if owner.pool.borrow().registry != registry.borrow().identity {
                return Err(invalid("Admission pool belongs to another executor"));
            }
            if op == Operation::AdmissionClose {
                owner.close();
                Ok(Some(0))
            } else {
                Ok(Some(owner.token))
            }
        }
        Operation::AdmissionStatus => {
            let lease = lease(frame.slots[0])?;
            let stored = &*lease.token().cast::<Stored<Permit>>();
            Ok(Some(stored.value.borrow().status as u64))
        }
        Operation::AdmissionTry | Operation::AdmissionAcquire => {
            let waker = if op == Operation::AdmissionAcquire {
                Some(
                    current_waker()
                        .ok_or_else(|| invalid("Admission wait requires an executor task"))?,
                )
            } else {
                None
            };
            if !frame.host_context.is_null() {
                let wait = &mut *frame.host_context.cast::<Wait>();
                if wait.pool.borrow().registry != registry.borrow().identity {
                    return Err(invalid("Admission wait belongs to another executor"));
                }
                let Some(permit) = wait.take(waker.unwrap()) else {
                    frame.wake = None;
                    return Ok(None);
                };
                stop(frame, 0);
                return adopt(arena, permit).map(Some);
            }
            match acquire(registry, frame.slots[0], waker)? {
                Acquisition::Ready(permit) => adopt(arena, permit).map(Some),
                Acquisition::Waiting(wait) => {
                    frame.host_context = Box::into_raw(Box::new(wait)).cast();
                    frame.host_stop = Some(stop);
                    frame.wake = None;
                    Ok(None)
                }
            }
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{vm::VmMode, FunctionSignature};
    thread_local! { static TOKEN: Cell<u64> = const { Cell::new(0) }; }
    extern "C" fn token() -> u64 {
        TOKEN.with(Cell::get)
    }
    const SOURCE: &str = r#"
        extern fn admission_test_token()->admission.Token
        extern fn __domain_admission_acquire(handle:admission.Token) @suspends ->admission.PermitOwner
        extern fn __domain_admission_status(p:&admission.PermitOwner) @suspends ->Int32
        fn wait() @suspends ->Int32 {
            let permit=__domain_admission_acquire(admission_test_token())
            __domain_admission_status(&permit)
        }
        fn fail() @suspends ->Int32 {
            let permit=__domain_admission_acquire(admission_test_token())
            let zero=__domain_admission_status(&permit)
            1/zero
        }
    "#;
    fn setup(mode: VmMode, capacity: u64, limit: u64) -> (VmEngine, Owner) {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "admission_test_token",
                FunctionSignature::new(vec![], Type::Named("admission.Token".into())),
                token as *const u8,
            )
            .unwrap();
        }
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let owner = create(&vm.domains, capacity, limit).unwrap();
        TOKEN.with(|t| t.set(owner.token));
        (vm, owner)
    }
    fn ready(vm: &VmEngine, owner: &Owner) -> Permit {
        match acquire(&vm.domains, owner.token, None).unwrap() {
            Acquisition::Ready(p) => p,
            _ => panic!("Unexpected wait"),
        }
    }
    fn start(vm: &VmEngine, entry: &str) -> TaskId {
        vm.executor
            .spawn(&TaskGroup::new(), vm.prepare_async(entry, &[]).unwrap())
            .unwrap()
    }
    #[test]
    fn admission_generated_waits_cancel_before_and_after_fifo_reservation() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (vm, owner) = setup(mode, 1, 2);
            let first = ready(&vm, &owner);
            let b = start(&vm, "wait");
            let c = start(&vm, "wait");
            vm.poll_tasks(100).unwrap();
            assert_eq!(owner.pool.borrow().waiters.len(), 2);
            assert_eq!(vm.poll_tasks(100).unwrap().polled, 0);
            let full = start(&vm, "wait");
            let report = vm.poll_tasks(100).unwrap();
            assert!(report.completed.iter().any(|r| r.id == full
                && r.result
                    .as_ref()
                    .is_ok_and(|v| *v == Status::WaitLimit as u64)));
            assert!(vm.executor.cancel(b));
            let d = start(&vm, "wait");
            vm.poll_tasks(100).unwrap();
            drop(first);
            assert_eq!(ready(&vm, &owner).status, Status::Full);
            assert_eq!(owner.pool.borrow().waiters[0].status, Status::Granted);
            // C is notified but has not received a language permit. Its stop must
            // transfer the reservation to D without requiring an extra release.
            assert!(vm.executor.cancel(c));
            assert_eq!(owner.pool.borrow().waiters.len(), 1);
            assert_eq!(owner.pool.borrow().waiters[0].status, Status::Granted);
            let report = vm.poll_tasks(100).unwrap();
            assert!(report
                .completed
                .iter()
                .any(|r| r.id == d && r.result.as_ref().is_ok_and(|v| *v == 0)));
            assert_eq!(owner.pool.borrow().in_use, 0);
            assert!(owner.pool.borrow().waiters.is_empty());
            let failed = start(&vm, "fail");
            let report = vm.poll_tasks(100).unwrap();
            assert!(report
                .completed
                .iter()
                .any(|r| r.id == failed && r.result.is_err()));
            assert_eq!(owner.pool.borrow().in_use, 0);
            drop(owner);
            assert!(vm.domains.borrow().admissions.is_empty());
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
    #[test]
    fn admission_close_revokes_wait_reservations_but_preserves_delivered_permits() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            for reserved in [false, true] {
                let (vm, owner) = setup(mode, 1, 2);
                let mut first = Some(ready(&vm, &owner));
                let b = start(&vm, "wait");
                let c = start(&vm, "wait");
                vm.poll_tasks(100).unwrap();
                if reserved {
                    first.take();
                }
                owner.close();
                owner.close();
                assert_eq!(ready(&vm, &owner).status, Status::Closed);
                let report = vm.poll_tasks(100).unwrap();
                for id in [b, c] {
                    assert!(report.completed.iter().any(|r| r.id == id
                        && r.result.as_ref().is_ok_and(|v| *v == Status::Closed as u64)));
                }
                assert_eq!(owner.pool.borrow().in_use, usize::from(!reserved));
                drop(first);
                assert_eq!(owner.pool.borrow().in_use, 0);
                drop(owner);
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
    #[test]
    fn admission_pool_and_permit_adoption_failures_refund_every_reservation() {
        let (vm, owner) = setup(VmMode::Interpreter, 1, 0);
        let baseline = vm.domains.borrow().budget.bytes.get();
        assert!(create(&vm.domains, 0, 0).is_err());
        assert!(create(&vm.domains, 1, u64::MAX).is_err());
        for limit in [0, std::mem::size_of::<Stored<Permit>>()] {
            let budget = std::sync::Arc::new(Budget {
                limit: (limit).into(),
                ..Default::default()
            });
            let mut arena = Arena::with_budget(budget.clone(), None);
            assert!(adopt(&mut arena, ready(&vm, &owner)).is_err());
            assert_eq!(owner.pool.borrow().in_use, 0);
            assert_eq!(budget.bytes.get(), 0);
        }
        for limit in [0, std::mem::size_of::<Stored<Owner>>()] {
            let budget = std::sync::Arc::new(Budget {
                limit: (limit).into(),
                ..Default::default()
            });
            let mut arena = Arena::with_budget(budget.clone(), None);
            assert!(adopt(&mut arena, create(&vm.domains, 1, 2).unwrap()).is_err());
            assert_eq!(vm.domains.borrow().admissions.len(), 1);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            assert_eq!(budget.bytes.get(), 0);
        }
        let old_limit = vm.domains.borrow().budget.limit.replace(baseline);
        assert!(create(&vm.domains, 1, 1).is_err());
        assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
        vm.domains.borrow().budget.limit.set(old_limit);
        let foreign = VmEngine::new();
        assert!(matches!(
            acquire(&foreign.domains, owner.token, None).unwrap(),
            Acquisition::Ready(Permit {
                status: Status::Closed,
                ..
            })
        ));
        let held = ready(&vm, &owner);
        drop(owner);
        assert_eq!(held.pool.as_ref().unwrap().borrow().in_use, 1);
        drop(held);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
    #[test]
    fn admission_shutdown_and_vm_drop_stop_all_waits() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let (vm, owner) = setup(mode, 1, 2);
            let held = ready(&vm, &owner);
            start(&vm, "wait");
            vm.poll_tasks(100).unwrap();
            assert_eq!(owner.pool.borrow().waiters.len(), 1);
            drop(vm);
            assert!(owner.pool.borrow().closed);
            assert!(owner.pool.borrow().waiters.is_empty());
            drop(held);
            assert_eq!(owner.pool.borrow().in_use, 0);
            drop(owner);
        }
    }
}
