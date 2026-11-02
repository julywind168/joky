use super::*;
use crate::runtime::{
    arena::{LIMIT, MAX_BYTES},
    parallel::{Storage, TaskBudget},
};
use std::{
    cell::RefCell,
    sync::mpsc,
    time::{Duration, Instant},
};
const LEGACY: &str =
    "$argon2id$v=19$m=256,t=2,p=1$c29tZXNhbHQ$nf65EOgLrQMR/uIPnA4rEsF5h7TKyQwu9U1bMCHGi/4";
type Hook = Arc<dyn Fn() + Send + Sync>;
thread_local! { static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) }; }
pub(super) fn before_compute() -> Option<Hook> {
    HOOK.with(|h| h.borrow().clone())
}
pub(crate) fn pause_worker() -> impl Drop {
    let (release, resume) = mpsc::channel();
    let resume = Mutex::new(resume);
    HOOK.with(|h| {
        *h.borrow_mut() = Some(Arc::new(move || {
            let _ = lock(&resume).recv_timeout(Duration::from_secs(10));
        }))
    });
    struct Paused(mpsc::Sender<()>);
    impl Drop for Paused {
        fn drop(&mut self) {
            let _ = self.0.send(());
            HOOK.with(|h| h.borrow_mut().take());
        }
    }
    Paused(release)
}

struct HookGuard;
impl Drop for HookGuard {
    fn drop(&mut self) {
        HOOK.with(|h| h.borrow_mut().take());
    }
}
struct Limit(usize);
impl Limit {
    fn new(bytes: usize) -> Self {
        Self(LIMIT.with(|n| n.replace(bytes)))
    }
}
impl Drop for Limit {
    fn drop(&mut self) {
        LIMIT.with(|n| n.set(self.0));
    }
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "password worker stalled");
        thread::yield_now();
    }
}
#[test]
fn password_phc_validation_and_rehash_policy_precede_allocation() {
    let _limit = Limit::new(0);
    assert_eq!(needs_rehash(LEGACY.as_bytes()), 1);
    let current = LEGACY
        .replace("m=256", "m=19456")
        .replace("c29tZXNhbHQ$", "MDEyMzQ1Njc4OWFiY2RlZg$");
    assert_eq!(needs_rehash(current.as_bytes()), 0);
    let stronger = current.replace("m=19456,t=2,p=1", "p=4,t=4,m=65536");
    assert_eq!(needs_rehash(stronger.as_bytes()), 0);
    let prefix = "$argon2id$v=19$m=256,t=2,p=1$c29tZXNhbHQ$";
    for text in [
        String::new(),
        "not a PHC".into(),
        format!("{LEGACY}$extra"),
        LEGACY.replace("v=19$", ""),
        LEGACY.replace("t=2,", ""),
        LEGACY.replace("t=2", "t=2,t=2"),
        LEGACY.replace("p=1", "p=1,data=YWJj"),
        LEGACY.replace("m=256", "m=NaN"),
        LEGACY.replace("m=256", "m=4294967296"),
        LEGACY.replace("c29tZXNhbHQ", "c29tZXNhbHQ="),
        prefix.into(),
        "x".repeat(MAX_ENCODED + 1),
    ] {
        assert!(
            hashing::Plan::parse(text.as_bytes()).is_err(),
            "accepted invalid PHC: {text}"
        );
    }
    for (old, new) in [
        ("m=256", "m=65537"),
        ("m=256", "m=0"),
        ("m=256", "m=7"),
        ("t=2", "t=5"),
        ("t=2", "t=0"),
        ("p=1", "p=5"),
        ("p=1", "p=0"),
    ] {
        assert_eq!(
            hashing::Plan::parse(LEGACY.replace(old, new).as_bytes()).err(),
            Some(PARAMETERS)
        );
    }
    for text in [
        LEGACY.replace("argon2id", "argon2i"),
        LEGACY.replace("v=19", "v=16"),
    ] {
        assert_eq!(needs_rehash(text.as_bytes()), UNSUPPORTED + 2);
    }
    assert_eq!(
        hashing::Plan::generate(|_| Err(())).err(),
        Some(RANDOM_UNAVAILABLE)
    );
    assert_eq!(needs_rehash(&[0xff]), INVALID_HASH + 2);
}
#[test]
fn password_worker_retains_budget_through_running_cancellation() {
    let _serial = lock(&TEST_LOCK);
    let (started, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let resume = Mutex::new(resume);
    HOOK.with(|h| {
        *h.borrow_mut() = Some(Arc::new(move || {
            started.send(()).unwrap();
            lock(&resume).recv_timeout(Duration::from_secs(10)).unwrap();
        }))
    });
    let _hook = HookGuard;
    let budget = TaskBudget::new(2 << 20);
    let root = Storage::new(vec![budget.clone()]).unwrap();
    let mut arena = Arena::with_storage(root.clone());
    let baseline = budget.used();
    let request = Request::start(
        &mut arena,
        b"password",
        hashing::Plan::parse(LEGACY.as_bytes()).unwrap(),
        Permit::acquire().unwrap(),
    )
    .unwrap();
    observed.recv_timeout(Duration::from_secs(10)).unwrap();
    let state = request.state.clone();
    let held = budget.used();
    assert!(held >= (256 * 1024) + STACK_BYTES);
    let audit_budget = budget.clone();
    let audit = thread::spawn(move || {
        wait(|| lock(&state).cancelled);
        assert_eq!(
            audit_budget.used(),
            held,
            "cancel must not refund live worker memory"
        );
        assert_eq!(ACTIVE.load(Ordering::Acquire), 1);
        assert!(lock(&state).wake.is_none());
        release.send(()).unwrap();
    });
    drop(request);
    audit.join().unwrap();
    assert_eq!(budget.used(), baseline);
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    drop((arena, root));
    assert_eq!(budget.used(), 0);
}
#[test]
fn password_worker_bounds_inputs_admission_failures_and_private_copies() {
    let _serial = lock(&TEST_LOCK);
    let mut arena = Arena::default();
    {
        let _limit = Limit::new(1);
        assert!(Request::start(
            &mut arena,
            b"password",
            hashing::Plan::parse(LEGACY.as_bytes()).unwrap(),
            Permit::acquire().unwrap()
        )
        .is_err());
    }
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    assert_eq!(arena.bytes(), 0);
    let permits: Vec<_> = (0..MAX_ACTIVE)
        .map(|_| Permit::acquire().unwrap())
        .collect();
    assert!(Permit::acquire().is_none());
    // A valid request at admission capacity reports Busy without starting a worker.
    let password = Buffer::copy_bytes(&mut arena, b"password").unwrap();
    let encoded = Buffer::copy_bytes(&mut arena, LEGACY.as_bytes()).unwrap();
    let mut code = 0u64;
    let mut frame = CpsFrame::new(4);
    frame.slots.copy_from_slice(&[
        1,
        password.as_ptr() as usize as u64,
        encoded.as_ptr() as usize as u64,
        &mut code as *mut u64 as usize as u64,
    ]);
    let output = unsafe { poll(&mut arena, &mut frame).unwrap().unwrap() };
    assert_eq!(code, BUSY);
    assert!(frame.host_context.is_null());
    unsafe {
        Buffer::release(&mut arena, output.as_ptr()).unwrap();
    }
    drop(permits);
    let baseline = arena.bytes();
    let mut input = b"password".to_vec();
    let request = Request::start(
        &mut arena,
        &input,
        hashing::Plan::parse(LEGACY.as_bytes()).unwrap(),
        Permit::acquire().unwrap(),
    )
    .unwrap();
    input.fill(0);
    wait(|| lock(&request.state).result.is_some());
    let packet = lock(&request.state).result.take().unwrap().unwrap();
    assert_eq!(&packet.bytes[..packet.len], &[1]);
    drop(request);
    assert_eq!(arena.bytes(), baseline);
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    LIMIT.with(|n| assert_eq!(n.get(), MAX_BYTES));
}
#[test]
fn password_generated_cancellation_and_worker_failure_release_storage() {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    use crate::vm::{VmEngine, VmMode};
    let _serial = lock(&TEST_LOCK);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source("import joky/password fn run() @suspends { password.hash(\"private input\").unwrap(); }").unwrap();
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("run", &[]).unwrap())
            .unwrap();
        executor.poll(1).unwrap();
        executor.cancel_group(&group);
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        HOOK.with(|h| {
            *h.borrow_mut() = Some(Arc::new(|| panic!("injected password worker failure")))
        });
        let _hook = HookGuard;
        let error =
            crate::runtime::driver::block_on(vm.prepare_async("run", &[]).unwrap()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::HostError);
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn password_output_allocation_failure_stops_and_refunds_worker() {
    let _serial = lock(&TEST_LOCK);
    let mut arena = Arena::default();
    let password = Buffer::copy_bytes(&mut arena, b"password").unwrap();
    let encoded = Buffer::copy_bytes(&mut arena, LEGACY.as_bytes()).unwrap();
    let baseline = arena.bytes();
    let request = Request::start(
        &mut arena,
        b"password",
        hashing::Plan::parse(LEGACY.as_bytes()).unwrap(),
        Permit::acquire().unwrap(),
    )
    .unwrap();
    wait(|| lock(&request.state).result.is_some());
    let mut code = 99u64;
    let mut frame = CpsFrame::new(4);
    frame.slots.copy_from_slice(&[
        1,
        password.as_ptr() as usize as u64,
        encoded.as_ptr() as usize as u64,
        &mut code as *mut u64 as usize as u64,
    ]);
    frame.host_context = Box::into_raw(Box::new(request)).cast();
    frame.host_stop = Some(stop);
    {
        let _limit = Limit::new(0);
        let error = unsafe { poll(&mut arena, &mut frame) }.unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
    }
    assert_eq!(code, 99, "failed output must not publish success");
    assert!(frame.host_context.is_null());
    assert_eq!(frame.state, FAILED);
    assert_eq!(arena.bytes(), baseline);
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
}
