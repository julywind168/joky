use super::*;
use crate::{
    runtime::{
        driver::block_on,
        executor::{HostExecutor, TaskGroup},
        suspension::Continuation,
    },
    vm::{InterpreterLimits, VmEngine, VmMode},
};
use std::{
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

const SOURCE: &str = include_str!("../../tests/fixtures/dns.jk");
thread_local! { static CAPTURED: std::cell::RefCell<Option<TaskWaker>> = const { std::cell::RefCell::new(None) }; }
unsafe extern "C" fn capture(frame: *mut CpsFrame) -> i32 {
    CAPTURED.with(|w| *w.borrow_mut() = current_waker());
    (*frame).state = DONE;
    0
}
fn waker() -> TaskWaker {
    let executor = HostExecutor::default();
    let task = unsafe { Continuation::from_native(capture, CpsFrame::new(0), Rc::new(())) };
    executor.spawn(&TaskGroup::new(), task).unwrap();
    executor.poll(1).unwrap();
    CAPTURED.with(|w| w.borrow_mut().take().unwrap())
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "DNS test did not make progress");
        thread::yield_now();
    }
}
struct Injected;
impl Injected {
    fn new(service: Arc<Service>) -> Self {
        TEST_SERVICE.with(|s| assert!(s.borrow_mut().replace(service).is_none()));
        Self
    }
}
impl Drop for Injected {
    fn drop(&mut self) {
        TEST_SERVICE.with(|s| s.borrow_mut().take());
    }
}
fn clean() {
    super::super::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    super::super::tcp::LIVE.with(|n| assert_eq!(n.get(), 0));
}
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
    vm
}

#[test]
fn net6_dns_numeric_system_errors_and_tcp_in_both_vm_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in ["numeric", "errors", "resolved", "loopback"] {
            assert_eq!(
                block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap(),
                42,
                "{mode:?}: {entry}"
            );
            clean();
        }
    }
}

#[test]
fn net6_dns_queue_running_and_ready_limits_remain_charged() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let service = Service::new(
        1,
        1,
        2,
        Arc::new(move |_, port| {
            started_tx.send(()).unwrap();
            lock(&release_rx).recv().unwrap();
            Packet::addresses([SocketAddr::from(([127, 0, 0, 1], port))])
        }),
    )
    .unwrap();
    let first = service.submit("first", 80, waker()).unwrap().unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(lock(&first.state).running);
    let queued = service.submit("queued", 80, waker()).unwrap().unwrap();
    assert!(service.submit("full", 80, waker()).unwrap().is_none());
    service.cancel(&queued);
    drop(queued);
    assert_eq!(service.core.outstanding.load(Ordering::Acquire), 1);
    service.cancel(&first);
    drop(first);
    // The running worker still owns its cancelled ticket.
    assert_eq!(service.core.outstanding.load(Ordering::Acquire), 1);
    let second = service.submit("second", 80, waker()).unwrap().unwrap();
    release_tx.send(()).unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_tx.send(()).unwrap();
    wait_until(|| lock(&second.state).result.is_some());
    assert_eq!(service.core.outstanding.load(Ordering::Acquire), 1);
    // A Ready result counts against total admission even with an empty queue.
    let third = service.submit("third", 80, waker()).unwrap().unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(service.submit("full", 80, waker()).unwrap().is_none());
    service.cancel(&second);
    drop(second);
    service.cancel(&third);
    drop(third);
    release_tx.send(()).unwrap();
    wait_until(|| service.core.outstanding.load(Ordering::Acquire) == 0);
    service.close();
    wait_until(|| service.quiescent());
}

#[test]
fn net6_dns_packet_bounds_dedup_scope_and_utf8() {
    let address: SocketAddr = "[fe80::1%7]:443".parse().unwrap();
    let packet = Packet::addresses([address, address]);
    assert_eq!(packet.length, 24);
    assert_eq!(
        u64::from_be_bytes(packet.bytes[16..24].try_into().unwrap()) >> 32,
        7
    );
    assert_eq!(Packet::addresses([]).code, NO_ADDRESSES);
    assert_eq!(
        Packet::addresses((0..65).map(|port| SocketAddr::from(([127, 0, 0, 1], port)))).code,
        LIMIT_EXCEEDED
    );
    let packet = Packet::error(SYSTEM_FAILURE, &"例".repeat(500));
    assert!(packet.length <= ERROR_BYTES);
    assert!(std::str::from_utf8(&packet.bytes[..packet.length]).is_ok());
    for host in [
        "",
        "a\0b",
        "a\nb",
        "a b",
        "例.test",
        "[::1]",
        "a:80",
        "fe80::1%en0",
    ] {
        assert_eq!(immediate(host, 80).unwrap().code, INVALID_INPUT);
    }
    assert_eq!(
        immediate(&"a".repeat(HOST_BYTES + 1), 80).unwrap().code,
        INVALID_INPUT
    );
    assert!(immediate("name_with_underscore.", 80).is_none());
}

#[test]
fn net6_dns_cancel_reload_drop_and_shutdown_do_not_wait_for_resolver() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let service = Arc::new(
            Service::new(
                1,
                2,
                4,
                Arc::new(move |_, port| {
                    started_tx.send(()).unwrap();
                    lock(&release_rx).recv().unwrap();
                    Packet::addresses([SocketAddr::from(([127, 0, 0, 1], port))])
                }),
            )
            .unwrap(),
        );
        let _injected = Injected::new(service.clone());
        let mut vm = vm(mode);
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("pending", &[]).unwrap())
            .unwrap();
        assert_eq!(executor.poll(1).unwrap().polled, 1);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        vm.load_source("fn other() -> Int32 { 7 }").unwrap();
        drop(vm);
        executor.cancel_group(&group);
        clean();
        assert_eq!(service.core.outstanding.load(Ordering::Acquire), 1);
        service.close();
        assert!(!service.quiescent());
        release_tx.send(()).unwrap();
        wait_until(|| service.quiescent());
        assert_eq!(service.core.outstanding.load(Ordering::Acquire), 0);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
    }
}

#[test]
fn net6_dns_fake_errors_results_and_repeated_pending_execute_generated_code() {
    let service = Arc::new(
        Service::new(
            2,
            4,
            8,
            Arc::new(|host, port| match host {
                "empty.test" => Packet::addresses([]),
                "failed.test" => Packet::error(SYSTEM_FAILURE, "controlled failure"),
                "limit.test" => Packet::error(LIMIT_EXCEEDED, "controlled limit"),
                "panic.test" => panic!("controlled resolver panic"),
                _ => Packet::addresses(
                    [SocketAddr::V6(std::net::SocketAddrV6::new(
                        std::net::Ipv6Addr::LOCALHOST,
                        port,
                        0,
                        7,
                    )); 2],
                ),
            }),
        )
        .unwrap(),
    );
    let _injected = Injected::new(service.clone());
    let source = r#"
        import joky/dns
        import joky/tcp.{type Address}
        fn main() @suspends -> Int32 {
            if !match dns.lookup("empty.test", 80) { Err(e) => match e.kind { NoAddresses => true, _ => false }, Ok(_) => false } { return 1 }
            if !match dns.lookup("failed.test", 80) { Err(e) => match e.kind { SystemFailure => true, _ => false }, Ok(_) => false } { return 2 }
            if !match dns.lookup("limit.test", 80) { Err(e) => match e.kind { LimitExceeded => true, _ => false }, Ok(_) => false } { return 3 }
            if !match dns.lookup("panic.test", 80) { Err(e) => match e.kind { SystemFailure => true, _ => false }, Ok(_) => false } { return 4 }
            let addresses = match dns.lookup("ok.test", 80) { Ok(addresses) => addresses, Err(_) => { return 6 } }
            if addresses.len() != 1 { return 5 }
            if addresses.get(0).unwrap_or(Address.ipv4(0, 0)).scope_id() != 7 { return 7 }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
    service.close();
    wait_until(|| service.quiescent());
}

#[test]
fn net6_dns_busy_is_a_business_result_in_both_vm_modes() {
    let service = Arc::new(Service::new(1, 0, 1, Arc::new(|_, _| unreachable!())).unwrap());
    let _injected = Injected::new(service.clone());
    let source = r#"
        import joky/dns
        fn main() @suspends -> Int32 {
            match dns.lookup("busy.test", 80) {
                Err(error) => match error.kind { Busy => 42, _ => 1 }
                Ok(_) => 2
            }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
    assert_eq!(service.core.outstanding.load(Ordering::Acquire), 0);
    service.close();
    wait_until(|| service.quiescent());
}

// Pending resolver cancellation, reload and cleanup are covered directly above.
// Instruction-prefix replay is useful as a full sweep, not an arbitrary stride.
#[test]
#[ignore = "exhaustive: DNS input/result and cancellation at every interpreter instruction"]
fn exhaustive_net6_dns_cancel_every_instruction() {
    let service = Arc::new(
        Service::new(
            1,
            64,
            128,
            Arc::new(|_, port| Packet::addresses([SocketAddr::from(([127, 0, 0, 1], port))])),
        )
        .unwrap(),
    );
    let _injected = Injected::new(service.clone());
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    for entry in ["numeric", "errors", "pending", "reuse"] {
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
            .unwrap();
        let mut steps = 0;
        wait_until(|| {
            let report = executor.poll(1).unwrap();
            steps += report.polled;
            for done in report.completed {
                assert_eq!(done.result, Ok(42));
            }
            executor.task_count() == 0
        });
        clean();
        for cut in 0..=steps {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
                .unwrap();
            let mut polls = 0;
            wait_until(|| {
                polls += executor.poll(1).unwrap().polled;
                polls >= cut || executor.task_count() == 0
            });
            executor.cancel_group(&group);
            clean();
            wait_until(|| service.core.outstanding.load(Ordering::Acquire) == 0);
        }
    }
    service.close();
    wait_until(|| service.quiescent());
}
#[test]
fn net6_dns_rejects_forged_runtime_signatures() {
    extern "C" fn forbidden_registration() {}
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for name in ["__dns_lookup", "__joky_dns_poll", "joky_dns_shutdown"] {
            assert!(unsafe {
                vm.register_fn(
                    name,
                    FunctionSignature::new(vec![], Type::Unit),
                    forbidden_registration as *const u8,
                )
            }
            .is_err());
        }
        for source in [
            "extern fn __dns_lookup(host: UInt64) @suspends -> UInt64 fn main() @suspends -> UInt64 { __dns_lookup(1) }",
            "fn __dns_lookup() {}",
            "fn __joky_dns_poll() {}",
            "fn joky_dns_shutdown() {}",
            "import joky/dns.{word} fn main() {}",
        ] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(vm.load_source(source).is_err(), "{source}");
        }
    }
}

#[test]
fn net6_dns_race_cancelled_lookup_does_not_cancel_next_lookup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let service = Arc::new(
            Service::new(
                2,
                4,
                8,
                Arc::new(move |host, port| {
                    if host == "controlled.test" {
                        started_tx.send(()).unwrap();
                        lock(&release_rx).recv().unwrap();
                    }
                    Packet::addresses([SocketAddr::from(([127, 0, 0, 1], port))])
                }),
            )
            .unwrap(),
        );
        let _injected = Injected::new(service.clone());
        let vm = vm(mode);
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("reuse", &[]).unwrap())
            .unwrap();
        executor.poll(1).unwrap();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        wait_until(|| {
            let report = executor.poll(1).unwrap();
            for done in report.completed {
                assert_eq!(done.result, Ok(42));
            }
            executor.task_count() == 0
        });
        clean();
        // Result delivery can precede the worker dropping its request identity.
        wait_until(|| service.core.outstanding.load(Ordering::Acquire) == 1);
        release_tx.send(()).unwrap();
        wait_until(|| service.core.outstanding.load(Ordering::Acquire) == 0);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        service.close();
        wait_until(|| service.quiescent());
    }
}

thread_local! { static PROBE_ARENA: std::cell::Cell<*mut Arena> = const { std::cell::Cell::new(std::ptr::null_mut()) }; }
unsafe extern "C" fn probe(frame: *mut CpsFrame) -> i32 {
    let arena = PROBE_ARENA.with(|a| a.get());
    match poll(&mut *arena, &mut *frame) {
        Ok(Some(owner)) => {
            (*frame).result = owner.as_ptr() as usize as u64;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::super::error::record_error(error);
            super::super::cps::RUNTIME_ERROR
        }
    }
}

#[test]
fn net6_dns_copy_peak_failure_stops_and_refunds_request() {
    let service = Arc::new(
        Service::new(
            1,
            2,
            4,
            Arc::new(|_, port| Packet::addresses([SocketAddr::from(([127, 0, 0, 1], port))])),
        )
        .unwrap(),
    );
    let _injected = Injected::new(service.clone());
    let budget = std::sync::Arc::new(super::super::arena::Budget {
        bytes: (0).into(),
        limit: (8192).into(),
        ..Default::default()
    });
    let mut arena = Arena::with_budget(budget.clone(), None);
    let input = Buffer::copy_bytes(&mut arena, b"copy.test").unwrap();
    let base = arena.bytes();
    budget.limit.set(
        base + std::mem::size_of::<Request>()
            + std::mem::size_of::<Ticket>()
            + 2 * std::mem::size_of::<usize>()
            + "copy.test".len(),
    );
    PROBE_ARENA.with(|a| a.set(&mut arena));
    let mut code = 99u64;
    let mut frame = CpsFrame::new(3);
    frame.slots = vec![
        input.as_ptr() as usize as u64,
        443,
        &mut code as *mut u64 as usize as u64,
    ];
    let task = unsafe { Continuation::from_native(probe, frame, Rc::new(())) };
    let error = block_on(task).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert_eq!(code, 99);
    assert_eq!(arena.bytes(), base);
    assert_eq!(budget.bytes.get(), base);
    wait_until(|| service.core.outstanding.load(Ordering::Acquire) == 0);
    unsafe {
        Buffer::release(&mut arena, input.as_ptr()).unwrap();
    }
    assert_eq!(budget.bytes.get(), 0);
    PROBE_ARENA.with(|a| a.set(std::ptr::null_mut()));
    drop(arena);
    clean();
    service.close();
    wait_until(|| service.quiescent());
}

#[test]
fn net6_dns_slow_lookup_keeps_timer_and_other_root_progressing() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let service = Arc::new(
            Service::new(
                1,
                1,
                2,
                Arc::new(move |_, port| {
                    started_tx.send(()).unwrap();
                    lock(&release_rx).recv().unwrap();
                    Packet::addresses([SocketAddr::from(([127, 0, 0, 1], port))])
                }),
            )
            .unwrap(),
        );
        let _injected = Injected::new(service.clone());
        let mut vm = vm(mode);
        vm.load_source(&format!(
            "{SOURCE}\nfn timer() @suspends -> Int32 {{ task.sleep_ms(1); 7 }}"
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        let dns = executor
            .spawn(&group, vm.prepare_async("pending", &[]).unwrap())
            .unwrap();
        executor.poll(1).unwrap();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let timer = executor
            .spawn(&group, vm.prepare_async("timer", &[]).unwrap())
            .unwrap();
        let numeric = executor
            .spawn(&group, vm.prepare_async("numeric", &[]).unwrap())
            .unwrap();
        let mut done = std::collections::BTreeMap::new();
        wait_until(|| {
            for result in executor.poll(8).unwrap().completed {
                done.insert(result.id, result.result);
            }
            done.contains_key(&timer) && done.contains_key(&numeric)
        });
        assert_eq!(done[&timer], Ok(7));
        assert_eq!(done[&numeric], Ok(42));
        assert!(!done.contains_key(&dns));
        release_tx.send(()).unwrap();
        // Exercise DNS completion through the kernel-waker blocking wait too.
        while executor.task_count() != 0 {
            let report = executor.poll(8).unwrap();
            for result in report.completed {
                assert_eq!(result.result, Ok(42));
            }
            executor.wait().unwrap();
        }
        clean();
        service.close();
        wait_until(|| service.quiescent());
    }
}

#[test]
fn tcp_host_endpoints_in_local_and_parallel_vm_modes() {
    use crate::runtime::parallel::{ParallelExecutor, TaskBudget};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/tcp_endpoints.jk"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(), 0);
        clean();
        let budget = TaskBudget::new(8 << 20);
        let prepared = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap_or_else(|error| panic!("{error}"));
        drop(vm);
        let mut pool = ParallelExecutor::new(2, 32).unwrap();
        let task = prepared.submit(&pool).unwrap();
        wait_until(|| task.is_finished());
        task.take_result().unwrap_or_else(|error| panic!("{error}"));
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn tcp_host_endpoints_try_candidates_and_preserve_last_error() {
    let service = Arc::new(
        Service::new(
            1,
            8,
            16,
            Arc::new(|host, port| {
                if host == "failed.test" {
                    return Packet::error(SYSTEM_FAILURE, "controlled resolution failure");
                }
                let first = if host == "connect.test" {
                    "::1"
                } else {
                    "2001:db8::1"
                };
                Packet::addresses([
                    SocketAddr::new(first.parse().unwrap(), port),
                    SocketAddr::from(([127, 0, 0, 1], port)),
                ])
            }),
        )
        .unwrap(),
    );
    let _injected = Injected::new(service.clone());
    let source = r#"
        import joky/tcp
        import joky/check
        fn receive(listener: &mut tcp.TcpListener) @suspends {
            var stream = listener.accept()!
            check.assert(stream.read(1)![0] == 42, "fallback stream is usable")
        }
        fn send(port: UInt16) @suspends {
            var stream = tcp.connect("connect.test", port)!
            stream.write_all([42])!
        }
        fn main() @suspends -> Int32 {
            var listener = tcp.listen("listen.test", 0)!
            let address = listener.local_addr()!
            check.assert(!address.is_ipv6(), "listen falls back to IPv4")
            check.assert(match tcp.listen("listen.test", address.port()) {
                Err(Io(error)) => match error.kind() { AddrInUse => true, _ => false },
                _ => false
            }, "all failed candidates return the last socket error")
            check.assert(match tcp.connect("failed.test", 80) {
                Err(Resolve(error)) => error.message.equals("controlled resolution failure"),
                _ => false
            }, "DNS failure preserves its diagnostic")
            join { | receive(&mut listener) | send(address.port()) }
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
    service.close();
    wait_until(|| service.quiescent());
    assert_eq!(service.core.outstanding.load(Ordering::Acquire), 0);
}

unsafe extern "C" fn install_parallel_service(frame: *mut CpsFrame) -> i32 {
    // The test and registered host owner retain this Arc until all workers close.
    let pointer = (&(*frame).slots)[0] as usize as *const Service;
    Arc::increment_strong_count(pointer);
    TEST_SERVICE.with(|slot| slot.replace(Some(Arc::from_raw(pointer))));
    0
}

#[test]
fn net6_dns_parallel_completion_cancellation_and_pool_close() {
    use crate::runtime::parallel::{ParallelExecutor, TaskBudget};
    use crate::vm::{InstanceValue, TaskValue};
    let source = r#"
        import joky/dns
        import joky/task
        extern fn install(service: UInt64) @suspends
        fn lookup(service: &UInt64) @suspends -> Int32 {
            install(*service)
            let addresses = dns.lookup("controlled.test", 443)!
            if addresses.len() == 1 { 42 } else { 1 }
        }
        fn pending(service: UInt64) @suspends -> Int32 {
            let (value, _) = join { | lookup(&service) | task.yield_now() }
            value
        }
        fn next(service: UInt64) @suspends -> Int32 {
            install(service)
            dns.lookup("next.test", 80)!
            7
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let service = Arc::new(
            Service::new(
                2,
                8,
                16,
                Arc::new(move |host, port| {
                    if host == "controlled.test" {
                        started_tx.send(()).unwrap();
                        lock(&release_rx).recv().unwrap();
                    }
                    Packet::addresses([SocketAddr::from(([127, 0, 0, 1], port))])
                }),
            )
            .unwrap(),
        );
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "install",
                FunctionSignature::new(vec![Type::UInt64], Type::Unit),
                install_parallel_service,
                1,
                service.clone(),
            )
            .unwrap();
        }
        vm.load_source(source)
            .unwrap_or_else(|error| panic!("{error}"));
        let argument =
            TaskValue::Scalar(InstanceValue::UInt64(Arc::as_ptr(&service) as usize as u64));
        for (workers, action) in [1, 3]
            .into_iter()
            .flat_map(|workers| ["complete", "cancel", "close"].map(|action| (workers, action)))
        {
            let budget = TaskBudget::new(8 << 20);
            let mut pool = ParallelExecutor::new(workers, 16).unwrap();
            let task = vm
                .prepare_parallel_task("pending", vec![argument.clone()], vec![budget.clone()])
                .unwrap_or_else(|error| panic!("{error}"))
                .submit(&pool)
                .unwrap();
            started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(!task.is_finished());
            // One blocked resolver must not pin the only task worker.
            let next = vm
                .prepare_parallel_task("next", vec![argument.clone()], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait_until(|| next.is_finished());
            assert_eq!(
                next.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::Int32(7))
            );
            drop(next);
            if action == "complete" {
                release_tx.send(()).unwrap();
                wait_until(|| task.is_finished());
                assert_eq!(
                    task.take_result().unwrap().value(),
                    &TaskValue::Scalar(InstanceValue::Int32(42))
                );
            } else {
                if action == "close" {
                    pool.request_close();
                } else {
                    task.request_cancel();
                }
                wait_until(|| task.is_finished());
                assert_eq!(
                    task.take_result().err().unwrap().code(),
                    ErrorCode::Cancelled
                );
                // Cancellation refunds task storage, but a running OS request retains its quota.
                wait_until(|| service.core.outstanding.load(Ordering::Acquire) == 1);
            }
            let observed = crate::runtime::parallel::scoped::observations(task.identity());
            let sender = observed.iter().find(|o| o.source).unwrap();
            assert!(observed
                .iter()
                .any(|o| !o.source && (workers == 1 || o.thread != sender.thread)));
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}: {action}");
            if action != "complete" {
                release_tx.send(()).unwrap();
            }
            wait_until(|| service.core.outstanding.load(Ordering::Acquire) == 0);
        }
        service.close();
        wait_until(|| service.quiescent());
    }
}
