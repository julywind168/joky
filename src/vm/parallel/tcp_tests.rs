use super::*;
use crate::runtime::parallel::ParallelExecutor;
use crate::vm::{VmEngine, VmMode};
use std::{
    collections::HashSet,
    sync::Mutex,
    thread::ThreadId,
    time::{Duration, Instant},
};
static THREADS: Mutex<Vec<(ThreadId, usize)>> = Mutex::new(Vec::new());
unsafe extern "C" fn touched(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
    let reactor = crate::runtime::executor::current_reactor().unwrap();
    THREADS.lock().unwrap().push((
        std::thread::current().id(),
        Arc::as_ptr(&reactor.signal()) as usize,
    ));
    0
}
fn wait(task: &ParallelTask) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !task.is_finished() {
        assert!(
            Instant::now() < deadline,
            "parallel TCP task did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn parallel_tcp_moves_connected_streams_and_halves_across_workers() {
    for (mode, workers) in [
        (VmMode::Jit, 3),
        (VmMode::Interpreter, 3),
        (VmMode::Interpreter, 1),
    ] {
        let mut vm = VmEngine::with_mode(mode);
        THREADS.lock().unwrap().clear();
        unsafe {
            vm.register_parallel_suspending_fn(
                "touched",
                FunctionSignature::new(vec![], Type::Unit),
                touched,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(31),
                ..Default::default()
            })
            .unwrap();
        }
        let source = include_str!("../../../tests/fixtures/parallel_tcp.jk")
            .replace("fn main()", "extern fn touched() @suspends\nfn main()")
            .replace("var writer = writer", "touched(); var writer = writer")
            .replace("var reader = reader", "touched(); var reader = reader");
        vm.load_source(&source).unwrap();
        let budget = TaskBudget::new(8 << 20);
        let prepared = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(workers, 32).unwrap();
        let task = prepared.submit(&pool).unwrap();
        wait(&task);
        task.take_result().unwrap();
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
        let observations = THREADS.lock().unwrap();
        let threads: HashSet<_> = observations.iter().map(|(thread, _)| *thread).collect();
        let reactors: HashSet<_> = observations.iter().map(|(_, reactor)| *reactor).collect();
        assert_eq!(
            reactors.len(),
            1,
            "all workers must share the same I/O driver and quota"
        );
        assert_eq!(
            threads.len(),
            workers,
            "TCP must actually run on every worker"
        );
        assert!(!threads.contains(&std::thread::current().id()));
    }
}

#[test]
fn parallel_tcp_cancel_and_pool_close_drain_io_and_refund_owners() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../tests/fixtures/parallel_tcp.jk"))
            .unwrap();
        for close in [false, true] {
            let budget = TaskBudget::new(8 << 20);
            let prepared = vm
                .prepare_parallel_task("parked", vec![], vec![budget.clone()])
                .unwrap();
            let mut pool = ParallelExecutor::new(2, 32).unwrap();
            let task = prepared.submit(&pool).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let reactor = loop {
                if let Some(reactor) = pool.reactor() {
                    if reactor.registration_count() == 2 && reactor.socket_count() == 2 {
                        break reactor;
                    }
                }
                assert!(
                    !task.is_finished(),
                    "parked task finished: {:?}",
                    task.take_result().err()
                );
                assert!(
                    Instant::now() < deadline,
                    "two read operations did not park"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            if close {
                pool.request_close();
            } else {
                task.request_cancel();
            }
            wait(&task);
            assert_eq!(
                task.take_result().err().unwrap().code(),
                ErrorCode::Cancelled
            );
            drop(task);
            assert_eq!(reactor.registration_count(), 0);
            assert_eq!(reactor.socket_count(), 0);
            if !close {
                // Cancelling one tree leaves the pool and its shared driver usable.
                let next = vm
                    .prepare_parallel_task("cancel_read", vec![], vec![budget.clone()])
                    .unwrap()
                    .submit(&pool)
                    .unwrap();
                wait(&next);
                next.take_result().unwrap();
                drop(next);
            }
            pool.wait_closed();
            reactor.signal().wake().unwrap();
            assert!(reactor.poll(Some(Duration::ZERO)).is_err());
            assert!(pool.is_closed());
            assert_eq!(reactor.registration_count(), 0);
            assert_eq!(reactor.socket_count(), 0);
            assert_eq!(budget.used(), 0, "{mode:?}, close={close}");
        }
    }
}

#[test]
fn parallel_tcp_budget_failures_reclaim_published_and_unpublished_resources() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../tests/fixtures/parallel_tcp.jk"))
            .unwrap();
        let mut success = false;
        // Exponential samples cover preparation, worker construction and live
        // network owners. Stop at the first successful, fully cleaned run.
        for bytes in [0, 4096, 16384, 65536, 262144, 1048576, 8388608] {
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(2, 32).unwrap();
            let result = vm
                .prepare_parallel_task("cancel_read", vec![], vec![budget.clone()])
                .and_then(|prepared| prepared.submit(&pool))
                .and_then(|task| {
                    wait(&task);
                    task.take_result()
                });
            success = result.is_ok();
            if let Err(error) = &result {
                assert_eq!(
                    error.code(),
                    ErrorCode::ResourceLimit,
                    "{mode:?}: {error:?}"
                );
            }
            drop(result);
            let reactor = pool.reactor();
            pool.wait_closed();
            if let Some(reactor) = reactor {
                assert_eq!(reactor.registration_count(), 0);
                assert_eq!(reactor.socket_count(), 0);
            }
            assert_eq!(budget.used(), 0, "{mode:?}, bytes={bytes}");
            if success {
                break;
            }
        }
        assert!(success);
        let budget = TaskBudget::new(8 << 20);
        let mut pool = ParallelExecutor::new(2, 32).unwrap();
        let task = vm
            .prepare_parallel_task("allocation_failure", vec![], vec![budget.clone()])
            .unwrap()
            .submit(&pool)
            .unwrap();
        wait(&task);
        assert_eq!(
            task.take_result().err().unwrap().code(),
            ErrorCode::ResourceLimit
        );
        drop(task);
        let reactor = pool.reactor().expect("failure must occur after connecting");
        pool.wait_closed();
        assert_eq!(reactor.socket_count(), 0);
        assert_eq!(reactor.registration_count(), 0);
        assert_eq!(budget.used(), 0);
        assert!(vm
            .prepare_parallel_task("connected", vec![], vec![])
            .is_err());
    }
}

#[test]
fn parallel_tcp_pool_close_interrupts_real_write_backpressure() {
    use std::{io::Read, net::TcpListener, sync::mpsc};
    let source = r#"
        import joky/tcp.{type Address, type TcpStream}
        import joky/array.{type Array}
        struct Settings { port: UInt16, byte: UInt8 }
        fn fill(settings: &Settings) @suspends {
            var stream = match TcpStream.connect(Address.ipv4(0x7f000001, settings.port)) {
                Ok(stream) => stream, Err(_) => panic("connect")
            }
            var hello = Array.new::<UInt8>()
            hello.push(settings.byte)
            if stream.write_all(hello).is_err() { panic("hello") }
            while true {
                var bytes = Array.with_capacity::<UInt8>(16384)
                var i: UInt64 = 0
                while i < 16384 { bytes.push(7); i += 1 }
                if stream.write_all(bytes).is_err() { panic("write") }
            }
        }
        fn main(port: UInt16) @suspends {
            let settings = Settings { port, byte: 42 }
            let view = &settings
            scope { branch { fill(view) } }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [42]);
            sender.send(stream).unwrap();
        });
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let budget = TaskBudget::new(8 << 20);
        let prepared = vm
            .prepare_parallel_task(
                "main",
                vec![TaskValue::Scalar(InstanceValue::UInt16(port))],
                vec![budget.clone()],
            )
            .unwrap();
        let mut pool = ParallelExecutor::new(3, 8).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let stream = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        peer.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let reactor = loop {
            let reactor = pool.reactor().unwrap();
            if task.handle.waiting() && reactor.registration_count() == 1 {
                break reactor;
            }
            assert!(
                !task.is_finished(),
                "write task failed: {:?}",
                task.take_result().err()
            );
            assert!(
                Instant::now() < deadline,
                "writer never reached backpressure"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        let observations = crate::runtime::parallel::scoped::observations(task.identity());
        let source = observations.iter().find(|o| o.source).unwrap();
        assert!(observations
            .iter()
            .any(|o| !o.source && o.thread != source.thread));
        // The peer is still connected and intentionally does not drain payloads.
        pool.request_close();
        wait(&task);
        pool.wait_closed();
        assert_eq!(
            task.take_result().err().unwrap().code(),
            ErrorCode::Cancelled
        );
        drop((task, stream));
        assert_eq!(reactor.registration_count(), 0);
        assert_eq!(reactor.socket_count(), 0);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scoped_borrow_tcp_calls_dispatch_and_drain_pending_io() {
    use crate::runtime::parallel::scoped;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../tests/fixtures/scoped_borrow_tcp.jk"))
            .unwrap_or_else(|error| panic!("{error}"));
        for workers in [1, 3] {
            let budget = TaskBudget::new(8 << 20);
            let mut pool = ParallelExecutor::new(workers, 32).unwrap();
            let task = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait(&task);
            task.take_result().unwrap();
            let tree = scoped::tree_observations(task.identity());
            assert!(tree.iter().any(|o| o.source && o.parent != task.identity()));
            let roots = scoped::observations(task.identity());
            let sender = roots.iter().find(|o| o.source).unwrap();
            assert!(roots.iter().any(|o| !o.source));
            if workers > 1 {
                assert!(roots.iter().any(|o| !o.source && o.thread != sender.thread));
            }
            drop(task);
            // Observe actual reactor registrations, not merely entry into the
            // function: both accepts/reads must be Pending before cancellation.
            for entry in ["parked_accept", "parked_read"] {
                // Each accept also reserves a permit for its future socket.
                let sockets = if entry == "parked_accept" { 4 } else { 2 };
                let task = vm
                    .prepare_parallel_task(entry, vec![], vec![budget.clone()])
                    .unwrap()
                    .submit(&pool)
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(20);
                let reactor = loop {
                    if let Some(reactor) = pool.reactor() {
                        if reactor.registration_count() == 2 && reactor.socket_count() == sockets {
                            break reactor;
                        }
                    }
                    assert!(
                        !task.is_finished(),
                        "{entry}: {:?}",
                        task.take_result().err()
                    );
                    assert!(Instant::now() < deadline, "{entry} did not park");
                    std::thread::sleep(Duration::from_millis(1));
                };
                let observations = scoped::observations(task.identity());
                assert!(observations.iter().any(|o| !o.source));
                if entry == "parked_read" {
                    pool.request_close();
                } else {
                    task.request_cancel();
                }
                wait(&task);
                assert_eq!(
                    task.take_result().err().unwrap().code(),
                    ErrorCode::Cancelled
                );
                drop(task);
                assert_eq!(reactor.registration_count(), 0);
                assert_eq!(reactor.socket_count(), 0);
            }
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}/{workers}");
        }
    }
}

#[test]
fn scoped_borrow_tcp_budget_failures_refund_readers_and_io() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../tests/fixtures/scoped_borrow_tcp.jk"))
            .unwrap_or_else(|error| panic!("{error}"));
        let mut success = false;
        for bytes in [0, 4096, 16384, 65536, 262144, 1048576, 8388608] {
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let result = vm
                .prepare_parallel_task("allocation_budget", vec![], vec![budget.clone()])
                .and_then(|prepared| prepared.submit(&pool))
                .and_then(|task| {
                    wait(&task);
                    task.take_result()
                });
            success = result.is_ok();
            if let Err(error) = &result {
                assert_eq!(
                    error.code(),
                    ErrorCode::ResourceLimit,
                    "{mode:?}/{bytes}: {error}"
                );
            }
            drop(result);
            let reactor = pool.reactor();
            pool.wait_closed();
            if let Some(reactor) = reactor {
                assert_eq!(reactor.registration_count(), 0);
                assert_eq!(reactor.socket_count(), 0);
            }
            assert_eq!(budget.used(), 0, "{mode:?}/{bytes}");
            if success {
                break;
            }
        }
        assert!(success);
    }
}

#[test]
fn parallel_tcp_borrowed_groups_stay_local_but_reference_packets_are_rejected() {
    let mut vm = VmEngine::with_mode(VmMode::Jit);
    vm.load_source(
        r#"
        import joky/tcp.{type TcpListener, type TcpStream}
        import joky/drop.{type Drop}
        fn inspect(listener: &TcpListener) @suspends {}
        fn shared(listener: TcpListener) @suspends {
            join { | inspect(&listener) | inspect(&listener) }
        }
        fn capture(listener: &TcpListener) @suspends { scope { branch { inspect(listener) } } }
        fn captured_owner(listener: TcpListener) @suspends { capture(&listener) }
        struct Tracked { stream: TcpStream }
        impl Drop for Tracked { fn drop(&mut self) {} }
        fn consume(value: Tracked) @suspends {}
        fn custom(value: Tracked) @suspends {
            scope { branch { consume(value) } }
        }
    "#,
    )
    .unwrap();
    vm.parallel_signature("shared").unwrap();
    for entry in ["captured_owner", "custom"] {
        assert!(vm.parallel_signature(entry).is_err(), "{entry}");
    }
}
