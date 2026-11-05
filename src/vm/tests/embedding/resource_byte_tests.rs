use super::{VmEngine, VmMode};
use crate::runtime::byte_request::ByteIoKind;
mod host {
    use crate as joky;
    include!("../../../../tests/fixtures/resource_bytes_host.rs");
}
const SOURCE: &str = include_str!("../../../../tests/fixtures/resource_bytes.jk");
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.register_resource_type("Socket").unwrap();
    unsafe {
        vm.register_resource_factory("byte_make", "Socket", host::FACTORY, None)
            .unwrap();
        vm.register_resource_io("read", "Socket", ByteIoKind::Read, host::READ, None)
            .unwrap();
        vm.register_resource_io("write", "Socket", ByteIoKind::Write, host::WRITE, None)
            .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span().and_then(|s| s.slice(SOURCE))));
    vm
}
fn clean() {
    host::check();
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn resource_bytes_move_binary_data_and_hold_permission_until_stop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in [
            "receive",
            "eof",
            "send",
            "empty_write",
            "zero_read",
            "oversized",
            "bad_count",
            "zero_progress",
            "cancel_reuse",
            "unstarted",
            "field",
            "indirect",
            "independent",
            "dynamic",
        ] {
            for cancel in [false, true] {
                host::reset();
                let result: Result<Option<u64>, crate::Error> = (|| {
                    let mut task = vm.call_async(entry, &[])?;
                    if cancel {
                        return Ok(None);
                    }
                    for _ in 0..50 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume()?;
                    }
                    assert!(task.is_finished(), "{mode:?}: {entry}");
                    Ok(task.cps_result())
                })();
                if !cancel {
                    let fails =
                        ["zero_read", "oversized", "bad_count", "zero_progress"].contains(&entry);
                    assert_eq!(
                        result.is_err(),
                        fails,
                        "{mode:?}: {entry}: {}",
                        result
                            .as_ref()
                            .err()
                            .map(ToString::to_string)
                            .unwrap_or_default()
                    );
                    if !fails {
                        assert_eq!(result.unwrap(), Some(42), "{mode:?}: {entry}");
                    }
                    if entry == "send" {
                        assert_eq!(host::sent(), vec![0, 255]);
                    }
                }
                clean();
            }
        }
    }
}

#[test]
fn resource_bytes_reject_conflicting_loans_escapes_and_wrong_descriptors() {
    for source in [
        "fn bad() @suspends { var socket=byte_make(1); join { hold(&mut socket), hold(&mut socket) } }",
        "fn bad() @suspends { var socket=byte_make(1); let bytes=read(&socket,4) }",
        "fn bad() @suspends -> &mut Socket { var socket=byte_make(1); &mut socket }",
        "fn consume(socket:Socket) {} fn bad() @suspends { var socket=byte_make(1); scope { let view=&mut socket; branch {hold(view)}; consume(socket) } }",
        "fn bad() -> Array<&Socket> {var socket=byte_make(1); var a=Array.new::<&Socket>(); a.push(&socket); a}",
        "fn __joky_resource_poll_v1() {}",
        "fn joky_byte_request_data_v1() {}",
    ] {
        let source=format!("{SOURCE}\n{source}");
        for mode in [VmMode::Jit,VmMode::Interpreter] {
            assert!(vm(mode).load_source(&source).is_err(),"{mode:?}: {source}");
        }
        let mut compiler=crate::aot::AotCompiler::new("resource_bytes_negative").unwrap();
        compiler.register_resource_type("Socket").unwrap();
        compiler.register_resource_factory("byte_make","Socket").unwrap();
        compiler.register_resource_io("read","Socket",ByteIoKind::Read).unwrap();
        compiler.register_resource_io("write","Socket",ByteIoKind::Write).unwrap();
        assert!(compiler.compile_module(&crate::parser::parse(&source).unwrap()).is_err(),"{source}");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        assert!(vm
            .load_source(&SOURCE.replace("limit: UInt64", "limit: Int64"))
            .is_err());
        assert!(vm
            .load_source(&SOURCE.replace(
                "extern fn read(socket: &mut Socket",
                "extern fn read(socket: &Socket"
            ))
            .is_err());
    }
}
fn cancellation(exhaustive: bool) {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    for entry in [
        "receive",
        "send",
        "cancel_reuse",
        "unstarted",
        "field",
        "dynamic",
    ] {
        host::reset();
        let mut task = vm.call_async(entry, &[]).unwrap();
        let mut steps = 0;
        while !task.is_finished() {
            assert!(steps < 5000, "{entry}");
            task.resume().unwrap();
            steps += 1;
        }
        drop(task);
        clean();
        let cuts = if exhaustive {
            (0..=steps).collect::<Vec<_>>()
        } else {
            let mut cuts: Vec<_> = (0..=steps).step_by(8).collect();
            cuts.push(steps);
            cuts
        };
        for cut in cuts {
            for budget in if exhaustive {
                vec![None, Some(cut as u64)]
            } else {
                vec![None]
            } {
                vm.set_interpreter_limits(super::InterpreterLimits {
                    poll_budget: Some(1),
                    instruction_budget: budget,
                    ..Default::default()
                })
                .unwrap();
                host::reset();
                match vm.call_async(entry, &[]) {
                    Ok(mut task) => {
                        for _ in 0..if budget.is_some() { steps + 8 } else { cut } {
                            if task.is_finished() {
                                break;
                            }
                            if let Err(error) = task.resume() {
                                assert!(budget.is_some(), "{entry}: {}", crate::Error::from(error));
                                break;
                            }
                        }
                        drop(task);
                    }
                    Err(error) => assert!(budget.is_some(), "{entry}: {error}"),
                }
                clean();
            }
        }
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
    }
}
#[test]
fn resource_bytes_cancel_pending_operations_and_refund_storage() {
    cancellation(false);
}
#[test]
#[ignore = "exhaustive: byte requests cancelled at every interpreter instruction"]
fn exhaustive_resource_bytes_cancel_every_instruction() {
    cancellation(true);
}
