use super::*;
use crate::vm::VmMode;

const SOURCE: &str = r#"
struct State { n:Int32 }
struct Inner { pub delta:Int32, tag:Int32 }
struct Input { inner:Inner }
struct Reply { pub first:Int32, second:Int32 }
fn make(n:Int32)->State {State{n:n}}
fn read(s:&State)->Int32 {s.n}
fn turn(a:&mut State,b:&mut State,input:Input)->Reply {
    a.n=a.n+input.inner.delta
    b.n=b.n+input.inner.tag
    Reply{first:a.n,second:b.n}
}
"#;

fn enqueue_value(
    vm: &VmEngine,
    a: DomainHandle,
    b: DomainHandle,
) -> (DomainRequest, Rc<RefCell<Vec<u64>>>) {
    let output = Rc::new(RefCell::new(vec![0; 2]));
    let request = pairs::submit_values(
        &vm.domains,
        &[a, b],
        vm.current.as_ref().unwrap(),
        "turn",
        vec![2, 3],
        pairs::ValueOutput {
            words: output.clone(),
            snapshot: false,
        },
        vm.interpreter_limits(),
    )
    .unwrap();
    (request, output)
}
fn read(vm: &mut VmEngine, h: DomainHandle, expected: i32) {
    let mut request = vm.queue_domain_call(h, "read", &[]).unwrap();
    vm.poll_tasks(100).unwrap();
    assert!(
        matches!(request.take_result(),Some(DomainOutcome::Value(InstanceValue::Int32(n))) if n==expected)
    );
}

#[test]
fn domain_value_packets_rebind_queued_operations_and_reject_changed_contracts() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for change in [
            "compatible",
            "input_type",
            "input_name",
            "input_order",
            "privacy",
            "result_type",
            "result_order",
            "permission",
            "missing",
        ] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(20)])
                .unwrap();
            let (mut request, output) = enqueue_value(&vm, a.write_handle(), b.write_handle());
            let next = match change {
                "compatible" => SOURCE.replace("+input.inner.delta", "+input.inner.delta*10"),
                "input_type" => SOURCE
                    .replace("delta:Int32", "delta:UInt32")
                    .replace("+input.inner.delta", "+int32(input.inner.delta)"),
                "input_name" => SOURCE.replace("Inner", "Other"),
                "input_order" => {
                    SOURCE.replace("pub delta:Int32, tag:Int32", "tag:Int32, pub delta:Int32")
                }
                "privacy" => SOURCE.replace("pub delta", "delta"),
                "result_type" => SOURCE
                    .replace("first:Int32", "first:Int64")
                    .replace("first:a.n", "first:int64(a.n)"),
                "result_order" => SOURCE.replace(
                    "pub first:Int32, second:Int32",
                    "second:Int32, pub first:Int32",
                ),
                "permission" => SOURCE
                    .replace("b:&mut State", "b:&State")
                    .replace("b.n=b.n+input.inner.tag", ""),
                "missing" => SOURCE.replace("fn turn(", "fn changed("),
                _ => unreachable!(),
            };
            vm.load_source(&next)
                .unwrap_or_else(|e| panic!("{change}: {e}"));
            assert_eq!(vm.publish_domain_code().unwrap(), 2);
            vm.poll_tasks(100).unwrap();
            if change == "compatible" {
                assert!(matches!(
                    request.take_result(),
                    Some(DomainOutcome::Value(InstanceValue::Unit))
                ));
                assert_eq!(*output.borrow(), [30, 23]);
                read(&mut vm, a.read_handle(), 30);
                read(&mut vm, b.read_handle(), 23);
            } else {
                assert!(
                    matches!(request.take_access_result(),Some((DomainOutcome::Failed(e), Some(access::Rejection::Incompatible))) if e.code()==ErrorCode::TypeMismatch),
                    "{change}"
                );
                assert_eq!(*output.borrow(), [0, 0]);
                read(&mut vm, a.read_handle(), 10);
                read(&mut vm, b.read_handle(), 20);
                assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
                assert_eq!(vm.domain_revision(b.read_handle()).unwrap(), 0);
            }
            a.close();
            b.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}

#[test]
fn domain_value_packets_charge_results_and_keep_completed_writes_after_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let a = vm
            .create_domain("make", &[InstanceValue::Int32(10)])
            .unwrap();
        let b = vm
            .create_domain("make", &[InstanceValue::Int32(20)])
            .unwrap();
        let (request, output) = enqueue_value(&vm, a.write_handle(), b.write_handle());
        let charge = vm.domains.borrow().usage().2;
        assert!(charge >= 2 * 8);
        request.cancel();
        vm.poll_tasks(100).unwrap();
        assert_eq!(*output.borrow(), [0, 0]);
        drop(request);
        read(&mut vm, a.read_handle(), 10);
        let (request, output) = enqueue_value(&vm, a.write_handle(), b.write_handle());
        vm.poll_tasks(100).unwrap();
        assert_eq!(*output.borrow(), [12, 23]);
        assert_eq!(vm.domains.borrow().usage().2, charge);
        request.cancel();
        drop(request);
        read(&mut vm, a.read_handle(), 12);
        let (mut pending, _) = enqueue_value(&vm, a.write_handle(), b.write_handle());
        b.close();
        vm.poll_tasks(100).unwrap();
        assert!(matches!(pending.take_result(), Some(DomainOutcome::Closed)));
        read(&mut vm, a.read_handle(), 12);
        a.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn domain_value_packets_reject_preflight_failures_and_isolate_only_writes_on_body_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let a = vm
            .create_domain("make", &[InstanceValue::Int32(10)])
            .unwrap();
        let b = vm
            .create_domain("make", &[InstanceValue::Int32(20)])
            .unwrap();
        for handles in [
            [a.write_handle(), a.write_handle()],
            [a.read_handle(), b.write_handle()],
        ] {
            assert!(pairs::submit_values(
                &vm.domains,
                &handles,
                vm.current.as_ref().unwrap(),
                "turn",
                vec![2, 3],
                pairs::ValueOutput {
                    words: Rc::new(RefCell::new(vec![0; 2])),
                    snapshot: false
                },
                vm.interpreter_limits()
            )
            .is_err());
        }
        // A budget just below the complete request (including result storage)
        // must reject admission before the operation can mutate either root.
        let (request, _) = enqueue_value(&vm, a.write_handle(), b.write_handle());
        let bytes = vm.domains.borrow().usage().2;
        drop(request);
        vm.set_domain_limits(DomainLimits {
            max_request_bytes: bytes - 1,
            ..Default::default()
        })
        .unwrap();
        assert!(pairs::submit_values(
            &vm.domains,
            &[a.write_handle(), b.write_handle()],
            vm.current.as_ref().unwrap(),
            "turn",
            vec![2, 3],
            pairs::ValueOutput {
                words: Rc::new(RefCell::new(vec![0; 2])),
                snapshot: false
            },
            vm.interpreter_limits()
        )
        .is_err());
        vm.set_domain_limits(Default::default()).unwrap();
        assert_eq!(vm.domains.borrow().usage().1, 0);
        read(&mut vm, a.read_handle(), 10);
        read(&mut vm, b.read_handle(), 20);
        let failing = format!("import joky/check {}", SOURCE
            .replace("b:&mut State", "b:&State")
            .replace("b.n=b.n+input.inner.tag", "let message=\"value operation failure\" check.assert(input.inner.delta==0,&message)"));
        vm.load_source(&failing).unwrap();
        assert_eq!(vm.publish_domain_code().unwrap(), 2);
        let (mut request, output) = enqueue_value(&vm, a.write_handle(), b.read_handle());
        vm.poll_tasks(100).unwrap();
        assert!(matches!(
            request.take_result(),
            Some(DomainOutcome::Failed(_))
        ));
        assert_eq!(*output.borrow(), [0, 0]);
        assert!(vm.queue_domain_call(a.read_handle(), "read", &[]).is_err());
        read(&mut vm, b.read_handle(), 20);
        a.close();
        b.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn domain_snapshot_values_capture_at_execution_and_freeze_before_collection() {
    let source = r#"
struct State {n:Int32}
struct Reply {pub n:Int32,wide:UInt64}
fn make(n:Int32)->State {State{n:n}}
fn read(s:&State)->Int32{s.n}
fn set(s:&mut State,n:Int32){s.n=n}
fn one(s:&State)->Reply {Reply{n:s.n,wide:18446744073709551615}}
fn two(a:&State,b:&State)->Reply {Reply{n:a.n+b.n,wide:18446744073709551615}}
fn broken(s:&State)->Reply {Reply{n:1/(s.n-7),wide:0}}
"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for pair in [false, true] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source).unwrap();
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let handles = if pair {
                vec![a.read_handle(), b.read_handle()]
            } else {
                vec![a.read_handle()]
            };
            let output = Rc::new(RefCell::new(vec![0; 2]));
            let mut snapshot = pairs::submit_values(
                &vm.domains,
                &handles,
                vm.current.as_ref().unwrap(),
                if pair { "two" } else { "one" },
                vec![],
                pairs::ValueOutput {
                    words: output.clone(),
                    snapshot: true,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            vm.load_source(
                &source
                    .replace("n:s.n,", "n:s.n+100,")
                    .replace("n:a.n+b.n,", "n:a.n+b.n+100,"),
            )
            .unwrap();
            assert_eq!(vm.publish_domain_code().unwrap(), 2);
            let mut write = vm
                .queue_domain_call(a.write_handle(), "set", &[InstanceValue::Int32(99)])
                .unwrap();
            vm.poll_tasks(100).unwrap();
            assert!(matches!(write.take_result(), Some(DomainOutcome::Value(_))));
            let (result, _, reads) = snapshot.take_snapshot_result().unwrap();
            assert!(matches!(result, DomainOutcome::Value(InstanceValue::Unit)));
            let reads = reads.unwrap();
            assert_eq!(reads.first.revision, 0);
            assert_eq!(reads.first.code, 1);
            assert_eq!(
                reads.validate(&vm.domains.borrow()),
                Err(commits::Status::Conflict)
            );
            assert_eq!(&*output.borrow(), &[if pair { 117 } else { 107 }, u64::MAX]);
            if pair {
                assert_eq!(reads.second().unwrap().revision, 0);
            }
            read(&mut vm, a.read_handle(), 99);
            a.close();
            b.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let a = vm
            .create_domain("make", &[InstanceValue::Int32(7)])
            .unwrap();
        let baseline = vm.domains.borrow().usage();
        let output = Rc::new(RefCell::new(vec![0; 2]));
        let mut snapshot = pairs::submit_values(
            &vm.domains,
            &[a.read_handle()],
            vm.current.as_ref().unwrap(),
            "broken",
            vec![],
            pairs::ValueOutput {
                words: output,
                snapshot: true,
            },
            vm.interpreter_limits(),
        )
        .unwrap();
        vm.poll_tasks(100).unwrap();
        let (result, _, reads) = snapshot.take_snapshot_result().unwrap();
        assert!(
            matches!(result,DomainOutcome::Failed(error) if error.message().contains("Division by zero"))
        );
        assert!(reads.is_none());
        assert_eq!(vm.domains.borrow().usage(), baseline);
        read(&mut vm, a.read_handle(), 7);
        assert_eq!(vm.domain_revision(a.read_handle()).unwrap(), 0);
    }
}
