use super::{VmEngine, VmMode};
mod host {
    use crate as joky;
    include!("../../tests/fixtures/resource_factory_host.rs");
}
const SOURCE: &str = include_str!("../../tests/fixtures/resource_owners.jk");
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.register_resource_type("Socket").unwrap();
    // SAFETY: Static factory owns each listener and releases it exactly once.
    unsafe {
        vm.register_resource_factory("make", "Socket", host::FACTORY, None)
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
fn resource_owners_move_in_results_containers_and_tasks() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in [
            "result",
            "pair",
            "array_task",
            "partial",
            "unstarted",
            "dynamic",
        ] {
            for cancel in [false, true] {
                host::reset();
                let result: Result<Option<u64>, crate::Error> = (|| {
                    let mut task = vm.call_async(entry, &[])?;
                    if cancel {
                        return Ok(None);
                    }
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume()?;
                    }
                    Ok(task.cps_result())
                })();
                if !cancel {
                    if entry == "partial" {
                        assert!(result.is_err());
                    } else {
                        assert_eq!(result.unwrap(), Some(42), "{mode:?}: {entry}");
                    }
                }
                clean();
            }
        }
    }
}
#[test]
fn resource_owners_survive_state_reload_and_release_on_instance_close() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        host::reset();
        let mut vm = vm(mode);
        let id = vm.create_instance("create", &[]).unwrap();
        assert_eq!(host::events(), vec![-1]);
        vm.load_source("fn unrelated() -> Int32 {0}").unwrap();
        vm.close_instance(id).unwrap();
        assert_eq!(host::events(), vec![-1, 1]);
        clean();
    }
}
#[test]
fn resource_factory_failure_releases_before_language_adoption() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        host::reset();
        crate::runtime::arena::LIMIT.with(|n| n.set(0));
        let result = vm.call_async("result", &[]);
        crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
        assert!(result.is_err());
        assert_eq!(host::events(), vec![-1, 1]);
        clean();
    }
}

fn cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = vm(VmMode::Interpreter);
    for entry in ["result", "pair", "array_task", "dynamic", "unstarted"] {
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        host::reset();
        let mut cuts = std::collections::BTreeSet::from([0]);
        let mut previous = 0;
        let mut steps: usize = 0;
        let mut task = vm.call_async(entry, &[]).unwrap();
        loop {
            let current = host::events().len();
            if current != previous || steps.is_multiple_of(32) {
                cuts.extend(steps.saturating_sub(1)..=steps + 1);
            }
            previous = current;
            if task.is_finished() {
                break;
            }
            assert!(steps < 5000, "{entry}");
            task.resume().unwrap();
            steps += 1;
        }
        drop(task);
        clean();
        cuts.insert(steps);
        if matches!(sweep, super::support::CancellationSweep::EveryInstruction) {
            cuts.extend(0..=steps);
        }
        for cut in cuts.into_iter().filter(|&n| n <= steps) {
            host::reset();
            let mut task = vm.call_async(entry, &[]).unwrap();
            for _ in 0..cut {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            drop(task);
            clean();
        }
    }
}
#[test]
fn resource_owners_cancel_at_resource_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: resource owner cancellation at every interpreter instruction"]
fn exhaustive_resource_owners_cancel_at_each_instruction() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn resource_owners_reject_copy_forgery_and_unchecked_abi() {
    for source in [
        "fn take(s: Socket) {} fn bad() { let s=make(1) take(s) take(s) }",
        "fn copy<T: Copy>(s: T) {} fn bad() { copy(make(1)) }",
        "fn bad() -> Socket { 1 }",
        "extern fn leak(s: Socket) fn bad() { leak(make(1)) }",
        "extern fn leak() -> Socket fn bad() { let s=leak() }",
        "extern fn make(n: Int32) -> Socket fn bad() { let s=make(1) }",
    ] {
        let source = if source.starts_with("extern fn make") {
            source.to_owned()
        } else {
            format!("extern fn make(n: UInt64) -> Socket {source}")
        };
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = vm(mode);
            assert!(vm.load_source(&source).is_err(), "{mode:?}: {source}");
        }
        let mut compiler = crate::aot::AotCompiler::new("resource_negative").unwrap();
        compiler.register_resource_type("Socket").unwrap();
        compiler
            .register_resource_factory("make", "Socket")
            .unwrap();
        assert!(
            compiler
                .compile_module(&crate::parser::parse(&source).unwrap())
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn resource_owners_pin_callback_code_until_pending_task_is_released() {
    use std::{cell::Cell, rc::Rc};
    struct Owner(Rc<Cell<bool>>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let dropped = Rc::new(Cell::new(false));
        host::reset();
        let mut vm = VmEngine::with_mode(mode);
        vm.register_resource_type("Socket").unwrap();
        unsafe {
            vm.register_resource_factory(
                "make",
                "Socket",
                host::FACTORY,
                Some(Rc::new(Owner(dropped.clone()))),
            )
            .unwrap();
        }
        vm.load_source(SOURCE).unwrap();
        let task = vm.call_async("result", &[]).unwrap();
        drop(vm);
        assert!(!dropped.get());
        assert_eq!(host::events(), vec![-1]);
        drop(task);
        assert!(dropped.get());
        assert_eq!(host::events(), vec![-1, 1]);
        clean();
    }
}

const HANDOFF: &str = include_str!("../../tests/fixtures/resource_handoff.jk");
fn handoff_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.register_resource_type("Socket").unwrap();
    unsafe {
        vm.register_resource_factory_async("acquire", "Socket", host::ASYNC_FACTORY, None)
            .unwrap();
    }
    vm.load_source(HANDOFF).unwrap_or_else(|e| {
        panic!(
            "{mode:?}: {e} {:?}",
            e.span().and_then(|s| s.slice(HANDOFF))
        )
    });
    vm
}
#[test]
fn resource_handoff_stops_before_release_on_success_failure_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = handoff_vm(mode);
        for entry in [
            "success",
            "startup_failure",
            "output_failure",
            "invalid_pending",
            "empty_success",
            "runtime_failure",
            "race_cancel",
            "race_unstarted",
        ] {
            for cancel in [false, true] {
                host::reset();
                host::reset_stops();
                let result: Result<Option<u64>, crate::Error> = (|| {
                    let mut task = vm.call_async(entry, &[])?;
                    if cancel {
                        return Ok(None);
                    }
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume()?;
                    }
                    assert!(task.is_finished());
                    Ok(task.cps_result())
                })();
                if !cancel {
                    let fails = entry.ends_with("failure")
                        || entry == "invalid_pending"
                        || entry == "empty_success";
                    assert_eq!(result.is_err(), fails, "{mode:?}: {entry}: {result:?}");
                    if !fails {
                        assert_eq!(result.unwrap(), Some(42));
                    }
                }
                if entry == "race_unstarted" {
                    assert!(host::stops().is_empty());
                } else {
                    assert_eq!(host::stops().len(), 1, "{mode:?}: {entry}");
                }
                clean();
            }
        }
    }
}
#[test]
fn resource_handoff_quota_failure_releases_published_owner() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = handoff_vm(mode);
        host::reset();
        host::reset_stops();
        let mut task = vm.call_async("success", &[]).unwrap();
        assert_eq!(host::events(), vec![-1]);
        crate::runtime::arena::LIMIT.with(|n| n.set(0));
        let result = task.resume();
        crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
        assert!(result.is_err());
        drop(task);
        assert_eq!(host::events(), vec![-1, 1]);
        assert_eq!(host::stops(), vec![0]);
        clean();
    }
}
#[test]
#[ignore = "exhaustive: resource handoff at every instruction and budget exhaustion"]
fn exhaustive_resource_handoff_cancel_and_exhaust_at_every_instruction() {
    let mut vm = handoff_vm(VmMode::Interpreter);
    for entry in ["success", "race_cancel", "race_unstarted"] {
        for limit in 0..180 {
            for exhaust in [false, true] {
                vm.set_interpreter_limits(super::InterpreterLimits {
                    poll_budget: Some(1),
                    instruction_budget: exhaust.then_some(limit),
                    ..Default::default()
                })
                .unwrap();
                host::reset();
                host::reset_stops();
                if let Ok(mut task) = vm.call_async(entry, &[]) {
                    for _ in 0..if exhaust { 180 } else { limit } {
                        if task.is_finished() || task.resume().is_err() {
                            break;
                        }
                    }
                }
                assert!(host::stops().len() <= 1);
                clean();
            }
        }
    }
}
