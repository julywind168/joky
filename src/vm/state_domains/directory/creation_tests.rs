use super::*;
use crate::runtime::executor::Clock;
use crate::vm::VmMode;
use std::time::{Duration, Instant};

const SOURCE: &str = r#"import joky/domain
#state struct S { value:Int32 }
impl S { #every(10) fn tick(&mut self) {self.value+=1} }
fn make()->S {S{value:10}}
fn read(s:&S)->Int32 {s.value}
fn empty(s:&S)->Bool {s.value==0}
fn operation(routes:domain.DirectoryHandle,delta:Int32,reject:Bool) @suspends ->Int32 {
    when "create" (mut room:S=routes.get_or_stage("room",make())) {
        room.value+=delta
        let result:Result<Int32,Int32>=if reject {Err(7)} else {Ok(room.value)}
        result
    }.unwrap().unwrap()
}"#;
const ENTRY: &str = "$when/named/4:main/9:operation/6:637265617465";
struct ManualClock(Cell<Instant>);
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
fn setup(mode: VmMode, source: &str, tasks: usize) -> (VmEngine, Rc<ManualClock>) {
    let mut vm = VmEngine::with_mode(mode);
    let clock = Rc::new(ManualClock(Cell::new(Instant::now())));
    vm.configure_tasks(clock.clone(), tasks).unwrap();
    vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
    (vm, clock)
}
fn submit(
    vm: &VmEngine,
    module: &LoadedModule,
    directory: u64,
    reject: bool,
) -> (DomainRequest, Rc<RefCell<Vec<u64>>>) {
    let output = Rc::new(RefCell::new(vec![0; 3]));
    let request = pairs::paths::submit(
        &vm.domains,
        module.clone(),
        ENTRY,
        pairs::paths::Packet {
            source_key: 0,
            descriptors: vec![0, 6, 0, 0, directory, 0, 0],
            keys: vec!["room".into()],
        },
        vec![2, u64::from(reject)],
        pairs::ValueOutput {
            words: output.clone(),
            snapshot: false,
        },
        vm.interpreter_limits(),
    )
    .unwrap();
    (request, output)
}
fn read(vm: &VmEngine, handle: DomainHandle) -> i32 {
    let mut request = vm
        .queue_domain_call(handle.read_only(), "read", &[])
        .unwrap();
    vm.poll_tasks(10000).unwrap();
    match request.take_result().unwrap() {
        DomainOutcome::Value(InstanceValue::Int32(n)) => n,
        other => panic!("{other:?}"),
    }
}
#[test]
fn state_domains_creation_serializes_names_and_publishes_owners_and_timers_once() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, clock) = setup(mode, SOURCE, 64);
        let table = create(&vm.domains, 2).unwrap();
        let old = vm.current.as_ref().unwrap().clone();
        let (mut first, a) = submit(&vm, &old, table.token, false);
        let (mut second, b) = submit(&vm, &old, table.token, false);
        assert_eq!(vm.domains.borrow().entries.len(), 0);
        vm.poll_tasks(10000).unwrap();
        assert!(first.take_access_result().unwrap().1.is_none());
        assert!(second.take_access_result().unwrap().1.is_none());
        assert_eq!(*a.borrow(), vec![0, 12, 0]);
        assert_eq!(*b.borrow(), vec![0, 14, 0]);
        assert_eq!(table.table.borrow().entries.len(), 1);
        assert_eq!(vm.domains.borrow().entries.len(), 1);
        assert_eq!(vm.task_count(), 1);
        let handle = resolve_key(&vm.domains.borrow(), table.token, &Key::Name("room".into()))
            .unwrap()
            .unwrap();
        assert_eq!(read(&vm, handle), 14);
        clock.0.set(clock.0.get() + Duration::from_millis(10));
        vm.poll_tasks(10000).unwrap();
        assert_eq!(read(&vm, handle), 15);
        assert_eq!(
            remove_if(
                &vm.domains,
                table.token,
                &Key::Name("room".into()),
                &old,
                "empty"
            )
            .unwrap(),
            (0, false)
        );
        let token = table.table.borrow().entries[0].token;
        assert!(remove_key(
            &vm.domains,
            table.token,
            &Key::Name("room".into()),
            token
        ));
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().entries.len(), 0);
        // No existing root anchors this queued old flow. Publication still selects
        // the new constructor and operation together without retaining old code.
        let (mut late, output) = submit(&vm, &old, table.token, false);
        vm.publish_source(&SOURCE.replace("value:10", "value:20"))
            .unwrap();
        vm.poll_tasks(10000).unwrap();
        assert!(late.take_access_result().unwrap().1.is_none());
        assert_eq!(*output.borrow(), vec![0, 22, 0]);
        drop(table);
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}
#[test]
fn state_domains_creation_rolls_back_timers_on_rejection_cancellation_and_partial_admission() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for action in ["reject", "cancel", "closed", "task_limit", "constructor"] {
            let source = if action == "task_limit" {
                SOURCE.replace(
                    "fn make()",
                    "impl S {#every(10) fn second(&mut self){self.value+=100}} fn make()",
                )
            } else if action == "constructor" {
                SOURCE.replace("S{value:10}", "let zero=0; S{value:10/zero}")
            } else {
                SOURCE.into()
            };
            let (vm, _) = setup(mode, &source, if action == "task_limit" { 2 } else { 64 });
            let table = create(&vm.domains, 2).unwrap();
            let baseline = vm.domains.borrow().budget.bytes.get();
            let (mut request, output) = submit(
                &vm,
                vm.current.as_ref().unwrap(),
                table.token,
                action == "reject",
            );
            if action == "cancel" {
                request.cancel();
            }
            if action == "closed" {
                close_table(&table.table);
            }
            vm.poll_tasks(10000).unwrap();
            let (outcome, rejection) = request.take_access_result().unwrap();
            match action {
                "reject" => {
                    assert!(rejection.is_none());
                    assert_eq!(*output.borrow(), vec![1, 0, 7]);
                }
                "constructor" => {
                    assert!(rejection.is_none());
                    assert!(matches!(outcome, DomainOutcome::Failed(_)));
                }
                "cancel" => assert!(matches!(outcome, DomainOutcome::Cancelled)),
                "closed" => assert_eq!(rejection, Some(access::Rejection::Closed)),
                "task_limit" => assert_eq!(rejection, Some(access::Rejection::ResourceLimit)),
                _ => unreachable!(),
            }
            assert!(table.table.borrow().entries.is_empty(), "{action}");
            assert_eq!(vm.task_count(), 0, "{action}");
            assert_eq!(vm.domains.borrow().entries.len(), 0, "{action}");
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline, "{action}");
            drop(table);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        }
    }
}
#[test]
fn state_domains_creation_budgets_fail_before_any_root_or_route_is_published() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let (mut vm, _) = setup(mode, SOURCE, 64);
        let table = create(&vm.domains, 2).unwrap();
        let baseline = vm.domains.borrow().budget.bytes.get();
        let mut completed = false;
        for extra in (0..=16384).step_by(128) {
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: baseline + extra,
                ..Default::default()
            })
            .unwrap();
            let (mut request, _) = submit(&vm, vm.current.as_ref().unwrap(), table.token, false);
            vm.poll_tasks(10000).unwrap();
            let (_, rejection) = request.take_access_result().unwrap();
            if rejection.is_none() {
                let token = table.table.borrow().entries[0].token;
                assert!(remove_key(
                    &vm.domains,
                    table.token,
                    &Key::Name("room".into()),
                    token
                ));
                completed = true;
            } else {
                assert_eq!(rejection, Some(access::Rejection::ResourceLimit));
            }
            assert_eq!(vm.task_count(), 0);
            assert_eq!(vm.domains.borrow().entries.len(), 0);
            assert_eq!(vm.domains.borrow().budget.bytes.get(), baseline);
            if completed {
                break;
            }
        }
        assert!(
            completed,
            "{mode:?}: budget scan missed a complete creation"
        );
        drop(table);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn controlled_creation_publication_replacement_and_close_replay() {
    use crate::runtime::executor::controlled::ControlledExecutor;
    fn run(mode: VmMode, control: &ControlledExecutor) -> Vec<Vec<u64>> {
        let mut vm = VmEngine::with_mode(mode);
        vm.executor = Rc::new(control.executor().clone());
        vm.domains = vm.executor.domains();
        vm.load_source(SOURCE).unwrap();
        let table = create(&vm.domains, 2).unwrap();
        let old = vm.current.as_ref().unwrap().clone();
        let (mut first, a) = submit(&vm, &old, table.token, false);
        let (mut second, b) = submit(&vm, &old, table.token, false);
        let (mut cancelled, _) = submit(&vm, &old, table.token, false);
        control
            .inject(1, || {
                cancelled.cancel();
                vm.publish_source(&SOURCE.replace("value:10", "value:20"))
                    .unwrap();
            })
            .unwrap();
        assert_eq!(vm.poll_tasks(64).unwrap().polled, 2);
        assert!(first.take_access_result().unwrap().1.is_none());
        assert!(second.take_access_result().unwrap().1.is_none());
        assert!(matches!(
            cancelled.take_result(),
            Some(DomainOutcome::Cancelled)
        ));
        assert!(cancelled.take_result().is_none());
        let values = vec![a.borrow().clone(), b.borrow().clone()];
        let mut sorted = values.clone();
        sorted.sort();
        assert_eq!(sorted, [vec![0, 22, 0], vec![0, 24, 0]]);
        assert_eq!(table.table.borrow().entries.len(), 1);
        let key = Key::Name("room".into());
        let before = resolve_key(&vm.domains.borrow(), table.token, &key)
            .unwrap()
            .unwrap();
        let (mut late, output) = submit(&vm, &old, table.token, false);
        control
            .inject(2, || {
                let token = table.table.borrow().entries[0].token;
                assert!(remove_key(&vm.domains, table.token, &key, token));
            })
            .unwrap();
        vm.poll_tasks(64).unwrap();
        assert!(late.take_access_result().unwrap().1.is_none());
        assert_eq!(*output.borrow(), vec![0, 22, 0]);
        let after = resolve_key(&vm.domains.borrow(), table.token, &key)
            .unwrap()
            .unwrap();
        assert_ne!(before, after);
        assert!(vm.domain_revision(before).is_err());
        assert_eq!(vm.domains.borrow().entries.len(), 1);
        assert_eq!(vm.task_count(), 1); // Only the replacement's timer survives.
        let (mut closing, _) = submit(&vm, &old, table.token, false);
        control.inject(3, || close_table(&table.table)).unwrap();
        vm.poll_tasks(64).unwrap();
        let (outcome, rejection) = closing.take_access_result().unwrap();
        assert!(
            matches!(outcome, DomainOutcome::Closed)
                || rejection == Some(access::Rejection::Closed)
        );
        assert!(closing.take_result().is_none());
        drop(table);
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        control.finish().unwrap();
        values
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for seed in [0, 5] {
            let control = ControlledExecutor::new(seed, 64, 32).unwrap();
            let result = run(mode, &control);
            let replay = ControlledExecutor::replay(control.trace(), 64).unwrap();
            assert_eq!(run(mode, &replay), result);
        }
    }
}
