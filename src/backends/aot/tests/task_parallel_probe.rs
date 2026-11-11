//! TP0 bridge probe: one generated structured root owns both worker calls.
//! No production frame, Rc, arena, VM or continuation crosses a thread boundary.
//! This does not implement automatic branch lowering or asynchronous draining.
use super::{instances::shared_runtime_object, AotCompiler, ScratchDir};
use crate::{
    backends::{interp, jit},
    core::prepared::PreparedModule,
    runtime::{
        arena,
        bindings::{HostAbiKind, HostBinding, HostEntry},
        borrowed_host::{STOP_CANCELLED, STOP_COMPLETED},
        cps::{CpsEntry, CpsFrame},
        executor::{current_waker, HostExecutor, TaskGroup, TaskWaker},
        suspension::{Continuation, ModuleId},
    },
    FunctionSignature, Type, TypeRegistry,
};
use std::{
    any::Any,
    cell::RefCell,
    collections::HashMap,
    process::Command,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle, ThreadId},
    time::{Duration, Instant},
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/task_parallel_probe.jk");
const TIMEOUT: Duration = Duration::from_secs(10);
type Work = unsafe extern "C" fn(i32) -> i32;

#[derive(Clone, Copy, Debug)]
enum Backend {
    Jit,
    Interpreter,
    Aot,
}

// Only immutable, audited code entry points cross to workers. The artifact
// remains on the test thread until every root and worker has been destroyed.
#[derive(Clone, Copy)]
enum WorkerCode {
    Native(Work),
    Interpreted,
}

struct Artifact {
    owner: Rc<dyn Any>,
    roots: HashMap<String, (CpsEntry, usize)>,
    bytecode: Option<Rc<interp::BytecodeModule>>,
    worker: WorkerCode,
    // The library owner must be destroyed before its temporary directory.
    _scratch: Option<ScratchDir>,
}

fn host_bindings() -> HashMap<String, HostBinding> {
    [
        ("probe_enter", Type::Int32, enter as *const u8),
        ("probe_cancel", Type::Int32, checkpoint as *const u8),
    ]
    .into_iter()
    .map(|(name, ret, address)| {
        (
            name.to_owned(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature: FunctionSignature::new(vec![Type::Int32], ret),
                entry: HostEntry::Direct(address),
                owner: None,
            },
        )
    })
    .chain([
        (
            "probe_parent".into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature: FunctionSignature::new(vec![], Type::Unit),
                entry: HostEntry::Direct(parent as *const u8),
                owner: None,
            },
        ),
        (
            "probe_dispatch".into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature: FunctionSignature::new(vec![Type::Int32], Type::Int32),
                entry: HostEntry::Cps {
                    entry: dispatch,
                    frame_slots: Some(1),
                },
                owner: None,
            },
        ),
    ])
    .collect()
}

fn prepared() -> PreparedModule {
    PreparedModule::new(
        &crate::parser::parse(SOURCE).unwrap(),
        &TypeRegistry::default().with_prelude(false),
        &Default::default(),
    )
    .unwrap()
}

fn bytecode() -> Rc<interp::BytecodeModule> {
    Rc::new(
        interp::compile(
            &prepared(),
            &HashMap::from([("probe_dispatch".into(), 1)]),
            host_bindings(),
            ModuleId(0),
        )
        .unwrap(),
    )
}

impl Artifact {
    fn new(backend: Backend) -> Self {
        let hosts = [
            ("probe_enter", enter as *const u8),
            ("probe_cancel", checkpoint as *const u8),
            ("probe_parent", parent as *const u8),
            ("probe_dispatch", dispatch as *const u8),
        ];
        match backend {
            Backend::Interpreter => {
                let module = bytecode();
                Self {
                    owner: module.clone(),
                    roots: HashMap::new(),
                    bytecode: Some(module),
                    worker: WorkerCode::Interpreted,
                    _scratch: None,
                }
            }
            Backend::Jit => {
                let module = Rc::new(
                    jit::compile(
                        &prepared(),
                        &hosts,
                        &HashMap::from([("probe_dispatch".into(), 1)]),
                        vec![],
                        ModuleId(0),
                        false,
                    )
                    .unwrap(),
                );
                // SAFETY: Exact generated scalar/CPS signatures, immutable
                // finalized code, static host callbacks; module outlives workers.
                let worker = WorkerCode::Native(unsafe {
                    std::mem::transmute::<*const u8, Work>(module.exports["work"].entry)
                });
                let roots = ["joined", "raced", "branched"]
                    .into_iter()
                    .map(|name| {
                        let function = &module.exports[name];
                        (
                            name.into(),
                            (
                                unsafe { crate::runtime::abi::as_cps_entry(function.entry) },
                                function.info.frame_slots,
                            ),
                        )
                    })
                    .collect();
                Self {
                    owner: module,
                    roots,
                    bytecode: None,
                    worker,
                    _scratch: None,
                }
            }
            Backend::Aot => {
                let scratch = ScratchDir::new("task-parallel-probe");
                let mut compiler = AotCompiler::new("task_parallel_probe").unwrap();
                compiler.set_prelude_enabled(false).unwrap();
                compiler
                    .compile_with_import_layouts(
                        &crate::parser::parse(SOURCE).unwrap(),
                        &HashMap::from([("probe_dispatch".into(), 1)]),
                    )
                    .unwrap();
                let slots: Vec<_> = ["joined", "raced", "branched"]
                    .into_iter()
                    .map(|name| (name, compiler.cps_frame_slots(name).unwrap()))
                    .collect();
                let symbols = crate::runtime::cps::symbols()
                    .into_iter()
                    .chain(crate::runtime::error::symbols())
                    .chain(crate::runtime::arena::symbols())
                    .chain(crate::runtime::buffer::symbols())
                    .chain(crate::runtime::cleanup::symbols())
                    .chain(crate::runtime::float_env::symbols())
                    .chain(hosts)
                    .map(|(name, address)| (name.to_owned(), address))
                    .collect();
                let bridge = scratch.path().join("bridge.o");
                let object = scratch.path().join("probe.o");
                let library = scratch.path().join("probe.so");
                std::fs::write(&bridge, shared_runtime_object(&compiler.module, &symbols)).unwrap();
                compiler.write_object(&object).unwrap();
                let output = Command::new("cc")
                    .arg(if cfg!(target_os = "macos") {
                        "-dynamiclib"
                    } else {
                        "-shared"
                    })
                    .arg(&object)
                    .arg(&bridge)
                    .arg("-o")
                    .arg(&library)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
                // SAFETY: The locally generated object forwards to this runtime;
                // no second runtime or foreign frame layout is loaded.
                let library = Rc::new(unsafe { libloading::Library::new(library).unwrap() });
                let worker = WorkerCode::Native(unsafe { *library.get::<Work>(b"work").unwrap() });
                let roots = slots
                    .into_iter()
                    .map(|(name, slots)| {
                        (
                            name.into(),
                            (
                                unsafe { *library.get::<CpsEntry>(name.as_bytes()).unwrap() },
                                slots,
                            ),
                        )
                    })
                    .collect();
                Self {
                    owner: library,
                    roots,
                    bytecode: None,
                    worker,
                    _scratch: Some(scratch),
                }
            }
        }
    }

    fn root(&self, name: &str) -> Continuation {
        if let Some(module) = &self.bytecode {
            Continuation::new_interpreted(
                interp::prepare_scalar(module.clone(), name, &[], Default::default()).unwrap(),
                ModuleId(0),
                module.clone(),
            )
        } else {
            let (entry, slots) = self.roots[name];
            // SAFETY: Compiler-authored layout and signature; owner pins code
            // through all source children and their worker stop callbacks.
            unsafe { Continuation::from_native(entry, CpsFrame::new(slots), self.owner.clone()) }
        }
    }
}

#[derive(Default)]
struct State {
    entered: [Option<ThreadId>; 2],
    checkpoint_entered: [bool; 2],
    cancelled: [bool; 2],
    released: bool,
    finished: [bool; 2],
    results: [Option<i32>; 2],
    stops: [usize; 2],
    stop_reasons: [Option<u32>; 2],
    parent_calls: usize,
    errors: Vec<String>,
}

struct Root {
    state: Mutex<State>,
    changed: Condvar,
    race: bool,
    deadline: Instant,
    worker: WorkerCode,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Root>>> = const { RefCell::new(None) };
}

impl Root {
    fn new(worker: WorkerCode, race: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            race,
            deadline: Instant::now() + TIMEOUT,
            worker,
        })
    }

    fn wait_for(&self, ready: impl Fn(&State) -> bool) {
        let mut state = self.state.lock().unwrap();
        while !ready(&state) {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let errors = state.errors.clone();
                drop(state); // Keep cancellation cleanup usable during unwind.
                panic!("worker protocol timeout: {errors:?}");
            }
            state = self.changed.wait_timeout(state, remaining).unwrap().0;
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.changed.notify_all();
    }
}

// Non-unwinding scalar ABI callbacks. Timeouts become recorded probe failures,
// allowing generated cleanup and the test's worker joins to finish normally.
extern "C" fn enter(id: i32) -> i32 {
    CURRENT.with(|slot| {
        let root = slot.borrow().as_ref().unwrap().clone();
        let mut state = root.state.lock().unwrap();
        state.entered[id as usize] = Some(thread::current().id());
        if arena::LIVE.with(|live| live.get()) == 0 {
            state
                .errors
                .push("worker entered without its generated allocation".into());
        }
        root.changed.notify_all();
        while !state.released && !state.cancelled[id as usize] {
            let remaining = root.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.errors.push("worker entry timeout".into());
                return 0;
            }
            state = root.changed.wait_timeout(state, remaining).unwrap().0;
        }
        i32::from(!state.cancelled[id as usize])
    })
}

extern "C" fn checkpoint(id: i32) -> i32 {
    CURRENT.with(|slot| {
        let root = slot.borrow().as_ref().unwrap().clone();
        let mut state = root.state.lock().unwrap();
        state.checkpoint_entered[id as usize] = true;
        root.changed.notify_all();
        // Hold the losing generated call at its explicit cancellation boundary.
        // The winner must also wait until the loser reaches this boundary;
        // otherwise cancellation could still catch the loser in enter().
        while root.race
            && !state.cancelled[id as usize]
            && (id == 1 || !state.checkpoint_entered[1])
        {
            let remaining = root.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state
                    .errors
                    .push("checkpoint synchronization timeout".into());
                return 1;
            }
            state = root.changed.wait_timeout(state, remaining).unwrap().0;
        }
        i32::from(state.cancelled[id as usize])
    })
}

extern "C" fn parent() {
    CURRENT.with(|slot| {
        let root = slot.borrow().as_ref().unwrap().clone();
        let mut state = root.state.lock().unwrap();
        if state.finished != [true; 2] || state.stops != [1; 2] {
            state
                .errors
                .push("parent resumed before worker cleanup".into());
        }
        state.parent_calls += 1;
    });
}

struct Ticket {
    root: Arc<Root>,
    id: usize,
    worker: Option<JoinHandle<()>>,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.root.state.lock().unwrap().cancelled[self.id] = true;
        self.root.changed.notify_all();
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            self.root
                .state
                .lock()
                .unwrap()
                .errors
                .push("worker panicked".into());
        }
    }
}

fn run_worker(root: Arc<Root>, id: usize, wake: TaskWaker) {
    CURRENT.with(|slot| slot.replace(Some(root.clone())));
    let result = match root.worker {
        WorkerCode::Native(work) => {
            // SAFETY: Audited fixture takes/returns scalars, allocates only on
            // this worker, and calls static thread-local adapters. Code is pinned
            // by Artifact until Ticket::drop has joined every worker.
            crate::runtime::error::checked(|| unsafe { work(id as i32) })
        }
        WorkerCode::Interpreted => {
            // Rc bytecode is materialized locally; no VM or live interpreter
            // state is transferred. Shared bytecode storage is a TP2 concern.
            let module = bytecode();
            interp::call_sync(
                &module,
                "work",
                &[interp::Value::Int32(id as i32)],
                Default::default(),
            )
            .map(|value| match value {
                interp::Value::Int32(value) => value,
                _ => unreachable!("checked scalar result"),
            })
        }
    };
    let mut state = root.state.lock().unwrap();
    match result {
        Ok(value) => state.results[id] = Some(value),
        Err(error) => state.errors.push(error.to_string()),
    }
    if arena::LIVE.with(|live| live.get()) != 0 {
        state
            .errors
            .push("worker allocation survived generated cleanup".into());
    }
    state.finished[id] = true;
    drop(state);
    CURRENT.with(|slot| slot.take());
    root.changed.notify_all();
    wake.wake();
}

unsafe extern "C" fn dispatch(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.host_context.is_null() {
        let root = CURRENT.with(|slot| slot.borrow().as_ref().unwrap().clone());
        let id = frame.slots[0] as usize;
        let wake = current_waker().expect("root is driven by HostExecutor");
        let worker_root = root.clone();
        let worker = thread::spawn(move || run_worker(worker_root, id, wake));
        frame.host_context = Box::into_raw(Box::new(Ticket {
            root,
            id,
            worker: Some(worker),
        }))
        .cast();
        // Test-only bridge: arm the existing synchronous stop hook explicitly.
        // No frame or slot pointer is captured by the worker.
        frame.host_stop = Some(stop);
        frame.state = 1;
        return 1;
    }
    let ticket = &*frame.host_context.cast::<Ticket>();
    let result = ticket.root.state.lock().unwrap().results[ticket.id];
    match result {
        Some(result) => {
            frame.result = result as u32 as u64;
            frame.stop_host(STOP_COMPLETED);
            0
        }
        None => 1,
    }
}

unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
    let pointer = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if pointer.is_null() {
        return;
    }
    let ticket = Box::from_raw(pointer.cast::<Ticket>());
    let root = ticket.root.clone();
    let id = ticket.id;
    drop(ticket); // Cancel, join, then acknowledge cleanup; never under state lock.
    let mut state = root.state.lock().unwrap();
    state.stops[id] += 1;
    state.stop_reasons[id] = Some(reason);
}

struct Installed;
impl Installed {
    fn new(root: Arc<Root>) -> Self {
        CURRENT.with(|slot| assert!(slot.replace(Some(root)).is_none()));
        Self
    }
}
impl Drop for Installed {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.take());
    }
}

#[test]
fn task_parallel_probe_generated_children_overlap_and_drain_on_all_backends() {
    for backend in [Backend::Jit, Backend::Interpreter, Backend::Aot] {
        let artifact = Artifact::new(backend);
        for (entry, expected) in [("joined", 1536), ("raced", 512), ("branched", 77)] {
            let root = Root::new(artifact.worker, entry == "raced");
            let _installed = Installed::new(root.clone());
            let executor = HostExecutor::default();
            executor
                .spawn(&TaskGroup::new(), artifact.root(entry))
                .unwrap();
            assert_eq!(executor.poll(1).unwrap().polled, 1);
            root.wait_for(|state| state.entered.iter().all(Option::is_some));
            let (entered, finished) = {
                let state = root.state.lock().unwrap();
                (state.entered, state.finished)
            };
            assert_ne!(entered[0], entered[1], "{backend:?} {entry}");
            assert!(entered.iter().all(|id| *id != Some(thread::current().id())));
            assert_eq!(finished, [false; 2]);
            root.release();
            root.wait_for(|state| state.finished[0]);
            let mut completions = Vec::new();
            while executor.task_count() != 0 {
                assert!(Instant::now() < root.deadline, "{backend:?} {entry}");
                completions.extend(executor.poll(1).unwrap().completed);
                thread::yield_now();
            }
            assert_eq!(completions.len(), 1);
            assert_eq!(completions[0].result.as_ref().unwrap(), &(expected as u64));
            let state = root.state.lock().unwrap();
            assert!(
                state.errors.is_empty(),
                "{backend:?} {entry}: {:?}",
                state.errors
            );
            assert_eq!(state.stops, [1; 2]);
            assert_eq!(state.parent_calls, 1);
            if entry == "raced" {
                assert_eq!(state.checkpoint_entered, [true; 2]);
                assert_eq!(state.results, [Some(512), Some(-1)]);
                assert_eq!(
                    state.stop_reasons,
                    [Some(STOP_COMPLETED), Some(STOP_CANCELLED)]
                );
            } else {
                assert_eq!(state.results, [Some(512), Some(1024)]);
            }
        }
        let root = Root::new(artifact.worker, false);
        let _installed = Installed::new(root.clone());
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor.spawn(&group, artifact.root("joined")).unwrap();
        executor.poll(1).unwrap();
        root.wait_for(|state| state.entered.iter().all(Option::is_some));
        assert!(executor.cancel_group(&group));
        let state = root.state.lock().unwrap();
        assert_eq!(state.finished, [true; 2]);
        assert_eq!(state.stops, [1; 2]);
        assert_eq!(state.stop_reasons, [Some(STOP_CANCELLED); 2]);
        assert_eq!(state.parent_calls, 0);
        assert!(state.errors.is_empty(), "{backend:?}: {:?}", state.errors);
        drop(state);
        assert_eq!(executor.task_count(), 0);
        assert!(executor.poll(1).unwrap().completed.is_empty());
    }
}

/// Safe prototype of root-retained immutable allocation segments. Production
/// Arena/Buffer metadata is intentionally not made Send by this experiment.
struct Segment {
    values: Box<[u64]>,
    charge: usize,
    budget: Arc<std::sync::atomic::AtomicUsize>,
}

impl Segment {
    fn reserve(
        budget: &Arc<std::sync::atomic::AtomicUsize>,
        limit: usize,
        value: u64,
    ) -> Option<Arc<Self>> {
        use std::sync::atomic::Ordering;
        const CHARGE: usize = 8 * std::mem::size_of::<u64>() + std::mem::size_of::<Segment>();
        budget
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(CHARGE).filter(|next| *next <= limit)
            })
            .ok()?;
        Some(Arc::new(Self {
            values: vec![value; 8].into_boxed_slice(),
            charge: CHARGE,
            budget: budget.clone(),
        }))
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        self.budget
            .fetch_sub(self.charge, std::sync::atomic::Ordering::AcqRel);
    }
}

#[test]
fn task_parallel_probe_root_retention_and_competing_budget_reservations() {
    use std::sync::{atomic::AtomicUsize, atomic::Ordering, Barrier};
    let used = Arc::new(AtomicUsize::new(0));
    let limit = 8 * std::mem::size_of::<u64>() + std::mem::size_of::<Segment>();
    let root_segments = Mutex::new(Vec::new());
    let barrier = Barrier::new(2);
    thread::scope(|scope| {
        let tasks: Vec<_> = (0..2)
            .map(|id| {
                let used = &used;
                let barrier = &barrier;
                let root_segments = &root_segments;
                scope.spawn(move || {
                    barrier.wait();
                    let Some(segment) = Segment::reserve(used, limit, id) else {
                        return false;
                    };
                    root_segments.lock().unwrap().push(segment.clone());
                    // A branch-local handle can disappear without ending the
                    // root allocation lifetime or refunding its shared budget.
                    drop(segment);
                    true
                })
            })
            .collect();
        let admitted = tasks
            .into_iter()
            .map(|task| usize::from(task.join().unwrap()))
            .sum::<usize>();
        assert_eq!(admitted, 1);
    });
    assert_eq!(used.load(Ordering::Acquire), limit);
    let mut segments = root_segments.lock().unwrap();
    assert_eq!(segments.len(), 1);
    let retained = segments[0].clone();
    let address = retained.values.as_ptr();
    segments.clear();
    assert_eq!(retained.values.as_ptr(), address);
    assert!(retained
        .values
        .iter()
        .all(|value| *value == retained.values[0]));
    assert_eq!(used.load(Ordering::Acquire), limit);
    drop(retained);
    assert_eq!(used.load(Ordering::Acquire), 0);
    assert!(Segment::reserve(&used, limit, 42).is_some());
    assert_eq!(used.load(Ordering::Acquire), 0);
}
