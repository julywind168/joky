use super::{InterpreterLimits, VmEngine, VmMode};
use crate::runtime::{
    driver::block_on,
    executor::{HostExecutor, TaskGroup},
};
use std::time::{Duration, Instant};

const SOURCE: &str = include_str!("../../tests/fixtures/buffered_tcp.jk");
const PROBE: &str = r#"
    import joky/tcp.{type Address, type TcpListener, type TcpStream, buffered_probe}
    fn probe() @suspends -> Int32 {
        var listener = match TcpListener.bind(Address.ipv4(0x7f000001, 0)) { | Ok(listener) => listener | Err(error) => { return 1 } }
        let address = match listener.local_addr() { | Ok(address) => address | Err(error) => { return 2 } }
        let stream = match TcpStream.connect(address) { | Ok(stream) => stream | Err(error) => { return 3 } }
        let peer = match listener.accept() { | Ok(stream) => stream | Err(error) => { return 4 } }
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
fn net4a_buffered_tcp_reads_and_terminal_states() {
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
fn net4a_buffered_tcp_pending_cancel_reload_and_vm_drop() {
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
        assert!(steps < 10000 && Instant::now() < deadline);
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
fn net4a_buffered_tcp_cancel_at_instruction_boundaries() {
    cancel_cuts(false);
}
#[test]
#[ignore = "exhaustive: cancel buffered TCP refill, copy and terminal states at every instruction"]
fn exhaustive_net4a_buffered_tcp_cancel_every_instruction() {
    cancel_cuts(true);
}
#[test]
fn net4a_buffered_tcp_rejects_forged_state_and_aliases() {
    for source in [
        "import joky/tcp.{type BufferedTcp, type TcpStream} fn forged(stream: TcpStream) -> BufferedTcp { BufferedTcp { stream: stream } }",
        "import joky/tcp.{buffered_probe} fn main() {}",
        "import joky/tcp.{type ReadState} fn main() {}",
        "import joky/tcp.{type BufferedTcp} fn read(b: &mut BufferedTcp) @suspends { b.read(1); } fn both(b: &mut BufferedTcp) @suspends { parallel { | read(b) | read(b) } }",
        "import joky/tcp.{type BufferedTcp} fn peek(b: &BufferedTcp) { b.bytes; }",
        "import joky/tcp.{type BufferedTcp} fn fill(b: &mut BufferedTcp) @suspends { b.fill(); }",
        "import joky/tcp.{type BufferedTcp} import joky/array.{type Array} fn take(b: &mut BufferedTcp, out: &mut Array<UInt8>) { b.take_into(out, 1); }",
        "import joky/tcp.{buffered_frames_probe} fn main() {}",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(source).is_err(), "{source}");
        }
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net4a_buffered_tcp_remembers_real_connection_reset() {
    mod endpoint {
        include!("../../tests/support/buffered_tcp.rs");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (port, peer) = endpoint::reset_endpoint(1);
        let source = include_str!("../../tests/fixtures/buffered_tcp_reset.jk")
            .replace("PORT", &port.to_string());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let result = block_on(vm.prepare_async("main", &[]).unwrap());
        peer.join().unwrap();
        assert_eq!(result.unwrap(), 0, "{mode:?}");
        clean();
    }
}

const FRAMES: &str = include_str!("../../tests/fixtures/buffered_tcp_frames.jk");
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
fn net4b_buffered_tcp_exact_delimited_and_utf8_reads() {
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
fn net4b_buffered_tcp_partial_frames_cancel_reload_and_vm_drop() {
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
fn net4b_buffered_tcp_cancel_at_frame_instruction_boundaries() {
    frame_cancel_cuts(false);
}
#[test]
#[ignore = "exhaustive: cancel exact, delimited and UTF-8 frame ownership at every instruction"]
fn exhaustive_net4b_buffered_tcp_cancel_every_instruction() {
    frame_cancel_cuts(true);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net4b_buffered_tcp_frame_reads_propagate_real_reset() {
    mod endpoint {
        include!("../../tests/support/buffered_tcp.rs");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for method in ["read_exact(4)", "read_until(10, 4)", "read_line(4)"] {
            let (port, peer) = endpoint::reset_endpoint(1);
            let source = include_str!("../../tests/fixtures/buffered_tcp_reset.jk")
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
