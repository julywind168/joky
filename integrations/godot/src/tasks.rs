//! Main-thread engine waits. Callbacks only mark readiness; language.frame drives Joky.
use crate::{instance::Session, node};
use godot::{
    builtin::*,
    classes::{Node, Object, Timer},
    obj::{Gd, InstanceId},
};
use joky::{
    runtime::{
        borrowed_host::BorrowingHostV1,
        cps::{CpsFrame, DONE, RUNTIME_ERROR},
        error,
        executor::current_waker,
        string,
    },
    vm::{InstanceId as JokyId, InstanceValue as V, VmEngine},
    FunctionSignature, Type,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    panic::{catch_unwind, AssertUnwindSafe},
    rc::{Rc, Weak},
};

pub const SOURCE: &str = include_str!("../stdlib/task.jk");
thread_local! {
    static SESSIONS: RefCell<HashMap<JokyId, Weak<Session>>> = RefCell::new(HashMap::new());
    static RELEASES: RefCell<Vec<JokyId>> = const { RefCell::new(Vec::new()) };
    static DRIVING: Cell<bool> = const { Cell::new(false) };
}
pub fn register_session(id: JokyId, session: &Rc<Session>) {
    SESSIONS.with(|s| s.borrow_mut().insert(id, Rc::downgrade(session)));
}
pub fn unregister_session(id: JokyId) {
    if DRIVING.with(Cell::get) {
        RELEASES.with(|ids| ids.borrow_mut().push(id));
    } else {
        SESSIONS.with(|s| s.borrow_mut().remove(&id));
    }
}
pub fn current_session() -> Option<Rc<Session>> {
    let id = joky::vm::current_instance_id()?;
    SESSIONS.with(|s| s.borrow().get(&id).and_then(Weak::upgrade))
}
fn synchronous_session() -> Option<Rc<Session>> {
    let context = node::context()?;
    SESSIONS.with(|s| {
        s.borrow()
            .values()
            .filter_map(Weak::upgrade)
            .find(|s| s.owner == context.owner && Rc::ptr_eq(&s.program.bridge, &context.bridge))
    })
}
pub fn active_task_count() -> usize {
    SESSIONS.with(|s| {
        let mut programs = HashSet::new();
        s.borrow()
            .values()
            .filter_map(Weak::upgrade)
            .filter(|s| programs.insert(Rc::as_ptr(&s.program)))
            .map(|s| s.program.vm.instance_task_count())
            .sum()
    })
}
pub fn drive() {
    if DRIVING.with(|d| d.replace(true)) {
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            let ids = RELEASES.with(|ids| std::mem::take(&mut *ids.borrow_mut()));
            SESSIONS.with(|sessions| {
                let mut sessions = sessions.borrow_mut();
                for id in ids {
                    sessions.remove(&id);
                }
            });
            DRIVING.with(|d| d.set(false));
        }
    }
    let _reset = Reset;
    let sessions: Vec<_> =
        SESSIONS.with(|s| s.borrow().values().filter_map(Weak::upgrade).collect());
    for session in &sessions {
        session.before_drive();
    }
    let mut programs = HashSet::new();
    for session in &sessions {
        if !programs.insert(Rc::as_ptr(&session.program)) {
            continue;
        }
        if session.program.vm.instance_task_count() == 0 {
            continue;
        }
        match session.program.vm.poll_tasks(64) {
            Ok(report) => {
                for completion in report.completed {
                    for owner in &sessions {
                        owner.completed(completion.id);
                    }
                    if let Err(error) = completion.result {
                        crate::report(session.program.diagnostic(&error));
                    }
                }
            }
            Err(error) => crate::report(session.program.diagnostic(&error)),
        }
    }
    for session in &sessions {
        session.after_drive();
    }
}
fn failure(message: &str) -> joky::Error {
    joky::Error::new(joky::ErrorPhase::Runtime, message)
}
fn checked<T: Default>(f: impl FnOnce() -> Result<T, joky::Error>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            error::record_error(error);
            T::default()
        }
        Err(_) => {
            error::record_error(failure("Godot task callback panicked"));
            T::default()
        }
    }
}
unsafe extern "C" fn start(name: *const u8) {
    checked(|| {
        synchronous_session()
            .ok_or_else(|| failure("No Godot task owner"))?
            .start(string::borrow(name)?, &[])
    });
}
unsafe fn state(name: *const u8, args: &[V]) -> Result<V, joky::Error> {
    joky::vm::call_current_instance(string::borrow(name)?, args)
}
macro_rules! scalar_access {
    ($read:ident, $write:ident, $variant:ident, $ty:ty) => {
        unsafe extern "C" fn $read(name: *const u8) -> $ty {
            checked(|| match state(name, &[])? {
                V::$variant(v) => Ok(v),
                _ => Err(failure("State method result type mismatch")),
            })
        }
        unsafe extern "C" fn $write(name: *const u8, value: $ty) {
            checked(|| match state(name, &[V::$variant(value)])? {
                V::Unit => Ok(()),
                _ => Err(failure("State setter must return Unit")),
            })
        }
    };
}
scalar_access!(read_int32, write_int32, Int32, i32);
scalar_access!(read_float64, write_float64, Float64, f64);
unsafe extern "C" fn call(name: *const u8) {
    checked(|| match state(name, &[])? {
        V::Unit => Ok(()),
        _ => Err(failure("State method must return Unit")),
    });
}
struct Request {
    kind: i32,
    token: i32,
    seconds: f64,
}
#[derive(Clone, Copy, PartialEq)]
enum Status {
    Pending,
    Ready,
    Gone,
}
struct Connection {
    id: InstanceId,
    signal: StringName,
    callable: Callable,
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut object) = Gd::<Object>::try_from_instance_id(self.id) {
            if object.is_connected(&self.signal, &self.callable) {
                object.disconnect(&self.signal, &self.callable);
            }
        }
    }
}
struct Wait {
    status: Rc<Cell<Status>>,
    connections: Vec<Connection>,
    timer: Option<InstanceId>,
}
impl Drop for Wait {
    fn drop(&mut self) {
        self.connections.clear();
        if let Some(id) = self.timer.take() {
            if let Ok(mut timer) = Gd::<Timer>::try_from_instance_id(id) {
                timer.stop();
                if !timer.is_queued_for_deletion() {
                    timer.queue_free();
                }
            }
        }
    }
}
impl Wait {
    fn connect(
        &mut self,
        mut object: Gd<Object>,
        signal: &str,
        owner: Option<InstanceId>,
        gone: bool,
    ) -> Result<(), joky::Error> {
        let status = self.status.clone();
        let waker =
            current_waker().ok_or_else(|| failure("Wait requires an instance task executor"))?;
        let callable = Callable::from_fn("Joky wait", move |_: &[&Variant]| {
            if status.get() != Status::Pending {
                return;
            }
            if let Some(owner) = owner {
                if !Gd::<Node>::try_from_instance_id(owner).is_ok_and(|n| n.can_process()) {
                    return;
                }
            }
            status.set(if gone { Status::Gone } else { Status::Ready });
            waker.wake();
        });
        let signal = StringName::from(signal);
        if object.connect(&signal, &callable) != godot::global::Error::OK {
            return Err(failure("Godot signal connection failed"));
        }
        self.connections.push(Connection {
            id: object.instance_id(),
            signal,
            callable,
        });
        Ok(())
    }
    fn new(request: &Request) -> Result<Self, joky::Error> {
        if !current_session().is_some_and(|s| s.accepts_tasks()) {
            return Err(failure("Task owner no longer accepts waits"));
        }
        let context = node::context().ok_or_else(|| failure("No Godot task context"))?;
        let mut owner = Gd::<Node>::try_from_instance_id(context.owner)
            .map_err(|_| failure("Task owner is deleted"))?;
        if !owner.is_inside_tree() {
            return Err(failure("Task owner is outside the tree"));
        }
        let tree = owner.get_tree();
        let mut wait = Self {
            status: Rc::new(Cell::new(Status::Pending)),
            connections: vec![],
            timer: None,
        };
        match request.kind {
            0 | 1 => wait.connect(
                tree.upcast(),
                if request.kind == 0 {
                    "process_frame"
                } else {
                    "physics_frame"
                },
                Some(context.owner),
                false,
            )?,
            2 | 3 => {
                if !request.seconds.is_finite() || request.seconds < 0.0 {
                    return Err(failure("Wait duration must be finite and nonnegative"));
                }
                let mut timer = <Timer as godot::obj::NewAlloc>::new_alloc();
                wait.timer = Some(timer.instance_id());
                timer.set_name("_JokyWait");
                timer.set_one_shot(true);
                timer.set_wait_time(request.seconds.max(0.000001));
                timer.set_ignore_time_scale(request.kind == 3);
                timer.set_process_mode(if request.kind == 3 {
                    godot::classes::node::ProcessMode::ALWAYS
                } else {
                    godot::classes::node::ProcessMode::PAUSABLE
                });
                owner
                    .add_child_ex(&timer)
                    .internal(godot::classes::node::InternalMode::BACK)
                    .done();
                wait.connect(timer.clone().upcast(), "timeout", None, false)?;
                timer.start();
            }
            4 => {
                let (id, signal) = context
                    .bridge
                    .signals
                    .borrow()
                    .get(&request.token)
                    .cloned()
                    .ok_or_else(|| failure("Signal handle is invalid for this program"))?;
                let target = Gd::<Node>::try_from_instance_id(id)
                    .map_err(|_| failure("Signal emitter is deleted"))?;
                if target.is_queued_for_deletion() {
                    return Err(failure("Signal emitter is being deleted"));
                }
                wait.connect(target.clone().upcast(), &signal, None, false)?;
                if signal != "tree_exiting" {
                    wait.connect(target.upcast(), "tree_exiting", None, true)?;
                }
            }
            _ => return Err(failure("Unknown Godot wait kind")),
        }
        Ok(wait)
    }
}
// Safety: registered scalar slots are Int32, Int32, Float64. The request
// is decoded during startup; no frame or slot address is retained. Only owned subscription data
// survives Pending; stop synchronously disconnects every callback before returning.
unsafe extern "C" fn poll(frame: *mut CpsFrame) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let frame = &mut *frame;
        if frame.host_context.is_null() {
            let request = Request {
                kind: frame.slots[0] as i32,
                token: frame.slots[1] as i32,
                seconds: f64::from_bits(frame.slots[2]),
            };
            frame.host_context = Box::into_raw(Box::new(Wait::new(&request)?)).cast();
        }
        let wait = &*frame.host_context.cast::<Wait>();
        match wait.status.get() {
            Status::Pending => Ok(1),
            Status::Ready => {
                frame.state = DONE;
                frame.result = 0;
                Ok(0)
            }
            Status::Gone => Err(failure("Signal emitter left the tree while waiting")),
        }
    }));
    match result {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            error::record_error(error);
            RUNTIME_ERROR
        }
        Err(_) => {
            error::record_error(failure("Godot wait callback panicked"));
            RUNTIME_ERROR
        }
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _reason: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Wait>()));
    }
}
// Reuse the versioned poll/stop protocol for these scalar operations. This
// arms stop before startup and stops terminal polls too, so partial startup,
// completion, failure and frame cancellation all use the same synchronous path.
unsafe extern "C" fn wait(frame: *mut CpsFrame) -> i32 {
    static OPERATION: BorrowingHostV1 = BorrowingHostV1 {
        abi_version: 1,
        poll: Some(poll),
        stop: Some(stop),
    };
    joky::runtime::borrowed_host::joky_cps_poll_borrowing_v1(frame, &OPERATION)
}
pub fn register(vm: &mut VmEngine) -> Result<(), joky::Error> {
    let text = || Type::reference(Type::String, false);
    // Exact static callback ABIs; no reference escapes synchronous callbacks.
    unsafe {
        vm.register_suspending_fn(
            "godot_task_wait",
            FunctionSignature::new(vec![Type::Int32, Type::Int32, Type::Float64], Type::Unit),
            wait,
        )?;
        for (name, params, ret, entry) in [
            ("start", vec![text()], Type::Unit, start as *const u8),
            ("call", vec![text()], Type::Unit, call as *const u8),
            (
                "read_int32",
                vec![text()],
                Type::Int32,
                read_int32 as *const u8,
            ),
            (
                "write_int32",
                vec![text(), Type::Int32],
                Type::Unit,
                write_int32 as *const u8,
            ),
            (
                "read_float64",
                vec![text()],
                Type::Float64,
                read_float64 as *const u8,
            ),
            (
                "write_float64",
                vec![text(), Type::Float64],
                Type::Unit,
                write_float64 as *const u8,
            ),
        ] {
            vm.register_text_fn(
                &format!("godot_task_{name}"),
                FunctionSignature::new(params, ret),
                entry,
            )?;
        }
    }
    Ok(())
}
