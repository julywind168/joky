use super::{InterpreterLimits, VmEngine, VmMode};
use crate::runtime::{
    driver::block_on,
    executor::{HostExecutor, TaskGroup},
};
use std::time::{Duration, Instant};
const SOURCE: &str = include_str!("../../tests/fixtures/buffered_tcp_split.jk");
mod matrix {
    include!("../../tests/support/buffered_tcp_split.rs");
}
fn clean() {
    crate::runtime::tcp::LIVE.with(|n| assert_eq!(n.get(), 0));
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
fn vm(mode: VmMode, source: &str) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(source).unwrap_or_else(|e| {
        panic!(
            "{mode:?}: {e} {:?}",
            e.span().and_then(|s| e
                .sources()
                .get(s.source_id())
                .map(|f| (f.name(), s.slice(f.text()))))
        )
    });
    vm
}
#[test]
fn net5c_buffered_split_duplex_cache_framing_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, SOURCE);
        for (entry, expected) in [
            ("duplex", 42),
            ("sweep", 42),
            ("cancel", 42),
            ("limits", 42),
            ("main", 0),
        ] {
            assert_eq!(
                block_on(vm.prepare_async(entry, &[]).unwrap())
                    .unwrap_or_else(|e| panic!("{mode:?} {entry}: {e}")),
                expected
            );
            clean();
        }
        let vm = self::vm(mode, &matrix::frames_source());
        assert_eq!(block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(), 0);
        clean();
    }
}
#[test]
fn net5c_buffered_split_preserves_private_cursor_and_terminal_states() {
    struct ResetBudget(usize);
    impl Drop for ResetBudget {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let _reset = ResetBudget(crate::runtime::arena::LIMIT.with(|limit| limit.get()));
    let source = r#"
        import joky/tcp.{type Address, type TcpListener, type TcpStream, PROBE}
        fn main() @suspends -> Int32 {
            var listener = match TcpListener.bind(Address.ipv4(0x7f000001, 0)) { | Ok(value) => value | _ => { return 1 } }
            let address = match listener.local_addr() { | Ok(value) => value | _ => { return 2 } }
            let stream = match TcpStream.connect(address) { | Ok(value) => value | _ => { return 3 } }
            let peer = match listener.accept() { | Ok(value) => value | _ => { return 4 } }
            PROBE(stream)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for probe in [
            "buffered_split_probe",
            "buffered_split_eof_probe",
            "buffered_split_shutdown_probe",
            "buffered_split_messages_probe",
        ] {
            let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
            registry
                .types()
                .standard
                .set(crate::core::imports::tcp_test_library())
                .unwrap_or_else(|_| panic!("Test library precedes resolution"));
            let mut vm = VmEngine::with_mode_and_registry(mode, registry);
            // SAFETY: Production callback implements the declared zero-input Unit CPS ABI.
            unsafe {
                vm.register_suspending_fn(
                    "__joky_task_yield_now",
                    crate::FunctionSignature::new(vec![], crate::Type::Unit),
                    crate::stdlib::task::yield_internal,
                )
                .unwrap();
            }
            vm.load_source(&source.replace("PROBE", probe))
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
            crate::runtime::arena::LIMIT.with(|limit| {
                limit.set(if probe == "buffered_split_messages_probe" {
                    8192
                } else {
                    _reset.0
                })
            });
            assert_eq!(
                block_on(vm.prepare_async("main", &[]).unwrap())
                    .unwrap_or_else(|e| panic!("{mode:?} {probe}: {e}")),
                42
            );
            clean();
        }
    }
}
#[test]
fn net5c_buffered_split_pending_reload_drop_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode, SOURCE);
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("pending", &[]).unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while executor.poll(1).unwrap().polled != 0
            || crate::runtime::tcp::LIVE.with(|n| n.get()) != 3
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
fn cancellation(exhaustive: bool) {
    let mut vm = vm(VmMode::Interpreter, SOURCE);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    executor
        .spawn(&group, vm.prepare_async("handoff", &[]).unwrap())
        .unwrap();
    let mut steps = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while executor.task_count() != 0 {
        let report = executor.poll(1).unwrap();
        steps += report.polled;
        for done in report.completed {
            assert_eq!(done.result, Ok(42));
        }
        assert!(steps < 10000 && Instant::now() < deadline);
    }
    clean();
    for cut in (0..=steps).filter(|n| exhaustive || n % 32 == 0 || *n == steps) {
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("handoff", &[]).unwrap())
            .unwrap();
        let mut polls = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while polls < cut && executor.task_count() != 0 {
            polls += executor.poll(1).unwrap().polled;
            assert!(Instant::now() < deadline);
        }
        executor.cancel_group(&group);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        clean();
    }
}
#[test]
fn net5c_buffered_split_cancel_at_instruction_boundaries() {
    cancellation(false);
}
#[test]
#[ignore = "exhaustive: cancel buffered split, cached frames and independent writer at every instruction"]
fn exhaustive_net5c_buffered_split_cancel_every_instruction() {
    cancellation(true);
}
#[test]
fn net5c_buffered_split_rejects_private_state_moves_and_aliases() {
    for source in [
        "import joky/tcp.{type BufferedTcpReader} fn fake() -> BufferedTcpReader { BufferedTcpReader { stream: 1 } }",
        "import joky/tcp.{type ReceiveBuffer} fn main() {}",
        "import joky/tcp.{type BufferedTcpReader} fn peek(reader: &BufferedTcpReader) { reader.buffer; }",
        "import joky/tcp.{type BufferedTcpReader} fn read(reader: &mut BufferedTcpReader) @suspends { reader.read_message(1); } fn bad(reader: &mut BufferedTcpReader) @suspends { parallel { | read(reader) | read(reader) } }",
        "import joky/tcp.{type BufferedTcp} fn bad(stream: BufferedTcp) @suspends { stream.split(); stream.read(1); }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] { assert!(VmEngine::with_mode(mode).load_source(source).is_err(), "{source}"); }
        let ast = crate::parser::parse(source).unwrap();
        assert!(crate::aot::AotCompiler::new("invalid-buffered-split").unwrap().compile_module(&ast).is_err(), "{source}");
    }
}
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net5c_buffered_split_preserves_real_reset_and_os_code() {
    mod endpoint {
        include!("../../tests/support/buffered_tcp.rs");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for method in [
            "read(4)",
            "read_exact(4)",
            "read_line(4)",
            "read_message(4)",
        ] {
            let (port, peer) = endpoint::reset_endpoint(1);
            let vm = vm(mode, &matrix::reset_source(port, method));
            let result = block_on(vm.prepare_async("main", &[]).unwrap());
            peer.join().unwrap();
            assert_eq!(result.unwrap(), 0, "{mode:?}: {method}");
            clean();
        }
    }
}
