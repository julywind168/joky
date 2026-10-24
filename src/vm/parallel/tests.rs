use super::*;
use crate::vm::{VmEngine, VmMode};
use std::sync::atomic::{AtomicUsize, Ordering};
const SOURCE: &str = r#"
        fn echo(text: String) @suspends -> String { text }
        fn text(text: String) @suspends -> String {
            let (a, b) = join { | echo(text) | echo("discarded") }
            a
        }
        fn raced(text: String) @suspends -> String { race { | echo(text) | echo("other") } }
        fn consume(text: String) @suspends { }
        fn branched(text: String) @suspends -> String {
            scope { branch { consume(text) } }
            "ready"
        }
        fn bad() @suspends -> Int32 { 1 / 0 }
    "#;
pub(crate) fn finish(task: PreparedParallelTask) -> Result<ParallelOutput, Error> {
    let mut running = task.start()?;
    while !running.resume()? {}
    running.take_result()
}
fn send<T: Send>() {}
#[test]
fn parallel_frozen_tasks_keep_code_and_owning_inputs_after_vm_drop() {
    send::<PreparedParallelTask>();
    send::<ParallelOutput>();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap();
        let budget = TaskBudget::new(1 << 20);
        let tasks: Vec<_> = ["text", "raced", "branched"]
            .into_iter()
            .map(|name| {
                vm.prepare_parallel_task(
                    name,
                    vec![TaskValue::String("owned 跨线程".into())],
                    vec![budget.clone()],
                )
                .unwrap()
            })
            .collect();
        vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
        drop(vm);
        let workers: Vec<_> = tasks
            .into_iter()
            .map(|task| std::thread::spawn(move || finish(task).unwrap()))
            .collect();
        for (worker, expected) in workers
            .into_iter()
            .zip(["owned 跨线程", "owned 跨线程", "ready"])
        {
            let output = worker.join().unwrap();
            assert_eq!(output.value(), &TaskValue::String(expected.into()));
            assert!(budget.used() > 0);
            drop(output);
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn parallel_failure_budget_and_unstarted_packets_refund_once() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(SOURCE).unwrap();
        let mut completed = false;
        for bytes in (0..32768).step_by(64) {
            let budget = TaskBudget::new(bytes);
            let result = vm
                .prepare_parallel_task(
                    "text",
                    vec![TaskValue::String("input".repeat(8))],
                    vec![budget.clone()],
                )
                .and_then(finish);
            completed = result.is_ok();
            drop(result);
            assert_eq!(budget.used(), 0, "{mode:?}, {bytes}");
            if completed {
                break;
            }
        }
        assert!(completed);
        let budget = TaskBudget::new(1 << 20);
        let prepared = vm
            .prepare_parallel_task(
                "text",
                vec![TaskValue::String("never started".into())],
                vec![budget.clone()],
            )
            .unwrap();
        std::thread::spawn(move || drop(prepared)).join().unwrap();
        assert_eq!(budget.used(), 0);
        let failed = vm
            .prepare_parallel_task("bad", vec![], vec![budget.clone()])
            .unwrap();
        assert!(std::thread::spawn(move || finish(failed))
            .join()
            .unwrap()
            .is_err());
        assert_eq!(budget.used(), 0);
    }
}

static HOST_CALLS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn host(frame: *mut CpsFrame) -> i32 {
    HOST_CALLS.fetch_add(1, Ordering::SeqCst);
    (*frame).result = (&(*frame).slots)[0] + 1;
    0
}
#[test]
fn parallel_host_admission_is_explicit_and_frozen_across_replacement() {
    let source = "extern fn host(n: Int32) @suspends -> Int32 fn root(n: Int32) @suspends -> Int32 { host(n) }";
    let sig = FunctionSignature::new(vec![Type::Int32], Type::Int32);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        unsafe {
            vm.register_suspending_fn("host", sig.clone(), host)
                .unwrap();
        }
        vm.load_source(source).unwrap();
        let calls = HOST_CALLS.load(Ordering::SeqCst);
        assert!(vm
            .prepare_parallel_task(
                "root",
                vec![TaskValue::Scalar(InstanceValue::Int32(41))],
                vec![]
            )
            .is_err());
        assert_eq!(HOST_CALLS.load(Ordering::SeqCst), calls);
        unsafe {
            vm.register_parallel_suspending_fn("host", sig.clone(), host, 1, Arc::new(()))
                .unwrap();
        }
        // New evidence cannot upgrade a previously loaded code snapshot.
        assert!(vm
            .prepare_parallel_task(
                "root",
                vec![TaskValue::Scalar(InstanceValue::Int32(41))],
                vec![]
            )
            .is_err());
        vm.load_source(source).unwrap();
        let task = vm
            .prepare_parallel_task(
                "root",
                vec![TaskValue::Scalar(InstanceValue::Int32(41))],
                vec![],
            )
            .unwrap();
        unsafe {
            vm.register_suspending_fn("host", sig.clone(), host)
                .unwrap();
        }
        vm.load_source(source).unwrap();
        assert!(vm
            .prepare_parallel_task(
                "root",
                vec![TaskValue::Scalar(InstanceValue::Int32(41))],
                vec![]
            )
            .is_err());
        drop(vm);
        assert_eq!(
            std::thread::spawn(move || finish(task).unwrap().into_value())
                .join()
                .unwrap(),
            TaskValue::Scalar(InstanceValue::Int32(42))
        );
    }
}

// A test-only Local dispatch bridge isolates TP2 ownership from TP3 scheduling.
// Generated join/race/branch children use the production frozen-code preparation
// and one shared allocation root; no VM, bytecode compilation or frame crosses.
type Worker = std::thread::JoinHandle<Result<ParallelOutput, Error>>;
thread_local! {
    static CHILD_CODE: std::cell::RefCell<Option<(Code, Storage)>> = const { std::cell::RefCell::new(None) };
    static WORKERS: std::cell::RefCell<Vec<Option<Worker>>> = const { std::cell::RefCell::new(Vec::new()) };
}
static OVERLAP: std::sync::OnceLock<(std::sync::Mutex<usize>, std::sync::Condvar)> =
    std::sync::OnceLock::new();
static THREADS: std::sync::Mutex<Vec<std::thread::ThreadId>> = std::sync::Mutex::new(Vec::new());
unsafe extern "C" fn overlap(_frame: *mut CpsFrame) -> i32 {
    THREADS.lock().unwrap().push(std::thread::current().id());
    let (count, wake) =
        OVERLAP.get_or_init(|| (std::sync::Mutex::new(0), std::sync::Condvar::new()));
    let mut count = count.lock().unwrap();
    *count += 1;
    let target = count.div_ceil(2) * 2;
    wake.notify_all();
    let (count, timeout) = wake
        .wait_timeout_while(count, std::time::Duration::from_secs(5), |count| {
            *count < target
        })
        .unwrap();
    if timeout.timed_out() && *count < target {
        crate::runtime::error::record_error(invalid("Frozen child execution did not overlap"));
        return crate::runtime::cps::RUNTIME_ERROR;
    }
    0
}
unsafe extern "C" fn stop_dispatch(frame: *mut CpsFrame, _reason: u32) {
    let frame = &mut *frame;
    if frame.state != 0 {
        let worker = WORKERS.with(|workers| workers.borrow_mut()[frame.state as usize - 1].take());
        if let Some(worker) = worker {
            drop(worker.join().unwrap());
        }
    }
}
unsafe extern "C" fn dispatch(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        let task = CHILD_CODE.with(|template| {
            let template = template.borrow();
            let (code, root) = template.as_ref().unwrap();
            PreparedParallelTask::in_root(
                code.clone(),
                "work",
                vec![TaskValue::Scalar(InstanceValue::Int32(
                    frame.slots[0] as i32,
                ))],
                root.clone(),
                Default::default(),
            )
        });
        let task = match task {
            Ok(task) => task,
            Err(error) => {
                crate::runtime::error::record_error(error);
                return crate::runtime::cps::RUNTIME_ERROR;
            }
        };
        let worker = std::thread::spawn(move || finish(task));
        frame.state = WORKERS.with(|workers| {
            let mut workers = workers.borrow_mut();
            workers.push(Some(worker));
            workers.len() as u32
        });
        frame.host_stop = Some(stop_dispatch);
        return 1;
    }
    let worker = WORKERS.with(|workers| {
        let mut workers = workers.borrow_mut();
        let worker = &mut workers[frame.state as usize - 1];
        worker
            .as_ref()
            .unwrap()
            .is_finished()
            .then(|| worker.take().unwrap())
    });
    let Some(worker) = worker else {
        return 1;
    };
    match worker.join().unwrap() {
        Ok(value) => {
            let TaskValue::Scalar(InstanceValue::Int32(value)) = value.into_value() else {
                unreachable!()
            };
            frame.result = value as u32 as u64;
            0
        }
        Err(error) => {
            crate::runtime::error::record_error(error);
            crate::runtime::cps::RUNTIME_ERROR
        }
    }
}

#[test]
fn parallel_frozen_children_share_one_root_in_join_race_and_branch() {
    let source = r#"
        extern fn overlap() @suspends
        extern fn dispatch(n: Int32) @suspends -> Int32
        fn consume(text: String) { }
        fn work(n: Int32) @suspends -> Int32 {
            overlap()
            let text = "worker-owned allocation"
            var i = 0
            var sum = 0
            while i < 100 { sum = sum + i; i = i + 1 }
            consume(text)
            n + sum - 4950
        }
        fn joined(n: Int32) @suspends -> Int32 { let (a, b) = join { | dispatch(n) | dispatch(n + 1) }; a + b }
        fn raced(n: Int32) @suspends -> Int32 { race { | dispatch(n) | dispatch(n + 1) } }
        fn branched(n: Int32) @suspends -> Int32 { scope { branch { dispatch(n); }; branch { dispatch(n + 1); } }; n }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        unsafe {
            vm.register_suspending_fn(
                "dispatch",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                dispatch,
            )
            .unwrap();
            vm.register_parallel_suspending_fn(
                "overlap",
                FunctionSignature::new(vec![], Type::Unit),
                overlap,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        for name in ["joined", "raced", "branched"] {
            let budget = TaskBudget::new(1 << 20);
            let template = vm
                .prepare_parallel_task(
                    "work",
                    vec![TaskValue::Scalar(InstanceValue::Int32(0))],
                    vec![budget.clone()],
                )
                .unwrap();
            CHILD_CODE
                .with(|slot| slot.replace(Some((template.code.clone(), template.root.clone()))));
            drop(template);
            THREADS.lock().unwrap().clear();
            let mut root = vm.call_async(name, &[41]).unwrap();
            while !root.is_finished() {
                root.resume().unwrap();
            }
            let value = root.cps_result().unwrap() as i32;
            match name {
                "joined" => assert_eq!(value, 83),
                "raced" => assert!([41, 42].contains(&value)),
                _ => assert_eq!(value, 41),
            }
            drop(root);
            WORKERS.with(|workers| {
                let mut workers = workers.borrow_mut();
                assert!(workers.iter().all(Option::is_none));
                workers.clear();
            });
            let threads = THREADS.lock().unwrap();
            assert_eq!(threads.len(), 2);
            assert_ne!(threads[0], threads[1]);
            assert!(!threads.contains(&std::thread::current().id()));
            assert!(budget.used() > 0, "root retains completed branch shards");
            CHILD_CODE.with(|slot| slot.borrow_mut().take());
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn parallel_pending_cancellation_cleans_on_the_worker_and_releases_code_last() {
    struct Owner(Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(std::thread::current().id());
        }
    }
    unsafe extern "C" fn pending(frame: *mut CpsFrame) -> i32 {
        (*frame).state = 1;
        1
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let drops = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        unsafe {
            vm.register_parallel_suspending_fn(
                "pause",
                FunctionSignature::new(vec![], Type::Unit),
                pending,
                0,
                Arc::new(Owner(drops.clone())),
            )
            .unwrap();
        }
        vm.load_source(r#"
            extern fn pause() @suspends
            fn parked(text: String) @suspends -> String { pause(); text }
            fn root(text: String) @suspends -> String { let (a, b) = join { | parked(text) | parked("pending owner") }; a }
        "#).unwrap();
        let budget = TaskBudget::new(1 << 20);
        let prepared = vm
            .prepare_parallel_task(
                "root",
                vec![TaskValue::String("input".into())],
                vec![budget.clone()],
            )
            .unwrap();
        drop(vm);
        assert!(drops.lock().unwrap().is_empty());
        let worker = std::thread::spawn(move || {
            let mut task = prepared.start().unwrap();
            assert!(!task.resume().unwrap());
            drop(task);
            std::thread::current().id()
        })
        .join()
        .unwrap();
        assert_eq!(*drops.lock().unwrap(), [worker]);
        assert_ne!(worker, std::thread::current().id());
        assert_eq!(budget.used(), 0);
    }
}
