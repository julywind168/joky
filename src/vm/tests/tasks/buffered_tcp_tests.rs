use super::{InterpreterLimits, VmEngine, VmMode};
use crate::runtime::{
    driver::block_on,
    executor::{HostExecutor, TaskGroup},
};
use std::time::{Duration, Instant};

const SOURCE: &str = include_str!("../../../../tests/fixtures/buffered_tcp.jk");
const PROBE: &str = r#"
    import joky/tcp.{type Address, type TcpListener, type TcpStream, buffered_probe}
    fn probe() @suspends -> Int32 {
        var listener = match TcpListener.bind(Address.ipv4(0x7f000001, 0)) { Ok(listener) => listener, Err(error) => { return 1 } }
        let address = match listener.local_addr() { Ok(address) => address, Err(error) => { return 2 } }
        let stream = match TcpStream.connect(address) { Ok(stream) => stream, Err(error) => { return 3 } }
        let peer = match listener.accept() { Ok(stream) => stream, Err(error) => { return 4 } }
        buffered_probe(stream)
    }
"#;
fn vm(mode: VmMode, probe: bool) -> VmEngine {
    let mut vm = if probe {
        let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
        registry
            .types()
            .standard
            .set(crate::core::imports::tcp_test_library())
            .unwrap_or_else(|_| panic!("Test library must precede source resolution"));
        VmEngine::with_mode_and_registry(mode, registry)
    } else {
        VmEngine::with_mode(mode)
    };
    if probe {
        // SAFETY: The production callback implements the zero-input Unit CPS ABI.
        unsafe {
            vm.register_suspending_fn(
                "__joky_task_yield_now",
                crate::FunctionSignature::new(vec![], crate::Type::Unit),
                crate::stdlib::task::yield_internal,
            )
            .unwrap();
        }
    }
    vm.load_source(if probe { PROBE } else { SOURCE })
        .unwrap_or_else(|e| panic!("{mode:?}: {e} at {:?}", e.span()));
    vm
}
fn clean() {
    crate::runtime::tcp::LIVE.with(|count| assert_eq!(count.get(), 0));
    crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
}
#[test]
fn buffered_tcp_reads_and_terminal_states() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for probe in [false, true] {
            let vm = vm(mode, probe);
            let entry = if probe { "probe" } else { "main" };
            let expected = if probe { 42 } else { 0 };
            assert_eq!(
                block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap(),
                expected,
                "{mode:?}: {entry}"
            );
            clean();
        }
    }
}
#[test]
fn buffered_tcp_pending_cancel_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode, false);
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("pending", &[]).unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // A connect may wait before the peer and buffered receiver exist.
        while executor.poll(1).unwrap().polled != 0
            || crate::runtime::tcp::LIVE.with(|count| count.get()) != 3
        {
            assert!(Instant::now() < deadline);
        }
        assert_eq!(executor.task_count(), 1);
        crate::runtime::tcp::LIVE.with(|count| assert_eq!(count.get(), 3));
        vm.load_source("fn unrelated() -> Int32 { 7 }").unwrap();
        drop(vm);
        executor.cancel_group(&group);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        clean();
    }
}
fn cancel_cuts(exhaustive: bool) {
    for probe in [false, true] {
        let mut vm = vm(VmMode::Interpreter, probe);
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        let entry = if probe { "probe" } else { "main" };
        cancel_entry(&vm, entry, if probe { 42 } else { 0 }, exhaustive);
    }
}
fn cancel_entry(vm: &VmEngine, entry: &str, expected: u64, exhaustive: bool) {
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    executor
        .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
        .unwrap();
    let mut steps = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while executor.task_count() != 0 {
        let report = executor.poll(1).unwrap();
        steps += report.polled;
        for done in report.completed {
            assert_eq!(done.result, Ok(expected));
        }
        assert!(
            steps < 20000 && Instant::now() < deadline,
            "cancellation probe exceeded its instruction/deadline safety bound"
        );
    }
    clean();
    for cut in (0..=steps).filter(|cut| exhaustive || cut % 32 == 0 || *cut == steps) {
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
            .unwrap();
        let mut polls = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while polls < cut && executor.task_count() != 0 {
            polls += executor.poll(1).unwrap().polled;
            assert!(Instant::now() < deadline);
        }
        executor.cancel_group(&group);
        clean();
        assert_eq!(executor.poll(1).unwrap().polled, 0);
    }
}

#[test]
fn buffered_tcp_cancel_at_instruction_boundaries() {
    cancel_cuts(false);
}
#[test]
#[ignore = "exhaustive: cancel buffered TCP refill, copy and terminal states at every instruction"]
fn exhaustive_buffered_tcp_cancel_every_instruction() {
    cancel_cuts(true);
}
#[test]
fn buffered_tcp_rejects_forged_state_and_aliases() {
    for source in [
        "import joky/tcp.{type BufferedTcp, type TcpStream} fn forged(stream: TcpStream) -> BufferedTcp { BufferedTcp { stream: stream } }",
        "import joky/tcp.{buffered_probe} fn main() {}",
        "import joky/tcp.{type ReadState} fn main() {}",
        "import joky/tcp.{type BufferedTcp} fn read(b: &mut BufferedTcp) @suspends { b.read(1); } fn both(b: &mut BufferedTcp) @suspends { parallel { | read(b) | read(b) } }",
        "import joky/tcp.{type BufferedTcp} fn peek(b: &BufferedTcp) { b.bytes; }",
        "import joky/tcp.{type BufferedTcp} fn fill(b: &mut BufferedTcp) @suspends { b.fill(); }",
        "import joky/tcp.{type BufferedTcp} import joky/array.{type Array} fn take(b: &mut BufferedTcp, out: &mut Array<UInt8>) { b.take_into(out, 1); }",
        "import joky/tcp.{buffered_frames_probe} fn main() {}",
        "import joky/tcp.{buffered_messages_probe} fn main() {}",
        "import joky/tcp.{message_header} fn main() {}",
        "import joky/tcp.{message_length} fn main() {}",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(source).is_err(), "{source}");
        }
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_remembers_real_connection_reset() {
    mod endpoint {
        include!("../../../../tests/support/buffered_tcp.rs");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (port, peer) = endpoint::reset_endpoint(1);
        let source = include_str!("../../../../tests/fixtures/buffered_tcp_reset.jk")
            .replace("PORT", &port.to_string());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let result = block_on(vm.prepare_async("main", &[]).unwrap());
        peer.join().unwrap();
        assert_eq!(result.unwrap(), 0, "{mode:?}");
        clean();
    }
}

const FRAMES: &str = include_str!("../../../../tests/fixtures/buffered_tcp_frames.jk");
fn frames_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(FRAMES).unwrap_or_else(|e| {
        panic!(
            "{e} {:?}",
            e.span().and_then(|span| e
                .sources()
                .get(span.source_id())
                .map(|file| (file.name(), span.slice(file.text()))))
        )
    });
    vm
}
#[test]
fn buffered_tcp_exact_delimited_and_utf8_reads() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = frames_vm(mode);
        for (entry, expected) in [("main", 0), ("sweep", 42)] {
            assert_eq!(
                block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap(),
                expected
            );
            clean();
        }
        let mut vm = self::vm(mode, true);
        for probe in ["buffered_frames_probe", "buffered_chunks_probe"] {
            vm.load_source(&PROBE.replace("buffered_probe", probe))
                .unwrap_or_else(|e| panic!("{probe}: {e}"));
            assert_eq!(
                block_on(vm.prepare_async("probe", &[]).unwrap()).unwrap(),
                42
            );
            clean();
        }
    }
}
#[test]
fn buffered_tcp_partial_frames_cancel_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for entry in ["pending_exact", "pending_until", "pending_line"] {
            let mut vm = frames_vm(mode);
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while executor.poll(1).unwrap().polled != 0
                || crate::runtime::tcp::LIVE.with(|count| count.get()) != 2
            {
                assert!(Instant::now() < deadline);
            }
            assert_eq!(executor.task_count(), 1);
            vm.load_source("fn unrelated() -> Int32 { 7 }").unwrap();
            drop(vm);
            executor.cancel_group(&group);
            assert_eq!(executor.poll(1).unwrap().polled, 0);
            clean();
        }
    }
}
fn frame_cancel_cuts(exhaustive: bool) {
    let mut vm = frames_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    cancel_entry(&vm, "sweep", 42, exhaustive);
}
#[test]
fn buffered_tcp_cancel_at_frame_instruction_boundaries() {
    frame_cancel_cuts(false);
}
#[test]
#[ignore = "exhaustive: cancel exact, delimited and UTF-8 frame ownership at every instruction"]
fn exhaustive_buffered_tcp_frames_cancel_every_instruction() {
    frame_cancel_cuts(true);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_frame_reads_propagate_real_reset() {
    mod endpoint {
        include!("../../../../tests/support/buffered_tcp.rs");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for method in ["read_exact(4)", "read_until(10, 4)", "read_line(4)"] {
            let (port, peer) = endpoint::reset_endpoint(1);
            let source = include_str!("../../../../tests/fixtures/buffered_tcp_reset.jk")
                .replace("PORT", &port.to_string())
                .replace(".read(4)", &format!(".{method}"));
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
            let result = block_on(vm.prepare_async("main", &[]).unwrap());
            peer.join().unwrap();
            assert_eq!(result.unwrap(), 0, "{mode:?}: {method}");
            clean();
        }
    }
}

const MESSAGES: &str = include_str!("../../../../tests/fixtures/buffered_tcp_messages.jk");
fn messages_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(MESSAGES).unwrap_or_else(|e| {
        panic!(
            "{mode:?}: {e} {:?}",
            e.span().and_then(|span| e
                .sources()
                .get(span.source_id())
                .map(|file| (file.name(), span.slice(file.text()))))
        )
    });
    vm
}
#[test]
fn buffered_tcp_messages_wire_limits_and_truncation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = messages_vm(mode);
        for (entry, expected) in [("main", 0), ("sweep", 42)] {
            assert_eq!(
                block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap(),
                expected,
                "{mode:?}: {entry}"
            );
            clean();
        }
    }
}

#[test]
#[ignore = "exhaustive: transfer maximum-size TCP messages in both VM modes"]
fn exhaustive_buffered_tcp_maximum_messages() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = messages_vm(mode);
        assert_eq!(
            block_on(vm.prepare_async("maximum", &[]).unwrap()).unwrap(),
            42,
            "{mode:?}"
        );
        clean();
    }
}

#[test]
fn buffered_tcp_rejects_message_length_before_payload_allocation() {
    struct ResetBudget(usize);
    impl Drop for ResetBudget {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let _reset = ResetBudget(crate::runtime::arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = self::vm(mode, true);
        vm.load_source(&PROBE.replace("buffered_probe", "buffered_messages_probe"))
            .unwrap_or_else(|e| panic!("{e}"));
        // A four-GiB advertised payload must fail as a business length error,
        // without needing even a one-MiB result allocation or another socket read.
        crate::runtime::arena::LIMIT.with(|limit| limit.set(8192));
        assert_eq!(
            block_on(vm.prepare_async("probe", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
}
#[test]
fn buffered_tcp_partial_messages_cancel_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for entry in ["pending_header", "pending_payload"] {
            let mut vm = messages_vm(mode);
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while executor.poll(1).unwrap().polled != 0
                || crate::runtime::tcp::LIVE.with(|count| count.get()) != 2
            {
                assert!(Instant::now() < deadline);
            }
            assert_eq!(executor.task_count(), 1);
            vm.load_source("fn unrelated() -> Int32 { 7 }").unwrap();
            drop(vm);
            executor.cancel_group(&group);
            assert_eq!(executor.poll(1).unwrap().polled, 0);
            clean();
        }
    }
}
fn message_cancel_cuts(exhaustive: bool) {
    let mut vm = messages_vm(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    cancel_entry(&vm, "sweep", 42, exhaustive);
}
#[test]
fn buffered_tcp_cancel_at_message_instruction_boundaries() {
    message_cancel_cuts(false);
}
#[test]
#[ignore = "exhaustive: cancel message header, payload and send ownership at every instruction"]
fn exhaustive_buffered_tcp_messages_cancel_every_instruction() {
    message_cancel_cuts(true);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_messages_propagate_real_reset() {
    mod endpoint {
        include!("../../../../tests/support/buffered_tcp.rs");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (port, peer) = endpoint::reset_endpoint(1);
        let source = include_str!("../../../../tests/fixtures/buffered_tcp_reset.jk")
            .replace("PORT", &port.to_string())
            .replace(".read(4)", ".read_message(4)");
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let result = block_on(vm.prepare_async("main", &[]).unwrap());
        peer.join().unwrap();
        assert_eq!(result.unwrap(), 0, "{mode:?}");
        clean();
    }
}

#[test]
fn buffered_tcp_echo_program() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../../examples/tcp_messages.jk"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(), 0);
        clean();
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_tcp_backpressure_resumes_and_cancel_closes() {
    // Keep real blocking and cleanup in both modes; interpreted draining is stress coverage.
    message_backpressure(false, &[VmMode::Jit], &[false, true]);
    message_backpressure(false, &[VmMode::Interpreter], &[true]);
}
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn buffered_writer_backpressure_resumes_and_cancel_closes() {
    message_backpressure(true, &[VmMode::Jit], &[false, true]);
    message_backpressure(true, &[VmMode::Interpreter], &[true]);
}
#[test]
#[ignore = "exhaustive: resume large buffered TCP writes in the interpreter"]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exhaustive_buffered_tcp_backpressure_resumes() {
    message_backpressure(false, &[VmMode::Interpreter], &[false]);
}
#[test]
#[ignore = "exhaustive: resume large split TCP writes in the interpreter"]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exhaustive_buffered_writer_backpressure_resumes() {
    message_backpressure(true, &[VmMode::Interpreter], &[false]);
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn message_backpressure(split: bool, modes: &[VmMode], cancellations: &[bool]) {
    mod endpoint {
        include!("../../../../tests/support/tcp_messages.rs");
    }
    thread_local! {
        static SENT: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
        static SEND_LIMIT: std::cell::Cell<i32> = const { std::cell::Cell::new(128) };
    }
    unsafe extern "C" fn sent(count: i32) {
        SENT.with(|value| value.set(count));
    }
    unsafe extern "C" fn send_limit() -> i32 {
        SEND_LIMIT.with(std::cell::Cell::get)
    }
    for &mode in modes {
        for &cancel in cancellations {
            let mut peer = endpoint::peer();
            let mut vm = VmEngine::with_mode(mode);
            // SAFETY: Both callbacks implement their declared scalar ABIs and live for the test.
            unsafe {
                vm.register_fn(
                    "sent",
                    crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Unit),
                    sent as *const u8,
                )
                .unwrap();
                vm.register_fn(
                    "send_limit",
                    crate::FunctionSignature::new(vec![], crate::Type::Int32),
                    send_limit as *const u8,
                )
                .unwrap();
            }
            let source = include_str!("../../../../tests/fixtures/buffered_tcp_pressure.jk")
                .replace("PORT", &peer.port.to_string())
                .replace("COUNT", "send_limit()")
                + "\nextern fn send_limit() -> Int32\n";
            let source = if split {
                source
                    .replace("type BufferedTcp}", "type BufferedTcp, type TcpWriter}")
                    .replace("connection: &mut BufferedTcp", "connection: &mut TcpWriter")
                    .replace(
                        "var connection = BufferedTcp.new(stream)",
                        "var (reader, connection) = BufferedTcp.new(stream).split()",
                    )
            } else {
                source
            };
            vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
            SENT.with(|value| value.set(0));
            SEND_LIMIT.with(|value| value.set(128));
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("main", &[]).unwrap())
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut idle = 0;
            loop {
                let report = executor.poll(1).unwrap();
                assert!(
                    report.completed.is_empty(),
                    "sender finished before observing backpressure"
                );
                if report.polled == 0
                    && peer.accepted.load(std::sync::atomic::Ordering::Acquire)
                    && SENT.with(|value| value.get()) > 0
                {
                    idle += 1;
                } else {
                    idle = 0;
                }
                if idle == 3 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{mode:?} split={split} cancel={cancel}: writer did not wait; sent={}",
                    SENT.with(std::cell::Cell::get),
                );
                if report.polled == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let written = SENT.with(|value| value.get()) as usize;
            assert!(written > 0 && written < 128);
            assert_eq!(executor.task_count(), 1);
            if cancel {
                vm.load_source("fn unrelated() -> Int32 { 7 }").unwrap();
                drop(vm);
                executor.cancel_group(&group);
                assert_eq!(executor.poll(1).unwrap().polled, 0);
            } else {
                // Keep enough input to fill the socket on either platform, but once
                // blocked, finish that message and one subsequent message. Draining
                // all 128 would also time millions of interpreted Array operations
                // under the suite's CPU contention, not just writable resumption.
                let target = (written + 2).min(128);
                SEND_LIMIT.with(|value| value.set(target as i32));
                peer.release();
                let deadline = Instant::now() + Duration::from_secs(120);
                let mut active_polls = 0;
                let mut idle_polls = 0;
                while executor.task_count() != 0 {
                    let report = executor.poll(1).unwrap();
                    active_polls += report.polled;
                    idle_polls += usize::from(report.polled == 0);
                    for done in report.completed {
                        assert_eq!(done.result, Ok(0));
                    }
                    assert!(
                        Instant::now() < deadline,
                        "{mode:?} split={split}: writer did not finish after release; \
                         blocked_after={written}, sent={}, target={target}, \
                         active_polls={active_polls}, idle_polls={idle_polls}",
                        SENT.with(std::cell::Cell::get),
                    );
                    if report.polled == 0 {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
            clean();
            let received = peer.finish();
            if cancel {
                assert_eq!(received.complete, written);
                assert!(received.partial < 65540);
                assert_eq!(executor.poll(1).unwrap().polled, 0);
                clean();
            } else {
                let target = SEND_LIMIT.with(std::cell::Cell::get) as usize;
                assert_eq!(SENT.with(std::cell::Cell::get) as usize, target);
                assert_eq!(received.complete, target);
                assert!(received.complete > written);
                assert_eq!(received.partial, 0);
            }
        }
    }
}
