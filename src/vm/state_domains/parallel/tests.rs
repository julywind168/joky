use super::*;
use crate::runtime::{
    executor::BlockingContext,
    parallel::{Guard, ParallelExecutor, Storage, TaskBudget},
};
use crate::vm::VmMode;
use std::{
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

static SERIAL: Mutex<()> = Mutex::new(());
static PROBE: Mutex<Option<Arc<Probe>>> = Mutex::new(None);
#[derive(Default)]
struct Observation {
    threads: Vec<std::thread::ThreadId>,
    release: bool,
}
#[derive(Default)]
struct Probe {
    state: Mutex<Observation>,
    wake: Condvar,
}
impl Probe {
    fn install() -> Arc<Self> {
        let probe = Arc::new(Self::default());
        *PROBE.lock().unwrap() = Some(probe.clone());
        probe
    }
    fn wait(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = self.state.lock().unwrap();
        while state.threads.len() < count {
            let (next, _) = self
                .wake
                .wait_timeout(state, Duration::from_millis(10))
                .unwrap();
            state = next;
            assert!(
                Instant::now() < deadline,
                "worker computations did not overlap"
            );
        }
    }
    fn release(&self) {
        self.state.lock().unwrap().release = true;
        self.wake.notify_all();
    }
}
struct Reset(Arc<Probe>);
impl Drop for Reset {
    fn drop(&mut self) {
        self.0.release();
        PROBE.lock().unwrap().take();
    }
}
pub(super) fn completed_computation(method: &str) -> Result<(), Error> {
    if !method.ends_with("/7:723870726f6265") {
        return Ok(());
    }
    let probe = PROBE.lock().unwrap().clone();
    let Some(probe) = probe else {
        return Ok(());
    };
    let mut state = probe.state.lock().unwrap();
    state.threads.push(std::thread::current().id());
    probe.wake.notify_all();
    while !state.release {
        pool::check_cancelled()?;
        state = probe
            .wake
            .wait_timeout(state, Duration::from_millis(1))
            .unwrap()
            .0;
    }
    Ok(())
}
#[test]
fn direct_parallel_when_discards_all_cancelled_candidates() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe extern "C" fn candidate_ready(frame: *mut CpsFrame) -> i32 {
        let probe = PROBE.lock().unwrap().as_ref().unwrap().clone();
        if probe.state.lock().unwrap().threads.is_empty() {
            (*frame).wake = Some(crate::runtime::driver::Wake::Immediate);
            return 1;
        }
        0
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "candidate_ready",
                crate::FunctionSignature::new(vec![], Type::Unit),
                candidate_ready,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(
            r#"
            import joky/domain
            extern fn candidate_ready() @suspends
            #state
            struct Stock { count: Int32 }
            fn stock(n: Int32) -> Stock { Stock { count: n } }
            fn transfer(a: domain.WriteHandle<Stock>, b: domain.WriteHandle<Stock>) @suspends {
                when "r8probe" (mut a, mut b) { a.count -= 1; b.count += 1 }!;
            }
            fn main() @suspends {
                let source = domain.create(stock, 1)
                let dest = domain.create(stock, 0)
                let a = source.handle()
                let b = dest.handle()
                race { transfer(a, b), candidate_ready() }
                assert(when (a, b) { a.count == 1 && b.count == 0 }!, "cancelled transfer changed balances")
            }
        "#,
        )
        .unwrap();
        let budget = TaskBudget::new(64 << 20);
        let root = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap();
        let mut pool = ParallelExecutor::new(4, 32).unwrap();
        let _reset = Reset(Probe::install());
        let task = root.submit(&pool).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !task.is_finished() {
            assert!(
                Instant::now() < deadline,
                "cancelled domain candidate did not drain"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        task.take_result().unwrap();
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn direct_parallel_disjoint_transfers_compute_together() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../../../tests/fixtures/parallel_multi_domain.jk"
        ))
        .unwrap();
        let root = vm.prepare_parallel_task("main", vec![], vec![]).unwrap();
        let pool = ParallelExecutor::new(4, 64).unwrap();
        let probe = Probe::install();
        let reset = Reset(probe.clone());
        let task = root.submit(&pool).unwrap();
        probe.wait(2);
        {
            let state = probe.state.lock().unwrap();
            assert_ne!(state.threads[0], state.threads[1]);
        }
        drop(reset);
        task.wait().unwrap();
        task.take_result().unwrap();
    }
}

struct Park;
impl pool::Job for Park {
    fn poll(&mut self) -> Poll<Result<pool::Output, Error>> {
        pool::wait_for_wake();
        Poll::Pending
    }
}
const SOURCE: &str = r#"import joky/domain
import joky/array.{type Array}
struct State { n:Int32, data:Array<Int32> }
fn make(n:Int32)->State {var data=Array.new::<Int32>(); data.push(n); State{n:n,data:data}}
fn read(s:&State)->Int32 {s.n}
fn operation(a:domain.WriteHandle<State>,b:domain.ReadHandle<State>,n:Int32) @suspends ->Int32 {
    when "r8probe" (mut a,b) {a.n+=n+b.n; let value=a.n; a.data.push(value); a.n}!
}
fn shared(a:domain.ReadHandle<State>,b:domain.ReadHandle<State>,n:Int32) @suspends ->Int32 {
    when "r8probe" (a,b) {a.n+b.n+n}!
}
"#;
const WRITE: &str = "$when/named/4:main/9:operation/7:723870726f6265";
const READ: &str = "$when/named/4:main/6:shared/7:723870726f6265";
fn queue(
    vm: &VmEngine,
    handles: &[DomainHandle],
    entry: &str,
) -> (DomainRequest, Rc<RefCell<Vec<u64>>>) {
    let output = Rc::new(RefCell::new(vec![0]));
    let request = pairs::submit_values(
        &vm.domains,
        handles,
        vm.current.as_ref().unwrap(),
        entry,
        vec![1],
        pairs::ValueOutput {
            words: output.clone(),
            snapshot: false,
        },
        vm.interpreter_limits(),
    )
    .unwrap();
    (request, output)
}
fn drain(vm: &mut VmEngine) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        vm.poll_tasks(128).unwrap();
        drive();
        let d = vm.domains.borrow();
        if d.parallel.flights.is_empty() && d.parallel.pending.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "domain coordinator did not drain"
        );
        drop(d);
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn root_value(vm: &VmEngine, handle: DomainHandle) -> i32 {
    let domains = vm.domains.borrow();
    let pointer = domains
        .get(handle)
        .unwrap()
        .root
        .as_ref()
        .unwrap()
        .scalar_pointer();
    // This fixture's first lane is Int32, and workers mutate private candidates.
    unsafe { (pointer as *const i32).read() }
}
#[test]
fn parallel_domains_grant_complete_sets_and_share_read_roots_without_copying() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for shared in [false, true] {
            let budget = TaskBudget::new(64 << 20);
            let storage = Storage::new(vec![budget.clone()]).unwrap();
            let _storage = Guard::enter(&storage);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let parent = pool
                .submitter()
                .publish(
                    storage.clone(),
                    Arc::new(()),
                    Box::new(|| Ok(Box::new(Park))),
                )
                .unwrap();
            let context = BlockingContext::new().unwrap();
            let scope = Scope::enter(parent.domain_spawner(), context.waker());
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE)
                .unwrap_or_else(|error| panic!("{error}"));
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(20)])
                .unwrap();
            let c = vm
                .create_domain("make", &[InstanceValue::Int32(30)])
                .unwrap();
            let before = vm.domains.borrow().budget.bytes.get();
            let probe = Probe::install();
            let reset = Reset(probe.clone());
            let handles = if shared {
                [a.read_handle(), b.read_handle()]
            } else {
                [a.write_handle(), b.read_handle()]
            };
            let other = if shared {
                handles
            } else {
                [c.write_handle(), b.read_handle()]
            };
            let entry = if shared { READ } else { WRITE };
            let (mut first, first_output) = queue(&vm, &handles, entry);
            let (mut second, second_output) = queue(&vm, &other, entry);
            vm.poll_tasks(128).unwrap();
            drive();
            probe.wait(2);
            {
                let observation = probe.state.lock().unwrap();
                let threads = &observation.threads;
                assert_ne!(threads[0], threads[1]);
                assert!(!threads.contains(&std::thread::current().id()));
            }
            assert_eq!(vm.domains.borrow().parallel.flights.len(), 2);
            assert_eq!(root_value(&vm, a.read_handle()), 10);
            assert_eq!(
                Arc::strong_count(
                    vm.domains
                        .borrow()
                        .get(b.read_handle())
                        .unwrap()
                        .root
                        .as_ref()
                        .unwrap()
                ),
                3
            );
            // The fixture's large-data variant is covered by the existing R5 budget test;
            // this asserts identity pinning on the actual parallel path.
            drop(reset);
            drain(&mut vm);
            assert!(matches!(first.take_result(), Some(DomainOutcome::Value(_))));
            assert!(matches!(
                second.take_result(),
                Some(DomainOutcome::Value(_))
            ));
            assert_eq!(*first_output.borrow(), vec![31]);
            assert_eq!(*second_output.borrow(), vec![if shared { 31 } else { 51 }]);
            assert!(vm.domains.borrow().budget.bytes.get() >= before);
            drop((first, second, a, b, c, vm));
            drive();
            drop(scope);
            parent.cancel();
            parent.wait().unwrap();
            drop(parent);
            pool.wait_closed();
            drop(storage);
            // Guard pins Storage until the enclosing iteration ends.
            drop(_storage);
            assert_eq!(budget.used(), 0);
        }
    }
}
#[test]
fn parallel_domains_fifo_cancel_close_and_publication_gate() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for action in ["commit", "cancel", "close", "publish", "migration_cancel"] {
            let budget = TaskBudget::new(64 << 20);
            let storage = Storage::new(vec![budget.clone()]).unwrap();
            let guard = Guard::enter(&storage);
            let mut pool = ParallelExecutor::new(2, 32).unwrap();
            let parent = pool
                .submitter()
                .publish(
                    storage.clone(),
                    Arc::new(()),
                    Box::new(|| Ok(Box::new(Park))),
                )
                .unwrap();
            let context = BlockingContext::new().unwrap();
            let scope = Scope::enter(parent.domain_spawner(), context.waker());
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE)
                .unwrap_or_else(|error| panic!("{error}"));
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(20)])
                .unwrap();
            let probe = Probe::install();
            let reset = Reset(probe.clone());
            let (mut first, _) = queue(&vm, &[a.write_handle(), b.read_handle()], WRITE);
            // A later reader cannot pass the earlier writer on a shared participant.
            let (mut second, result) = queue(&vm, &[b.write_handle(), a.read_handle()], WRITE);
            vm.poll_tasks(128).unwrap();
            drive();
            probe.wait(1);
            assert_eq!(vm.domains.borrow().parallel.flights.len(), 1);
            assert_eq!(vm.domains.borrow().parallel.pending.len(), 1);
            assert_eq!(probe.state.lock().unwrap().threads.len(), 1);
            let mut frame = CpsFrame::new(0);
            match action {
                "cancel" => first.cancel(),
                "close" => {
                    a.close();
                }
                "migration_cancel" => {
                    frame.slots = vec![0; 7];
                    frame.slots[1] = 7;
                    // An actual migration command must park before validating its packet.
                    let arena = unsafe { &mut *crate::runtime::cps::arena(&mut frame) };
                    let empty = crate::runtime::buffer::Buffer::copy_bytes(arena, b"").unwrap();
                    frame.slots[3..7].fill(empty.as_ptr() as u64);
                    assert_eq!(
                        unsafe {
                            crate::stdlib::migration_poll(
                                arena,
                                &mut frame,
                                crate::core::vm::MigrationOperation::Command,
                            )
                        }
                        .unwrap(),
                        None
                    );
                    assert!(!frame.host_context.is_null());
                    frame.stop_host(crate::runtime::borrowed_host::STOP_CANCELLED);
                    assert!(CONTEXT.with(|c| c.borrow().as_ref().unwrap().barriers == 0));
                    unsafe {
                        crate::runtime::buffer::Buffer::release(arena, empty.as_ptr()).unwrap();
                    }
                }
                "publish" => {
                    assert_eq!(
                        vm.publish_domain_code().unwrap_err().code(),
                        ErrorCode::InvalidState
                    );
                    let owner: Rc<dyn std::any::Any> = match vm.current.as_ref().unwrap() {
                        LoadedModule::Jit(module) => module.clone(),
                        LoadedModule::Interpreter(module) => module.clone(),
                    };
                    assert_eq!(
                        migration::batch::begin(&vm, &vm.domains, &[owner], "")
                            .err()
                            .unwrap()
                            .code(),
                        ErrorCode::InvalidState
                    );
                    assert!(!unsafe { gate(&mut frame) });
                }
                _ => {}
            }
            drop(reset);
            if action == "publish" {
                let deadline = Instant::now() + Duration::from_secs(10);
                while busy() {
                    drive();
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(vm.domains.borrow().parallel.pending.len(), 1);
                assert!(unsafe { gate(&mut frame) });
                vm.load_source(&SOURCE.replace("a.n+=n+b.n", "a.n+=100+n+b.n"))
                    .unwrap();
                vm.publish_domain_code().unwrap();
            }
            drain(&mut vm);
            let outcome = first.take_result().unwrap();
            if action == "cancel" {
                assert!(matches!(outcome, DomainOutcome::Cancelled));
                assert_eq!(root_value(&vm, a.read_handle()), 10);
            } else if action == "close" {
                assert!(matches!(outcome, DomainOutcome::Closed));
            } else {
                assert!(matches!(outcome, DomainOutcome::Value(_)));
            }
            if action != "close" {
                assert!(matches!(
                    second.take_result(),
                    Some(DomainOutcome::Value(_))
                ));
                assert_eq!(
                    *result.borrow(),
                    vec![match action {
                        "cancel" => 31,
                        "publish" => 152,
                        _ => 52,
                    }]
                );
            }
            drop((first, second, a, b, vm, frame));
            drive();
            drop(scope);
            parent.cancel();
            parent.wait().unwrap();
            drop(parent);
            pool.wait_closed();
            drop(storage);
            drop(guard);
            assert_eq!(budget.used(), 0, "{mode:?}/{action}");
        }
    }
}
#[test]
fn parallel_domains_later_reader_waits_behind_an_earlier_waiting_writer() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let budget = TaskBudget::new(64 << 20);
        let storage = Storage::new(vec![budget.clone()]).unwrap();
        let guard = Guard::enter(&storage);
        let mut pool = ParallelExecutor::new(3, 32).unwrap();
        let parent = pool
            .submitter()
            .publish(
                storage.clone(),
                Arc::new(()),
                Box::new(|| Ok(Box::new(Park))),
            )
            .unwrap();
        let context = BlockingContext::new().unwrap();
        let scope = Scope::enter(parent.domain_spawner(), context.waker());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{error}"));
        let a = vm
            .create_domain("make", &[InstanceValue::Int32(10)])
            .unwrap();
        let b = vm
            .create_domain("make", &[InstanceValue::Int32(20)])
            .unwrap();
        let c = vm
            .create_domain("make", &[InstanceValue::Int32(30)])
            .unwrap();
        let probe = Probe::install();
        let reset = Reset(probe.clone());
        // W1 runs; W2 waits on a; R3 only conflicts with the waiting W2 on c.
        let (mut first, _) = queue(&vm, &[a.write_handle(), b.read_handle()], WRITE);
        let (mut second, _) = queue(&vm, &[c.write_handle(), a.read_handle()], WRITE);
        let (mut third, result) = queue(&vm, &[c.read_handle(), c.read_handle()], READ);
        for _ in 0..3 {
            vm.poll_tasks(128).unwrap();
            drive();
        }
        probe.wait(1);
        std::thread::sleep(Duration::from_millis(20));
        drive();
        assert_eq!(vm.domains.borrow().parallel.flights.len(), 1, "{mode:?}");
        assert_eq!(vm.domains.borrow().parallel.pending.len(), 2, "{mode:?}");
        assert_eq!(probe.state.lock().unwrap().threads.len(), 1, "{mode:?}");
        drop(reset);
        drain(&mut vm);
        for request in [&mut first, &mut second, &mut third] {
            assert!(matches!(
                request.take_result(),
                Some(DomainOutcome::Value(_))
            ));
        }
        // W1: a = 10 + 1 + 20 = 31; W2: c = 30 + 1 + 31 = 62; R3 then reads 62 + 62 + 1.
        assert_eq!(*result.borrow(), vec![125], "{mode:?}");
        drop((first, second, third, a, b, c, vm));
        drive();
        drop(scope);
        parent.cancel();
        parent.wait().unwrap();
        drop(parent);
        pool.wait_closed();
        drop(storage);
        drop(guard);
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

const CREATION: &str = r#"import joky/domain
#state struct State { n:Int32 }
impl State { #every(100000ms) fn tick(&mut self) {self.n+=1} }
fn make()->State {State{n:10}}
fn operation(routes:domain.DirectoryHandle,delta:Int32,reject:Bool) @suspends ->Int32 {
    when "r8probe" (mut room:State=routes.get_or_stage("room",make())) {
        room.n+=delta
        let result:Result<Int32,Int32>=if reject {Err(7)} else {Ok(room.n)}
        result
    }.unwrap()
}"#;
#[test]
fn parallel_domains_staged_creation_is_invisible_until_commit_and_reclaims_cancelled_timers() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for action in ["commit", "cancel", "close", "reject"] {
            let storage = Storage::new(vec![TaskBudget::new(64 << 20)]).unwrap();
            let guard = Guard::enter(&storage);
            let mut pool = ParallelExecutor::new(2, 32).unwrap();
            let parent = pool
                .submitter()
                .publish(
                    storage.clone(),
                    Arc::new(()),
                    Box::new(|| Ok(Box::new(Park))),
                )
                .unwrap();
            let context = BlockingContext::new().unwrap();
            let scope = Scope::enter(parent.domain_spawner(), context.waker());
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(CREATION)
                .unwrap_or_else(|error| panic!("{error}"));
            let mut arena = Arena::default();
            let mut frame = CpsFrame::new(1);
            frame.slots[0] = 2;
            let owner = unsafe {
                directory::poll(
                    &mut arena,
                    &mut frame,
                    &vm.domains,
                    crate::runtime::domain::Operation::DirectoryCreate,
                )
            }
            .unwrap();
            frame.slots[0] = owner;
            let token = unsafe {
                directory::poll(
                    &mut arena,
                    &mut frame,
                    &vm.domains,
                    crate::runtime::domain::Operation::DirectoryHandle,
                )
            }
            .unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            let probe = Probe::install();
            let reset = Reset(probe.clone());
            let output = Rc::new(RefCell::new(vec![0; 3]));
            let mut request = pairs::paths::submit(
                &vm.domains,
                vm.current.as_ref().unwrap().clone(),
                WRITE,
                pairs::paths::Packet {
                    source_key: 0,
                    descriptors: vec![0, 6, 0, 0, token, 0, 0],
                    keys: vec!["room".into()],
                },
                vec![2, u64::from(action == "reject")],
                pairs::ValueOutput {
                    words: output.clone(),
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            vm.poll_tasks(128).unwrap();
            drive();
            probe.wait(1);
            assert_eq!(
                directory::resolve_key(
                    &vm.domains.borrow(),
                    token,
                    &directory::Key::Name("room".into())
                )
                .unwrap(),
                None
            );
            assert_eq!(vm.domains.borrow().entries.len(), 1);
            if action == "cancel" {
                request.cancel();
            }
            if action == "close" {
                unsafe {
                    crate::runtime::buffer::Buffer::release(
                        &mut arena,
                        owner as *mut crate::runtime::buffer::Buffer,
                    )
                }
                .unwrap();
            }
            drop(reset);
            drain(&mut vm);
            let (outcome, rejection) = request.take_access_result().unwrap();
            match action {
                "commit" => {
                    assert!(matches!(outcome, DomainOutcome::Value(_)));
                    assert!(rejection.is_none());
                    assert_eq!(*output.borrow(), vec![0, 12, 0]);
                    assert!(directory::resolve_key(
                        &vm.domains.borrow(),
                        token,
                        &directory::Key::Name("room".into())
                    )
                    .unwrap()
                    .is_some());
                    assert_eq!(vm.task_count(), 1);
                }
                "cancel" => assert!(matches!(outcome, DomainOutcome::Cancelled)),
                "close" => assert_eq!(rejection, Some(access::Rejection::Closed)),
                _ => {
                    assert!(matches!(outcome, DomainOutcome::Value(_)));
                    assert_eq!(*output.borrow(), vec![1, 0, 7]);
                }
            }
            if action != "commit" {
                assert_eq!(vm.task_count(), 0);
                assert!(vm.domains.borrow().entries.is_empty());
            }
            if matches!(action, "cancel" | "reject") {
                assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            }
            if action != "close" {
                unsafe {
                    crate::runtime::buffer::Buffer::release(
                        &mut arena,
                        owner as *mut crate::runtime::buffer::Buffer,
                    )
                }
                .unwrap();
            }
            drop((request, vm, arena));
            drive();
            drop(scope);
            parent.cancel();
            parent.wait().unwrap();
            drop(parent);
            pool.wait_closed();
            drop(guard);
        }
    }
}

fn allocation_budgets(exhaustive: bool) {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let budget = TaskBudget::new(64 << 20);
        let storage = Storage::new(vec![budget.clone()]).unwrap();
        let guard = Guard::enter(&storage);
        let mut pool = ParallelExecutor::new(2, 32).unwrap();
        let parent = pool
            .submitter()
            .publish(
                storage.clone(),
                Arc::new(()),
                Box::new(|| Ok(Box::new(Park))),
            )
            .unwrap();
        let context = BlockingContext::new().unwrap();
        let scope = Scope::enter(parent.domain_spawner(), context.waker());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{error}"));
        let a = vm
            .create_domain("make", &[InstanceValue::Int32(10)])
            .unwrap();
        let b = vm
            .create_domain("make", &[InstanceValue::Int32(20)])
            .unwrap();
        let baseline = vm.domains.borrow().budget.bytes.get();
        let allowances: Box<dyn Iterator<Item = usize>> = if exhaustive {
            Box::new(0..=32768)
        } else {
            Box::new([0, 256, 512, 1024, 2048, 4096, 8192, 32768].into_iter())
        };
        let mut succeeded = false;
        for allowance in allowances {
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: baseline + allowance,
                ..Default::default()
            })
            .unwrap();
            let (mut request, output) = queue(&vm, &[a.write_handle(), b.read_handle()], WRITE);
            drain(&mut vm);
            match request.take_result().unwrap() {
                DomainOutcome::Value(_) => {
                    assert_eq!(*output.borrow(), vec![31]);
                    succeeded = true;
                }
                DomainOutcome::Failed(error) => {
                    assert_eq!(
                        error.code(),
                        ErrorCode::ResourceLimit,
                        "{mode:?}/{allowance}: {error}"
                    );
                    assert_eq!(root_value(&vm, a.read_handle()), 10);
                    assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
                    assert_eq!(
                        vm.domains.borrow().budget.bytes.get(),
                        baseline,
                        "{mode:?}/{allowance}"
                    );
                }
                other => panic!("unexpected domain outcome: {other:?}"),
            }
            if succeeded {
                break;
            }
        }
        assert!(succeeded);
        drop((a, b, vm));
        drive();
        drop(scope);
        parent.cancel();
        parent.wait().unwrap();
        drop(parent);
        pool.wait_closed();
        drop(storage);
        drop(guard);
        assert_eq!(budget.used(), 0);
    }
}
#[test]
fn parallel_domains_allocation_boundaries_preserve_live_roots() {
    allocation_budgets(false);
}
#[test]
#[ignore = "exhaustive: every byte through successful worker preparation, computation and commit"]
fn exhaustive_parallel_domains_allocation_budgets() {
    allocation_budgets(true);
}
