use super::*;
use crate::runtime::executor::HostExecutor;
use crate::vm::VmMode;
const SOURCE: &str = include_str!("../../../../tests/fixtures/directory.jk");
fn cancellation(boundaries: impl Clone + IntoIterator<Item = usize>, exhaustive: bool) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        let baseline = crate::runtime::arena::LIVE.with(Cell::get);
        let mut complete = false;
        for boundary in boundaries.clone() {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut done = false;
            for _ in 0..boundary {
                for result in executor.poll(1).unwrap().completed {
                    assert_eq!(
                        result
                            .result
                            .unwrap_or_else(|e| panic!("{mode:?} boundary={boundary}: {e}")),
                        0
                    );
                    done = true;
                }
                if done {
                    break;
                }
            }
            executor.cancel_group(&group);
            executor.poll(100).unwrap();
            assert_eq!(executor.task_count(), 0);
            assert!(executor.domains().borrow().directories.is_empty());
            assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
            drop(executor);
            assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), baseline);
            if done {
                complete = true;
                break;
            }
        }
        if exhaustive {
            assert!(complete, "{mode:?}: cancellation sweep missed completion");
        }
    }
}
#[test]
fn directory_generated_lifetimes_and_cancellation() {
    cancellation(
        [
            0, 1, 2, 3, 10, 20, 50, 100, 200, 500, 1000, 2000, 4000, 8000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel directory publication and routing at every interpreter instruction"]
fn exhaustive_directory_cancellation() {
    cancellation(0..=16384, true);
}
#[test]
fn directory_budgets_executor_identity_and_shutdown() {
    let vm = VmEngine::new();
    assert!(create(&vm.domains, 0).is_err());
    assert!(create(&vm.domains, u64::MAX).is_err());
    for limit in [0, 1, 64, 256, 1024] {
        vm.domains.borrow().budget.limit.set(limit);
        drop(create(&vm.domains, 2));
        assert!(vm.domains.borrow().directories.is_empty());
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
    vm.domains.borrow().budget.limit.set(usize::MAX);
    let owner = create(&vm.domains, 2).unwrap();
    let other = VmEngine::new();
    assert!(table(&other.domains, owner.token).is_none());
    assert_eq!(insert(&other.domains, owner.token, 1, None, 1), 1);
    assert!(!remove(&other.domains, owner.token, 1, 1));
    shutdown(&vm.domains);
    assert!(table(&vm.domains, owner.token).is_none());
    drop(owner);
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}

#[test]
fn directory_rejects_wrong_state_types_without_user_projection() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"import joky/domain
struct Room { value:Int32 }
struct Player { value:Int32 }
fn make(value:Int32)->Room { Room{value:value} }
fn main() @suspends ->Int32 {
    let room=domain.create(make,42)
    let owner=domain.directory(1)
    let directory=owner.handle()
    directory.insert(1,room.handle());
    match domain.lookup::<Player>(directory,1) { Some(_)=>1,None=>0 }
}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let error =
            crate::runtime::driver::block_on(vm.prepare_async("main", &[]).unwrap()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert!(error.to_string().contains("Directory target"));
        assert_eq!(crate::runtime::arena::LIVE.with(Cell::get), 0);
    }
}

#[test]
fn directory_owned_roots_reject_foreign_executors_and_close_on_shutdown() {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source("struct Room { value:Int32 } fn make(value:Int32)->Room { Room{value:value} }")
        .unwrap();
    let directory = create(&vm.domains, 1).unwrap();
    let foreign = VmEngine::new();
    let room = vm
        .create_domain("make", &[InstanceValue::Int32(7)])
        .unwrap();
    let handle = room.read_handle();
    assert_eq!(insert_owned(&foreign.domains, directory.token, 1, room), 4);
    assert!(vm.domain_revision(handle).is_err());
    let room = vm
        .create_domain("make", &[InstanceValue::Int32(8)])
        .unwrap();
    let handle = room.read_handle();
    assert_eq!(insert_owned(&vm.domains, directory.token, 1, room), 0);
    assert!(vm.domain_revision(handle).is_ok());
    shutdown(&vm.domains);
    assert!(vm.domain_revision(handle).is_err());
    drop(directory);
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}

#[test]
fn directory_owned_quarantined_routes_release_roots_without_borrowing_registry() {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source("struct Room { value:Int32 } fn make(value:Int32)->Room { Room{value:value} }")
        .unwrap();
    let directory = create(&vm.domains, 1).unwrap();
    let room = vm
        .create_domain("make", &[InstanceValue::Int32(7)])
        .unwrap();
    let handle = room.read_handle();
    assert_eq!(insert_owned(&vm.domains, directory.token, 1, room), 0);
    vm.domains
        .borrow_mut()
        .entries
        .get_mut(&handle.sequence)
        .unwrap()
        .failure = Some(invalid("Quarantined test domain"));
    let room = vm
        .create_domain("make", &[InstanceValue::Int32(8)])
        .unwrap();
    let handle = room.read_handle();
    // Capacity reclamation must close the quarantined owned domain after releasing borrows.
    assert_eq!(insert_owned(&vm.domains, directory.token, 2, room), 0);
    assert_eq!(vm.domains.borrow().entries.len(), 1);
    vm.domains
        .borrow_mut()
        .entries
        .get_mut(&handle.sequence)
        .unwrap()
        .failure = Some(invalid("Quarantined test domain"));
    // A stale lookup follows the same cleanup rule before any type witness is needed.
    assert_eq!(
        lookup(
            &vm.domains,
            directory.token,
            2,
            vm.current.as_ref().unwrap(),
            "",
            false
        )
        .unwrap(),
        0
    );
    assert!(vm.domains.borrow().entries.is_empty());
    drop(directory);
    assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
}

#[test]
fn directory_owned_pruning_rechecks_resources_closed_by_task_cleanup() {
    unsafe extern "C" fn pending(_: *mut CpsFrame) -> i32 {
        1
    }
    for close_directory in [false, true] {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.load_source(
            "struct Room { value:Int32 } fn make(value:Int32)->Room { Room{value:value} }",
        )
        .unwrap();
        let mut directory = Some(create(&vm.domains, 1).unwrap());
        let directory_token = directory.as_ref().unwrap().token;
        let table = directory.as_ref().unwrap().table.clone();
        let room = vm
            .create_domain("make", &[InstanceValue::Int32(7)])
            .unwrap();
        let handle = room.read_handle();
        assert_eq!(insert_owned(&vm.domains, directory_token, 1, room), 0);
        let mut target = Some(
            vm.create_domain("make", &[InstanceValue::Int32(8)])
                .unwrap(),
        );
        let target_handle = target.as_ref().unwrap().read_handle();
        let token = vm.domains.borrow().entries[&target_handle.sequence].source_token * 2 + 1;
        // The retired domain's task owns a resource needed by the next insertion.
        let resource: Rc<dyn std::any::Any> = if close_directory {
            Rc::new(directory.take().unwrap())
        } else {
            Rc::new(target.take().unwrap())
        };
        let task = unsafe { Continuation::new(pending, CpsFrame::new(0), ModuleId(0), resource) };
        let group = TaskGroup::new();
        let task = vm.executor.spawn(&group, task).unwrap();
        {
            let mut domains = vm.domains.borrow_mut();
            let domain = domains.entries.get_mut(&handle.sequence).unwrap();
            domain.tasks.insert(task, BoundTask { group, module: 0 });
            domain.failure = Some(invalid("Quarantined test domain"));
        }
        assert_eq!(
            insert(&vm.domains, directory_token, 2, None, token),
            if close_directory { 1 } else { 4 }
        );
        assert!(table.borrow().entries.is_empty());
        assert_eq!(vm.executor.task_count(), 0);
        drop(target);
        drop(directory);
        drop(table);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn state_domains_paths_revalidate_keys_generations_aliases_and_retry_limits() {
    const SOURCE: &str = r#"import joky/domain
struct S { key:UInt64,n:Int32 }
fn make(n:Int32)->S { S{key:2,n:n} }
fn route(s:&mut S,key:UInt64) {s.key=key}
fn read(s:&S)->Int32 {s.n}
fn operation(first:domain.WriteHandle<S>,routes:domain.DirectoryHandle) @suspends ->Int32 {
    when "update" (mut first,mut second?:S=routes[first.key]) {
        first.n+=second.n; first.n
    } else {first.n+=10; first.n} alias {first.n+=100; first.n}.unwrap()
}"#;
    const ENTRY: &str = "$when/named/4:main/9:operation/6:757064617465";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        for action in [
            "replace", "key", "missing", "alias", "limit", "closed", "cancel",
        ] {
            let a = vm
                .create_domain("make", &[InstanceValue::Int32(1)])
                .unwrap();
            let b = vm
                .create_domain("make", &[InstanceValue::Int32(2)])
                .unwrap();
            let c = vm
                .create_domain("make", &[InstanceValue::Int32(3)])
                .unwrap();
            let table = create(&vm.domains, 4).unwrap();
            let token = |owner: &DomainOwner| {
                vm.domains.borrow().get(owner.handle).unwrap().source_token * 2 + 1
            };
            let at = token(&a);
            let bt = token(&b);
            let ct = token(&c);
            assert_eq!(insert(&vm.domains, table.token, 1, None, at), 0);
            assert_eq!(insert(&vm.domains, table.token, 2, None, bt), 0);
            assert_eq!(insert(&vm.domains, table.token, 3, None, ct), 0);
            let mut prior = if matches!(action, "key" | "missing" | "alias") {
                Some(
                    vm.queue_domain_call(
                        a.write_handle(),
                        "route",
                        &[InstanceValue::UInt64(match action {
                            "key" => 3,
                            "missing" => 4,
                            _ => 1,
                        })],
                    )
                    .unwrap(),
                )
            } else {
                None
            };
            let output = Rc::new(RefCell::new(vec![0]));
            let mut request = pairs::paths::submit(
                &vm.domains,
                vm.current.as_ref().unwrap().clone(),
                ENTRY,
                vec![1, 0, 0, 0, 0, at, 0, 1, 1, 0, table.token, 0, 1].into(),
                vec![],
                pairs::ValueOutput {
                    words: output.clone(),
                    snapshot: false,
                },
                vm.interpreter_limits(),
            )
            .unwrap();
            if matches!(action, "replace" | "limit") {
                assert_eq!(insert(&vm.domains, table.token, 2, Some(bt), ct), 0);
                b.close();
            }
            if action == "limit" {
                vm.domains.borrow_mut().limits.max_path_retries = 0;
            }
            if action == "closed" {
                close_table(&table.table);
            }
            if action == "cancel" {
                request.cancel();
            }
            vm.poll_tasks(10000).unwrap();
            if let Some(prior) = &mut prior {
                assert!(matches!(prior.take_result(), Some(DomainOutcome::Value(_))));
            }
            let (outcome, rejection) = request.take_access_result().unwrap();
            match action {
                "limit" => assert_eq!(rejection, Some(access::Rejection::RetryLimit)),
                "closed" => assert_eq!(rejection, Some(access::Rejection::Closed)),
                "cancel" => assert!(matches!(outcome, DomainOutcome::Cancelled)),
                _ => {
                    assert_eq!(rejection, None, "{action}: {outcome:?}");
                    assert_eq!(
                        *output.borrow(),
                        vec![match action {
                            "missing" => 11,
                            "alias" => 101,
                            _ => 4,
                        }]
                    );
                }
            }
            vm.domains.borrow_mut().limits.max_path_retries = 64;
            a.close();
            b.close();
            c.close();
            drop(table);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
