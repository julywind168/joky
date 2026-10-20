use super::*;
use crate::runtime::executor::HostExecutor;
use crate::vm::VmMode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/outbox.jk");
fn cancellation(boundaries: impl Clone + IntoIterator<Item = usize>, exhaustive: bool) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut host = VmEngine::with_mode(mode);
        host.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        if mode == VmMode::Interpreter {
            host.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        let baseline = crate::runtime::arena::LIVE.with(Cell::get);
        let mut completed = false;
        for boundary in boundaries.clone() {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            let id = executor
                .spawn(&group, host.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut done = false;
            for _ in 0..boundary {
                for result in executor.poll(1).unwrap().completed {
                    if result.id == id {
                        assert_eq!(
                            result
                                .result
                                .unwrap_or_else(|e| panic!("{mode:?} boundary {boundary}: {e}")),
                            0
                        );
                        done = true;
                    }
                }
                if done {
                    break;
                }
            }
            executor.cancel_group(&group);
            // Uncollected domain work is removed when its owning receipt dies.
            executor.poll(100).unwrap();
            assert_eq!(executor.task_count(), 0, "{mode:?} boundary {boundary}");
            assert!(executor.domains().borrow().outboxes.is_empty());
            assert_eq!(
                executor.domains().borrow().usage(),
                (0, 0, 0, 0),
                "{mode:?} boundary {boundary}"
            );
            drop(executor);
            assert_eq!(
                crate::runtime::arena::LIVE.with(Cell::get),
                baseline,
                "{mode:?} boundary {boundary}"
            );
            if done {
                completed = true;
                break;
            }
        }
        if exhaustive {
            assert!(completed, "{mode:?}: cancellation sweep missed completion");
        }
    }
}
#[test]
fn outbox_generated_cancellation_reclaims_batches_messages_and_waiters() {
    cancellation(
        [0, 1, 2, 3, 5, 10, 20, 50, 100, 200, 400, 800, 1600, 3200],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel outbox staging, atomic commits, and receive at every interpreter instruction"]
fn exhaustive_outbox_cancellation() {
    cancellation(0..=16384, true);
}

#[test]
fn outbox_pending_receive_wakes_on_close_and_cancels_without_polling() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for cancel in [false, true] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("import joky/outbox fn wait() @suspends ->Int32 { var q=outbox.create(2,16) match q.receive() { None=>42, Some(_)=>0 } }").unwrap();
            let baseline = crate::runtime::arena::LIVE.with(Cell::get);
            let group = TaskGroup::new();
            vm.executor
                .spawn(&group, vm.prepare_async("wait", &[]).unwrap())
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert_eq!(vm.poll_tasks(100).unwrap().polled, 0);
            let queue = vm
                .domains
                .borrow()
                .outboxes
                .values()
                .next()
                .unwrap()
                .upgrade()
                .unwrap();
            assert!(queue.borrow().waiter.is_some());
            if cancel {
                vm.executor.cancel_group(&group);
            } else {
                queue.borrow_mut().close();
                let report = vm.poll_tasks(100).unwrap();
                assert_eq!(report.completed.len(), 1);
                assert_eq!(*report.completed[0].result.as_ref().unwrap(), 42);
            }
            assert!(queue.borrow().waiter.is_none());
            assert!(queue.borrow().closed);
            drop(queue);
            assert!(vm.domains.borrow().outboxes.is_empty());
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), baseline);
        }
    }
}
#[test]
fn outbox_limits_identity_poison_and_allocation_failures_refund_storage() {
    let vm = VmEngine::new();
    assert!(create(&vm.domains, 0, 0).is_err());
    assert!(create(&vm.domains, u64::MAX, 0).is_err());
    assert!(batch(&vm.domains, u64::MAX, 0).is_err());
    drop(batch(&vm.domains, 257, 0).unwrap());
    drop(batch(&vm.domains, 0, 0).unwrap());
    for allowance in [0, 1, 64, 256, 1024, 4096] {
        vm.domains.borrow().budget.limit.set(allowance);
        drop(create(&vm.domains, 2, 6));
        drop(batch(&vm.domains, 2, 6));
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        assert!(vm.domains.borrow().outboxes.is_empty());
    }
    vm.domains.borrow().budget.limit.set(usize::MAX);
    let owner = create(&vm.domains, 1, 3).unwrap();
    let base = vm.domains.borrow().budget.bytes.get();
    let mut outgoing = batch(&vm.domains, 2, 6).unwrap();
    let allocated = vm.domains.borrow().budget.bytes.get();
    vm.domains.borrow().budget.limit.set(allocated + 2);
    assert_eq!(
        outgoing
            .stage(&vm.domains.borrow(), owner.token, &[0, 255, 42])
            .unwrap_err()
            .code(),
        ErrorCode::ResourceLimit
    );
    assert!(outgoing.messages.is_empty());
    assert_eq!(vm.domains.borrow().budget.bytes.get(), allocated);
    vm.domains.borrow().budget.limit.set(usize::MAX);
    assert_eq!(
        outgoing
            .stage(&vm.domains.borrow(), owner.token, &[])
            .unwrap(),
        0
    );
    assert_eq!(
        outgoing
            .stage(&vm.domains.borrow(), owner.token, &[])
            .unwrap(),
        0
    );
    assert_eq!(
        outgoing.prepare(&vm.domains.borrow()),
        Err(Status::OutboxFull)
    );
    assert!(owner.queue.borrow().messages.is_empty());
    // Even zero-byte messages consume slots; failed staging poisons the whole batch.
    assert_eq!(
        outgoing
            .stage(&vm.domains.borrow(), owner.token, &[])
            .unwrap(),
        1
    );
    drop(outgoing);
    assert_eq!(vm.domains.borrow().budget.bytes.get(), base);
    let old = owner.token;
    drop(owner);
    let replacement = create(&vm.domains, 1, 3).unwrap();
    assert_ne!(replacement.token, old);
    let other = VmEngine::new();
    let mut outgoing = batch(&vm.domains, 1, 3).unwrap();
    assert!(outgoing
        .stage(&other.domains.borrow(), replacement.token, &[42])
        .is_err());
    assert_eq!(outgoing.stage(&vm.domains.borrow(), old, &[42]).unwrap(), 2);
    assert_eq!(
        outgoing.prepare(&vm.domains.borrow()),
        Err(Status::OutboxClosed)
    );
    assert!(replacement.queue.borrow().messages.is_empty());
    drop((replacement, outgoing));
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}

thread_local! { static READY: Cell<bool> = const { Cell::new(false) }; }
extern "C" fn ready() -> i32 {
    READY.with(|r| r.set(true));
    0
}
#[test]
fn outbox_code_publication_invalidates_candidate_without_sending() {
    let source = r#"import joky/domain import joky/outbox import joky/array.{type Array} import joky/task
extern fn outbox_test_ready()->Int32
struct State { n:Int32 }
fn make(n:Int32)->State { State{n:n} }
fn read(s:&State)->Int32 { s.n }
fn run() @suspends ->Int32 {
    let state=domain.create(make,7)
    let queue=outbox.create(1,1)
    var batch=outbox.batch(1,1)
    var bytes=Array.new::<UInt8>() bytes.push(99)
    outbox.stage(&mut batch,queue.handle(),bytes);
    let next=domain.candidate_from(domain.snapshot_from(state.read_handle(),read), state.handle(), make, 99).with_outbox(batch)
    outbox_test_ready(); task.yield_now()
    var receipt=domain.commit_to(state.handle(),next)
    if match receipt.wait() { Err(Conflict)=>true, _=>false } {
        if domain.read_from(state.read_handle(),read).unwrap()==7 { return 42 }
    }
    0
}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.with(|r| r.set(false));
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "outbox_test_ready",
                crate::FunctionSignature::new(vec![], Type::Int32),
                ready as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        let baseline = crate::runtime::arena::LIVE.with(Cell::get);
        vm.executor
            .spawn(&TaskGroup::new(), vm.prepare_async("run", &[]).unwrap())
            .unwrap();
        for _ in 0..30 {
            vm.poll_tasks(100).unwrap();
            if READY.with(Cell::get) {
                break;
            }
        }
        assert!(READY.with(Cell::get));
        let queue = vm
            .domains
            .borrow()
            .outboxes
            .values()
            .next()
            .unwrap()
            .upgrade()
            .unwrap();
        assert!(queue.borrow().messages.is_empty());
        vm.load_source(&format!("{source}\nfn newer()->Int32 {{ 2 }}"))
            .unwrap();
        assert_eq!(vm.publish_domain_code().unwrap(), 1);
        let mut completed = false;
        for _ in 0..30 {
            let report = vm.poll_tasks(100).unwrap();
            assert!(queue.borrow().messages.is_empty());
            if let Some(result) = report.completed.first() {
                assert_eq!(
                    *result.result.as_ref().unwrap_or_else(|e| panic!("{e}")),
                    42
                );
                completed = true;
                break;
            }
        }
        assert!(completed);
        assert!(queue.borrow().closed);
        drop(queue);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), baseline);
    }
}

#[test]
fn outbox_large_batches_aggregate_targets_and_preserve_each_fifo() {
    let vm = VmEngine::new();
    let owners: Vec<_> = (0..300)
        .map(|_| create(&vm.domains, 2, 2).unwrap())
        .collect();
    let mut outgoing = batch(&vm.domains, 600, 600).unwrap();
    for owner in owners.iter().rev() {
        assert_eq!(
            outgoing
                .stage(&vm.domains.borrow(), owner.token, &[1])
                .unwrap(),
            0
        );
    }
    for owner in &owners {
        assert_eq!(
            outgoing
                .stage(&vm.domains.borrow(), owner.token, &[2])
                .unwrap(),
            0
        );
    }
    // Preflight and publication require no additional budget, even above 256 messages.
    vm.domains
        .borrow()
        .budget
        .limit
        .set(vm.domains.borrow().budget.bytes.get());
    outgoing.prepare(&vm.domains.borrow()).unwrap();
    assert_eq!(outgoing.targets.len(), 300);
    outgoing.publish();
    for owner in &owners {
        let queue = owner.queue.borrow();
        assert_eq!(queue.bytes, 2);
        assert_eq!(
            queue
                .messages
                .iter()
                .map(|m| m.bytes[0])
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }
    vm.domains.borrow().budget.limit.set(usize::MAX);
    let mut rejected = batch(&vm.domains, 300, 0).unwrap();
    for owner in owners.iter().rev() {
        rejected
            .stage(&vm.domains.borrow(), owner.token, &[])
            .unwrap();
    }
    assert_eq!(
        rejected.prepare(&vm.domains.borrow()),
        Err(Status::OutboxFull)
    );
    assert_eq!(rejected.failed_target, Some(owners[0].token));
    drop((outgoing, rejected, owners));
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}
