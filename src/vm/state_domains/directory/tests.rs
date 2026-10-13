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
fn directory_rejects_wrong_state_types_without_executing_projection() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"import joky/domain
struct Room { value:Int32 }
struct Player { value:Int32 }
fn make(value:Int32)->Room { Room{value:value} }
fn wrong(player:&Player)->Int32 { player.value }
fn main() @suspends ->Int32 {
    let room=domain.create(make,42)
    let owner=domain.directory(1)
    let directory=owner.handle()
    directory.insert(1,room.handle());
    match domain.lookup(directory,1,wrong) { Some(_)=>1,None=>0 }
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
