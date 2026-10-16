use super::*;
use crate::runtime::arena::{LIVE, TRANSFER_FAIL};
use crate::vm::VmMode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/state_domain_storage.jk");
fn owner(vm: &VmEngine, n: i32) -> DomainOwner {
    vm.create_domain("make", &[InstanceValue::Int32(n)])
        .unwrap()
}
fn read(vm: &mut VmEngine, owner: &DomainOwner, n: i32) {
    let mut request = vm
        .queue_domain_call(owner.read_handle(), "read", &[])
        .unwrap();
    vm.poll_tasks(100).unwrap();
    assert!(
        matches!(request.take_result(), Some(DomainOutcome::Value(InstanceValue::Int32(v))) if v == n)
    );
    let mut request = vm
        .queue_domain_call(owner.read_handle(), "inspect", &[])
        .unwrap();
    vm.poll_tasks(100).unwrap();
    assert!(matches!(
        request.take_result(),
        Some(DomainOutcome::Value(InstanceValue::Bool(true)))
    ));
}
#[test]
fn state_domains_storage_copy_budget_failure_refunds_every_partial_graph() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let owner = owner(&vm, 7);
        let baseline = LIVE.with(Cell::get);
        let domains = vm.domains.borrow();
        let domain = &domains.entries[&owner.handle.sequence];
        let root = domain.root.as_ref().unwrap();
        let budget = domains.budget.clone();
        let bytes = budget.bytes.get();
        let original = root.allocations().unwrap();
        let mut passed = false;
        let mut failed = 0;
        // Sweep the peak, including descriptors, nested Map storage and scratch.
        for allowance in (0..=100_000).step_by(97) {
            budget.limit.set(bytes + allowance);
            match root.snapshot(&domain.ty, budget.clone()) {
                Ok(copy) => {
                    assert!(copy
                        .allocations()
                        .unwrap()
                        .iter()
                        .all(|p| !original.contains(p)));
                    drop(copy);
                    passed = true;
                }
                Err(e) => {
                    assert_eq!(e.code(), ErrorCode::ResourceLimit);
                    failed += 1;
                }
            }
            assert_eq!(budget.bytes.get(), bytes, "{mode:?}: allowance {allowance}");
            assert_eq!(LIVE.with(Cell::get), baseline);
            if passed {
                break;
            }
        }
        assert!(passed && failed > 5);
        budget.limit.set(DomainLimits::default().max_state_bytes);
        drop(domains);
        read(&mut vm, &owner, 7);
        owner.close();
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn state_domains_storage_multi_failure_restores_readers_and_isolates_all_writers() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let baseline = LIVE.with(Cell::get);
        for (method, mutable_a, input) in [
            ("fail_swap", true, 0),
            ("fail_mixed", false, 0),
            ("copy_value", false, 4),
        ] {
            for fault in [None, Some(0), Some(1), Some(2)] {
                let a = owner(&vm, 7);
                let b = owner(&vm, 8);
                let c = owner(&vm, 9);
                let mut request = vm
                    .queue_domain_pair(
                        [
                            if mutable_a {
                                a.write_handle()
                            } else {
                                a.read_handle()
                            },
                            b.write_handle(),
                        ],
                        method,
                        &[InstanceValue::Int32(input)],
                    )
                    .unwrap();
                TRANSFER_FAIL.with(|f| f.set(fault));
                vm.poll_tasks(100).unwrap();
                TRANSFER_FAIL.with(|f| f.set(None));
                let success = method == "copy_value" && fault.is_none();
                let outcome = request.take_result().unwrap();
                assert_eq!(
                    matches!(outcome, DomainOutcome::Value(_)),
                    success,
                    "{mode:?} {method} {fault:?}: {outcome:?}"
                );
                if fault == Some(0) {
                    read(&mut vm, &a, 7);
                } else if mutable_a {
                    assert!(vm.queue_domain_call(a.read_handle(), "read", &[]).is_err());
                } else {
                    read(&mut vm, &a, 7);
                }
                if fault == Some(0) {
                    read(&mut vm, &b, 8);
                } else if success {
                    read(&mut vm, &b, 11);
                } else {
                    assert!(vm.queue_domain_call(b.read_handle(), "read", &[]).is_err());
                }
                read(&mut vm, &c, 9);
                a.close();
                b.close();
                c.close();
                drop(request);
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                assert_eq!(LIVE.with(Cell::get), baseline);
            }
        }
    }
}
#[test]
fn state_domains_storage_many_owned_roots_preserve_unique_cleanup() {
    let mut source = String::from("import joky/mem struct S{value:String,n:Int32} fn make(n:Int32)->S{S{value:\"state\",n:n}} fn read(s:&S)->Int32{s.n}");
    for count in [2, 3, 64, 65] {
        let params = (0..count)
            .map(|i| format!("s{i}:&mut S"))
            .collect::<Vec<_>>()
            .join(",");
        let swaps = (1..count)
            .map(|i| format!("mem.swap(s0,s{i}) "))
            .collect::<String>();
        source.push_str(&format!(" fn move{count}({params})->Int32{{{swaps} s0.n}}"));
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        for count in [2, 3, 64, 65] {
            let owners: Vec<_> = (0..count).map(|i| owner(&vm, i)).collect();
            let handles: Vec<_> = owners.iter().map(DomainOwner::write_handle).collect();
            let request = vm.queue_domain_access(&handles, &format!("move{count}"), &[]);
            if count == 65 {
                assert!(request.is_err());
            } else {
                let mut request = request.unwrap();
                vm.poll_tasks(100).unwrap();
                assert!(
                    matches!(request.take_result(),Some(DomainOutcome::Value(InstanceValue::Int32(n))) if n == count - 1)
                );
                for owner in &owners {
                    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
                }
            }
            for owner in owners {
                owner.close();
            }
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
