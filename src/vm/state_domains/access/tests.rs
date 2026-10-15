use super::*;
use crate::runtime::executor::TaskGroup;
use crate::vm::VmMode;

const SOURCE: &str = r#"
import joky/domain
struct State { n:Int32 }
struct Packet { n:Int32 }
fn make(n:Int32)->State {State{n:n}}
fn read(s:&State)->Int32 {s.n}
fn scalar(s:&mut State,n:Int32)->Int32 {s.n=n n}
fn packet(s:&mut State,p:Packet)->Packet {s.n=p.n p}
fn query_scalar(s:&State,n:Int32)->Int32 {s.n+n}
fn query_packet(s:&State,p:Packet)->Packet {Packet{n:s.n+p.n}}
fn run_query_scalar(n:Int32) @suspends ->Int32 {
    match domain.query(query_scalar,n) {
        Err(domain.DomainAccessError.Incompatible)=>41,
        Err(domain.DomainAccessError.ResourceLimit)=>42,
        Ok(value)=>value,
        _=>0
    }
}
fn run_query_packet(n:Int32) @suspends ->Int32 {
    match domain.query(query_packet,Packet{n:n}) {
        Err(domain.DomainAccessError.Incompatible)=>41,
        Err(domain.DomainAccessError.ResourceLimit)=>42,
        Ok(value)=>value.n,
        _=>0
    }
}
fn run_scalar(n:Int32) @suspends ->Int32 {
    match domain.write(scalar,n) {
        Err(domain.DomainAccessError.Incompatible)=>41,
        Err(domain.DomainAccessError.ResourceLimit)=>42,
        _=>0
    }
}
fn run_packet(n:Int32) @suspends ->Int32 {
    match domain.write(packet,Packet{n:n}) {
        Err(domain.DomainAccessError.Incompatible)=>41,
        Err(domain.DomainAccessError.ResourceLimit)=>42,
        _=>0
    }
}
"#;

#[test]
fn state_domains_access_rejections_survive_admission_and_late_binding() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for operation in [
            "run_scalar",
            "run_packet",
            "run_query_scalar",
            "run_query_packet",
        ] {
            // Reject before queueing, after queueing, or because admission is full.
            for stage in 0..3 {
                let mut vm = VmEngine::with_mode(mode);
                vm.load_source(SOURCE).unwrap();
                let owner = vm
                    .create_domain("make", &[InstanceValue::Int32(7)])
                    .unwrap();
                let id = vm
                    .spawn_domain_task(
                        if operation.contains("query") {
                            owner.read_handle()
                        } else {
                            owner.write_handle()
                        },
                        operation,
                        &[InstanceValue::Int32(99)],
                    )
                    .unwrap();
                if stage == 1 {
                    for _ in 0..1000 {
                        vm.poll_tasks(1).unwrap();
                        if !vm.domains.borrow().requests.is_empty() {
                            break;
                        }
                    }
                    assert_eq!(vm.domains.borrow().requests.len(), 1);
                }
                if stage == 2 {
                    vm.set_domain_limits(DomainLimits {
                        max_request_bytes: 1,
                        ..Default::default()
                    })
                    .unwrap();
                } else {
                    // Keep state compatible but remove the operation from published code.
                    vm.load_source("struct State { n:Int32 } fn make(n:Int32)->State {State{n:n}} fn read(s:&State)->Int32{s.n}").unwrap();
                    assert_eq!(vm.publish_domain_code().unwrap(), 1);
                }
                let mut result = None;
                for _ in 0..1000 {
                    for done in vm.poll_tasks(1).unwrap().completed {
                        if done.id == id {
                            result = Some(done.result.unwrap());
                        }
                    }
                    if result.is_some() {
                        break;
                    }
                }
                assert_eq!(
                    result,
                    Some(if stage == 2 { 42 } else { 41 }),
                    "{mode:?}, operation={operation}, stage={stage}"
                );
                assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
                vm.set_domain_limits(Default::default()).unwrap();
                let mut read = vm
                    .queue_domain_call(owner.read_handle(), "read", &[])
                    .unwrap();
                vm.poll_tasks(100).unwrap();
                assert!(matches!(
                    read.take_result(),
                    Some(DomainOutcome::Value(InstanceValue::Int32(7)))
                ));
                owner.close();
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            }
        }
    }
}

#[test]
fn state_domains_access_terminal_rejection_is_not_reclassified_on_collection() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(7)])
            .unwrap();
        let mut request = vm
            .queue_domain_call(owner.write_handle(), "scalar", &[InstanceValue::Int32(99)])
            .unwrap();
        vm.load_source("struct State {n:Int32} fn make(n:Int32)->State{State{n:n}}")
            .unwrap();
        vm.publish_domain_code().unwrap();
        vm.poll_tasks(100).unwrap();
        owner.close();
        let (outcome, rejection) = request.take_access_result().unwrap();
        assert!(matches!(outcome, DomainOutcome::Failed(_)));
        assert_eq!(rejection, Some(Rejection::Incompatible));
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn state_domains_access_error_results_clean_up_on_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../../../tests/fixtures/state_domain_access_errors.jk"
        ))
        .unwrap();
        for boundary in [0, 1, 2, 5, 10, 30, 60, 100, 200, 500] {
            let group = TaskGroup::new();
            vm.executor
                .spawn(&group, vm.prepare_async("main", &[]).unwrap())
                .unwrap();
            for _ in 0..boundary {
                vm.poll_tasks(1).unwrap();
            }
            vm.executor.cancel_group(&group);
            assert_eq!(vm.task_count(), 0);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}

#[test]
fn state_domains_queries_bind_queued_inputs_to_current_code_without_revision_changes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for operation in ["run_query_scalar", "run_query_packet"] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(SOURCE).unwrap();
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(7)])
                .unwrap();
            let id = vm
                .spawn_domain_task(owner.read_handle(), operation, &[InstanceValue::Int32(99)])
                .unwrap();
            for _ in 0..1000 {
                vm.poll_tasks(1).unwrap();
                if !vm.domains.borrow().requests.is_empty() {
                    break;
                }
            }
            assert_eq!(vm.domains.borrow().requests.len(), 1);
            vm.load_source(
                &SOURCE
                    .replace("s.n+n", "s.n+n+10")
                    .replace("s.n+p.n", "s.n+p.n+10"),
            )
            .unwrap();
            assert_eq!(vm.publish_domain_code().unwrap(), 1);
            let mut result = None;
            for _ in 0..1000 {
                for done in vm.poll_tasks(1).unwrap().completed {
                    if done.id == id {
                        result = Some(done.result.unwrap());
                    }
                }
                if result.is_some() {
                    break;
                }
            }
            assert_eq!(result, Some(116), "{mode:?}, {operation}");
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
            owner.close();
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
