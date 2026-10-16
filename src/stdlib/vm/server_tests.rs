//! HR7/W5 functional composition and cleanup at real server lifecycle boundaries.
use super::*;
use crate::runtime::{
    executor::{current_reactor, HostExecutor, TaskGroup},
    reactor::Reactor,
};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};
#[path = "../../../tests/support/server_reload.rs"]
mod fixture;

thread_local! {
    static PHASE: Cell<i32> = const { Cell::new(0) };
    static REACTOR: RefCell<Option<Reactor>> = const { RefCell::new(None) };
}
extern "C" fn record_phase(phase: i32) {
    PHASE.with(|p| p.set(phase));
    REACTOR.with(|r| *r.borrow_mut() = current_reactor().ok());
}

#[test]
fn server_reload_lifecycle_boundaries_release_tcp_domains_tasks_and_modules() {
    for host_mode in [VmMode::Jit, VmMode::Interpreter] {
        for create in ["new", "interpreter"] {
            let mut host = VmEngine::with_mode(host_mode);
            unsafe {
                host.register_fn(
                    "record_phase",
                    FunctionSignature::new(vec![Type::Int32], Type::Unit),
                    record_phase as *const u8,
                )
                .unwrap();
            }
            let source = fixture::source(create).replace(
                "fn checkpoint(phase:Int32) @suspends {task.yield_now()}",
                "extern fn record_phase(phase:Int32) fn checkpoint(phase:Int32) @suspends {record_phase(phase) task.yield_now()}",
            );
            host.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
            for cut in 1..=9 {
                PHASE.with(|p| p.set(0));
                REACTOR.with(|r| *r.borrow_mut() = None);
                let arenas = crate::runtime::arena::LIVE.with(Cell::get);
                let sockets = crate::runtime::tcp::LIVE.with(Cell::get);
                let executor = HostExecutor::default();
                let group = TaskGroup::new();
                let id = executor
                    .spawn(&group, host.prepare_async("main", &[]).unwrap())
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(40);
                let mut finished = false;
                let mut codes = Vec::new();
                let mut last_phase = 0;
                loop {
                    let report = executor.poll(1).unwrap();
                    for done in report.completed {
                        if done.id == id {
                            assert_eq!(
                                done.result.unwrap_or_else(|e| panic!(
                                    "{host_mode:?}/{create}, cut={cut}: {e}"
                                )),
                                0
                            );
                            finished = true;
                        }
                    }
                    let phase = PHASE.with(Cell::get);
                    if phase != last_phase {
                        last_phase = phase;
                        VMS.with(|vms| {
                            for state in vms.borrow().values() {
                                if let Some(owner) = state.engine.borrow().script_code_owner() {
                                    codes.push(Rc::downgrade(&owner));
                                }
                            }
                        });
                    }
                    if finished || phase == cut {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "server acceptance stalled at {phase}, cut={cut}"
                    );
                    if report.polled == 0 {
                        executor.wait().unwrap();
                    }
                }
                assert_eq!(finished, cut == 9, "{host_mode:?}/{create}, cut={cut}");
                assert!(crate::runtime::tcp::LIVE.with(Cell::get) > sockets || finished);
                executor.cancel_group(&group);
                VMS.with(|vms| assert!(vms.borrow().is_empty()));
                assert_eq!(executor.task_count(), 0);
                assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
                REACTOR.with(|reactor| {
                    let reactor = reactor
                        .borrow_mut()
                        .take()
                        .expect("server reached socket checkpoint");
                    assert_eq!(reactor.registration_count(), 0);
                    assert_eq!(reactor.socket_count(), 0);
                });
                assert!(
                    codes.iter().all(|code| code.strong_count() == 0),
                    "Script module retained after shutdown"
                );
                assert_eq!(crate::runtime::tcp::LIVE.with(Cell::get), sockets);
                drop(executor);
                assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), arenas);
                VMS.with(|vms| assert!(vms.borrow().is_empty()));
            }
        }
    }
}
