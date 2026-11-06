use super::*;
use crate::runtime::executor::HostExecutor;
use crate::vm::VmMode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/outbox.jk");
fn cancellation(
    source: &str,
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut host = VmEngine::with_mode(mode);
        host.load_source(source).unwrap_or_else(|e| panic!("{e}"));
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
        SOURCE,
        [0, 1, 2, 3, 5, 10, 20, 50, 100, 200, 400, 800, 1600, 3200],
        false,
    );
}
#[test]
fn outbox_policy_cancellation_reclaims_outputs_and_control_links() {
    cancellation(
        include_str!("../../../../tests/fixtures/outbox_policies.jk"),
        [
            0, 1, 2, 3, 5, 10, 20, 50, 100, 200, 400, 800, 1600, 3200, 6400,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel outbox staging, atomic commits, and receive at every interpreter instruction"]
fn exhaustive_outbox_cancellation() {
    cancellation(SOURCE, 0..=16384, true);
    cancellation(
        include_str!("../../../../tests/fixtures/outbox_policies.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn outbox_pending_receive_wakes_on_close_and_cancels_without_polling() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for cancel in [false, true] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("import joky/outbox fn wait() @suspends ->Int32 { var q=outbox.create(2,16); match q.receive() { None=>42, Some(_)=>0 } }").unwrap();
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
    var bytes=Array.new::<UInt8>(); bytes.push(99)
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

#[test]
fn state_domains_when_messages_rollback_on_traps_limits_and_business_rejection() {
    const SOURCE: &str = r#"
import joky/domain import joky/outbox
struct S { n:Int32 }
fn make(n:Int32)->S{S{n:n}}
fn read(s:&S)->Int32{s.n}
fn operation(h:domain.WriteHandle<S>,target:outbox.Handle,zero:Int32) @suspends {
    when "emit" (mut h) {
        h.n+=1
        let bytes="payload".into_bytes()
        outbox.emit(target,&bytes)
        let result:Result<Int32,Int32>=Err(1/zero)
        result
    };
}
"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (case, disconnect) in [
            "reject",
            "trap",
            "message_limit",
            "byte_limit",
            "allocation",
        ]
        .into_iter()
        .flat_map(|case| [(case, false), (case, true)])
        {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            // Rollback is policy-independent: keep the Reject queue alongside Disconnect.
            let control = disconnect.then(|| create(&vm.domains, 1, 0).unwrap());
            let queue = match &control {
                Some(control) => {
                    create_with_control(&vm.domains, 1, 0, Some(control.token)).unwrap()
                }
                None => create(&vm.domains, 2, 32).unwrap(),
            };
            let baseline = vm.domains.borrow().budget.bytes.get();
            let live = crate::runtime::arena::LIVE.with(Cell::get);
            let output = Rc::new(RefCell::new(vec![0; 3]));
            let mut request = pairs::submit_values(
                &vm.domains,
                &[owner.write_handle()],
                vm.current.as_ref().unwrap(),
                "$when/named/4:main/9:operation/4:656d6974",
                vec![queue.token, u64::from(case != "trap")],
                pairs::ValueOutput {
                    words: output,
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            vm.set_domain_limits(DomainLimits {
                max_operation_messages: if case == "message_limit" { 0 } else { 2 },
                max_operation_bytes: if case == "byte_limit" { 0 } else { 32 },
                max_state_bytes: if case == "allocation" {
                    baseline
                } else {
                    64 * 1024 * 1024
                },
                ..Default::default()
            })
            .unwrap();
            vm.poll_tasks(10000).unwrap();
            let (result, rejection) = request.take_access_result().unwrap();
            if case == "reject" {
                assert!(matches!(result, DomainOutcome::Value(InstanceValue::Unit)));
                assert!(rejection.is_none());
            } else if case == "trap" {
                assert!(
                    matches!(result, DomainOutcome::Failed(e) if e.message().contains("Division by zero"))
                );
                assert!(rejection.is_none());
            } else {
                assert_eq!(rejection, Some(access::Rejection::ResourceLimit), "{case}");
            }
            drop(request);
            assert!(queue.queue.borrow().messages.is_empty(), "{case}");
            assert!(!queue.queue.borrow().overflowed, "{case}");
            if let Some(control) = &control {
                assert!(!control.queue.borrow().closed, "{case}");
            }
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline, "{case}");
            assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), live, "{case}");
            vm.set_domain_limits(Default::default()).unwrap();
            let mut read = vm
                .queue_domain_call(owner.read_handle(), "read", &[])
                .unwrap();
            vm.poll_tasks(1000).unwrap();
            assert!(matches!(
                read.take_result(),
                Some(DomainOutcome::Value(InstanceValue::Int32(7)))
            ));
            drop((queue, control));
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}

#[test]
fn session_overflow_cancels_a_blocked_writer_and_runs_membership_cleanup() {
    session_blocked_writer(false);
}

#[test]
fn session_drains_a_blocked_writer_without_losing_committed_bytes() {
    session_blocked_writer(true);
}

fn session_blocked_writer(draining: bool) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    thread_local! { static OUTPUT: Cell<u64> = const { Cell::new(0) }; }
    extern "C" fn output(token: u64) {
        OUTPUT.with(|slot| slot.set(token));
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut vm = VmEngine::with_mode(mode);
        // SAFETY: The callback has the registered scalar ABI and lives for the test.
        unsafe {
            vm.register_fn(
                "output_ready",
                crate::FunctionSignature::new(vec![Type::UInt64], Type::Unit),
                output as *const u8,
            )
            .unwrap();
        }
        let run = if draining {
            "let stop = shutdown.create(); session.run_draining(connection,stream,session.Lines.default(),&handler,&stop)"
        } else {
            "session.run(connection,stream,session.Lines.default(),&handler)"
        };
        let keep_reading = !draining;
        vm.load_source(&format!(
            r#"
import joky/session import joky/domain import joky/shutdown
import joky/tcp.{{type TcpStream, type Address}}
extern fn output_ready(token:UInt64)
#state
struct Membership {{ live:Bool, closed:Int32 }}
fn make(_:Unit)->Membership {{ Membership{{live:true,closed:0}} }}
struct MemberHandler {{ member:domain.WriteHandle<Membership> }}
impl session.Handler for MemberHandler {{
    type Message = String
    fn on_message(&self, _text:String) @suspends @noescape ->Bool {{ {keep_reading} }}
    fn on_close(&self) @suspends @noescape {{
        let member=self.member
        when(mut member) {{ member.live=false; member.closed+=1 }}!
    }}
}}
fn main() @suspends ->Int32 {{
    let stream=TcpStream.connect(Address.ipv4(0x7f000001,{port}))!
    var options=session.Options.default()
    options.queue_messages=1
    options.queue_bytes=65536
    let connection=session.create(options)
    let output=connection.output()
    output_ready(output.identity())
    let owner=domain.create(make,{{}})
    let member=owner.handle()
    let handler=MemberHandler{{member}}
    {run}
    when(member) {{ if !member.live && member.closed==1 {{42}} else {{0}} }}!
}}
"#
        ))
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        OUTPUT.with(|slot| slot.set(0));
        vm.executor
            .spawn(&TaskGroup::new(), vm.prepare_async("main", &[]).unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut peer = None;
        while OUTPUT.with(Cell::get) == 0 || peer.is_none() {
            let report = vm.poll_tasks(100).unwrap();
            assert!(report.completed.is_empty());
            if peer.is_none() {
                match listener.accept() {
                    Ok((stream, _)) => peer = Some(stream),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("{e}"),
                }
            }
            assert!(Instant::now() < deadline, "connection did not start");
            if report.polled == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let token = OUTPUT.with(Cell::get);
        let queue = vm.domains.borrow().outboxes[&token].upgrade().unwrap();
        let control = queue.borrow().control.upgrade().unwrap();
        let payload = vec![42; 65536];
        let mut blocked = false;
        let mut submitted = 0;
        for _ in 0..128 {
            let mut send = batch(&vm.domains, 1, payload.len() as u64).unwrap();
            send.stage(&vm.domains.borrow(), token, &payload).unwrap();
            send.prepare(&vm.domains.borrow()).unwrap();
            send.publish();
            send.wake();
            submitted += payload.len();
            let mut idle = 0;
            while idle < 3 {
                let report = vm.poll_tasks(100).unwrap();
                assert!(report.completed.is_empty(), "session ended before overflow");
                if report.polled == 0 {
                    idle += 1;
                    std::thread::sleep(Duration::from_millis(1));
                } else {
                    idle = 0;
                }
                assert!(Instant::now() < deadline, "writer did not become idle");
            }
            if !queue.borrow().messages.is_empty() {
                blocked = true;
                break;
            }
        }
        assert!(blocked, "non-reading peer must cause real TCP backpressure");
        assert!(
            queue.borrow().waiter.is_none(),
            "writer is waiting on TCP, not receive"
        );
        assert!(!control.borrow().closed);
        if draining {
            let peer = peer.as_mut().unwrap();
            peer.write_all(b"stop\n").unwrap();
            peer.set_nonblocking(true).unwrap();
        } else {
            let mut overflow = batch(&vm.domains, 1, 1).unwrap();
            overflow.stage(&vm.domains.borrow(), token, &[1]).unwrap();
            overflow.prepare(&vm.domains.borrow()).unwrap();
            overflow.publish();
            overflow.wake();
            assert!(queue.borrow().overflowed && control.borrow().closed);
        }
        drop((queue, control));
        // Only the draining case reads; overflow must cancel an unread blocked socket.
        let mut received = 0;
        let mut eof = !draining;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut results = Vec::new();
        while vm.executor.task_count() != 0 || !eof {
            let report = vm.poll_tasks(100).unwrap();
            results.extend(report.completed);
            if draining && !eof {
                let mut bytes = [0; 65536];
                match peer.as_mut().unwrap().read(&mut bytes) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        assert!(bytes[..n].iter().all(|&byte| byte == 42));
                        received += n;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("{e}"),
                }
            }
            assert!(
                Instant::now() < deadline,
                "{mode:?}: blocked writer was not cancelled"
            );
            if report.polled == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if draining {
            assert_eq!(received, submitted, "all committed bytes survive sealing");
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].result, Ok(42));
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        assert!(vm.domains.borrow().outboxes.is_empty());
        crate::runtime::tcp::LIVE.with(|live| assert_eq!(live.get(), 0));
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        drop(peer);
    }
}

#[test]
fn disconnect_control_validation_and_failed_creation_preserve_ownership() {
    let vm = VmEngine::new();
    let other = VmEngine::new();
    let foreign = create(&other.domains, 1, 0).unwrap();
    assert!(create_with_control(&vm.domains, 1, 1, Some(foreign.token)).is_err());
    let control = create(&vm.domains, 1, 0).unwrap();
    let output = create_with_control(&vm.domains, 1, 1, Some(control.token)).unwrap();
    assert!(create_with_control(&vm.domains, 1, 1, Some(output.token)).is_err());
    drop(output);
    assert!(control.queue.borrow().closed);
    assert!(create_with_control(&vm.domains, 1, 1, Some(control.token)).is_err());
    drop(control);
    let control = create(&vm.domains, 1, 0).unwrap();
    let baseline = vm.domains.borrow().budget.bytes.get();
    vm.domains.borrow().budget.limit.set(baseline);
    assert!(create_with_control(&vm.domains, 1, 1, Some(control.token)).is_err());
    assert!(
        !control.queue.borrow().closed,
        "failed creation does not close its signal"
    );
    assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
    vm.domains.borrow().budget.limit.set(usize::MAX);
    let output = create_with_control(&vm.domains, 1, 1, Some(control.token)).unwrap();
    let weak = Rc::downgrade(&control.queue);
    drop(control);
    assert!(
        weak.upgrade().is_none(),
        "output must not retain control ownership"
    );
    output.queue.borrow_mut().close();
    drop(output);
    assert!(vm.domains.borrow().outboxes.is_empty());
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}

#[test]
fn outbox_owned_receive_preflights_budget_and_moves_bytes() {
    use crate::runtime::parallel::{Storage, TaskBudget};

    let vm = VmEngine::new();
    let mut arena = Arena::default();
    let owner = create(&vm.domains, 1, 3).unwrap();
    let queue = owner.queue.clone();
    let mut outgoing = batch(&vm.domains, 1, 3).unwrap();
    outgoing
        .stage(&vm.domains.borrow(), owner.token, &[0, 255, 42])
        .unwrap();
    outgoing.prepare(&vm.domains.borrow()).unwrap();
    outgoing.publish();
    drop(outgoing);
    let token = adopt(&mut arena, owner).unwrap();
    let original = queue.borrow().messages[0].bytes.as_ptr();
    let queued_budget = vm.domains.borrow().budget.bytes.get();
    let budget = TaskBudget::new(1 << 20);
    let storage = Storage::new(vec![budget.clone()]).unwrap();
    let baseline = budget.used();
    let held = storage.accounts().reserve((1 << 20) - baseline).unwrap();
    let mut frame = CpsFrame::new(1);
    frame.slots[0] = token;
    let receive = |frame: &mut CpsFrame| unsafe {
        receive_bytes(frame, &vm.domains, |bytes| {
            storage.accounts().reserve(17 + bytes.len())
        })
    };
    assert!(matches!(receive(&mut frame), Err(error) if error.code() == ErrorCode::ResourceLimit));
    assert_eq!(queue.borrow().bytes, 3);
    assert_eq!(queue.borrow().messages[0].bytes.as_ptr(), original);
    assert_eq!(vm.domains.borrow().budget.bytes.get(), queued_budget);
    drop(held);

    let received = receive(&mut frame).unwrap().unwrap();
    let bytes = received.bytes.as_ref().unwrap();
    assert_eq!(bytes, &[0, 255, 42]);
    assert_eq!(bytes.as_ptr(), original, "dequeue must move the allocation");
    assert!(queue.borrow().messages.is_empty());
    assert_eq!(queue.borrow().bytes, 0);
    assert_eq!(vm.domains.borrow().budget.bytes.get(), queued_budget - 3);
    assert_eq!(
        budget.used(),
        baseline + 20,
        "reply owns replacement accounting"
    );
    drop(received); // Also models a cancelled reply that the worker never installs.
    assert_eq!(budget.used(), baseline);
    queue.borrow_mut().close();
    let closed = receive(&mut frame).unwrap().unwrap();
    assert!(closed.bytes.is_none());
    drop(closed);
    assert_eq!(budget.used(), baseline);
    unsafe { Buffer::release(&mut arena, token as *mut Buffer).unwrap() };
    drop((frame, queue, storage));
    assert_eq!(budget.used(), 0);
    assert!(vm.domains.borrow().outboxes.is_empty());
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}

#[test]
fn outbox_sealing_and_session_draining_preserve_committed_output() {
    for source in [
        include_str!("../../../../tests/fixtures/outbox_seal.jk"),
        include_str!("../../../../tests/fixtures/session_drain.jk"),
        include_str!("../../../../tests/fixtures/session_messages.jk"),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source)
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
            assert_eq!(
                crate::runtime::driver::block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(),
                0
            );
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn session_rejects_mismatched_reader_and_handler_messages() {
    let source = r#"
import joky/session
import joky/tcp.{type TcpStream}
struct TextHandler {}
impl session.Handler for TextHandler {
    type Message = String
    fn on_message(&self, _text: String) @suspends @noescape -> Bool { true }
    fn on_close(&self) @suspends @noescape {}
}
fn invalid(connection: session.Connection, stream: TcpStream) @suspends {
    let handler = TextHandler {}
    session.run(connection, stream, session.LengthPrefixed.default(), &handler)
}
"#;
    let error = crate::core::prepared::check_source(source, &Default::default()).unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::TypeMismatch, "{error}");
}
