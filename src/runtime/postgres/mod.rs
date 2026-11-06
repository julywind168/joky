//! SQLx owns PostgreSQL sockets. Tokio tasks own copies of every input and
//! publish owned results. All access to Joky memory stays on the polling thread.
mod db;
use super::{
    arena::Arena, buffer::Buffer, cps::CpsFrame, resource::OwnedResource, shutdown::Wake,
    tcp::OwnerCharge,
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use db::{Fault, Loan, Target};
use std::future::Future;
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
};
const MAX: usize = 1024 * 1024;
fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::InvalidCall,
        "Invalid PostgreSQL host call",
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Operation {
    Open,
    Share,
    Begin,
    Run,
    Finish,
    Status,
    Count,
    Data,
    Connect,
}
const NAMES: &[(&str, Operation)] = &[
    ("__pg_open", Operation::Open),
    ("__pg_share", Operation::Share),
    ("__pg_begin", Operation::Begin),
    ("__pg_run", Operation::Run),
    ("__pg_finish", Operation::Finish),
    ("__pg_status", Operation::Status),
    ("__pg_count", Operation::Count),
    ("__pg_data", Operation::Data),
    ("__pg_connect", Operation::Connect),
];
impl Operation {
    pub(crate) fn lookup(name: &str) -> Option<Self> {
        NAMES.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }
    pub(crate) fn returns_pointer(self) -> bool {
        !matches!(self, Self::Status | Self::Count)
    }
    pub(crate) fn returns_bytes(self) -> bool {
        self == Self::Data
    }
    pub(crate) fn validate(self, s: &FunctionSignature, suspends: bool) -> Result<(), Error> {
        let owner = Type::Resource("postgres.Owner".into());
        let r = Type::reference(owner.clone(), false);
        let u = Type::UInt64;
        let bytes = |ty: &Type| matches!(ty,Type::Record(t) if t.name=="joky/array.Array" && t.arguments==[Type::UInt8] && t.fields.len()==1 && matches!(&t.fields[0].ty,Type::Buffer(b) if **b==Type::UInt8));
        let (p, t) = match self {
            Self::Open => (vec![u], owner),
            Self::Connect => (vec![Type::String, u.clone(), u], owner),
            Self::Share | Self::Begin => (vec![r], owner),
            Self::Finish => (vec![r, u], owner),
            Self::Count | Self::Status => (vec![r], u),
            Self::Data if bytes(&s.ret) => (vec![r], s.ret.clone()),
            Self::Run if s.params.get(3).is_some_and(bytes) => {
                (vec![r, u, Type::String, s.params[3].clone()], owner)
            }
            _ => return Err(invalid()),
        };
        if suspends && *s == FunctionSignature::new(p, t) {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}
static DRIVERS: AtomicUsize = AtomicUsize::new(0);
struct Driver {
    pending: Arc<tokio::sync::Semaphore>,
    handle: tokio::runtime::Handle,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Driver {
    fn start() -> Result<Arc<Self>, Fault> {
        if DRIVERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 8).then_some(n + 1)
            })
            .is_err()
        {
            return Err(Fault::LimitExceeded);
        }
        let result = (|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(2)
                .build()
                .map_err(|_| Fault::Unavailable)?;
            let handle = rt.handle().clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let thread = thread::Builder::new()
                .name("joky-postgres".into())
                .spawn(move || {
                    let _ = rt.block_on(rx);
                })
                .map_err(|_| Fault::Unavailable)?;
            Ok(Arc::new(Self {
                pending: Arc::new(tokio::sync::Semaphore::new(64)),
                handle,
                shutdown: Some(tx),
                thread: Some(thread),
            }))
        })();
        if result.is_err() {
            DRIVERS.fetch_sub(1, Ordering::AcqRel);
        }
        result
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        drop(self.shutdown.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        DRIVERS.fetch_sub(1, Ordering::AcqRel);
    }
}
enum Value {
    Pool(sqlx::PgPool),
    Tx(Mutex<Option<Loan>>),
    Bytes(Vec<u8>),
    Count(u64),
    Fault(Fault),
}
struct Answer {
    value: Value,
    restore: Option<Loan>,
}
impl Answer {
    fn new(value: Value) -> Self {
        Self {
            value,
            restore: None,
        }
    }
}
struct Owner {
    value: Value,
    driver: Option<Arc<Driver>>,
    charge: OwnerCharge,
}
unsafe extern "C" fn release(raw: *mut c_void) {
    drop(Box::from_raw(raw.cast::<Owner>()));
}
pub(crate) unsafe fn transferable(
    token: NonNull<c_void>,
    callback: super::resource::ResourceReleaseV1,
) -> bool {
    std::ptr::fn_addr_eq(callback, release as super::resource::ResourceReleaseV1)
        && token.cast::<Owner>().as_ref().charge.shared()
}
fn adopt(arena: &mut Arena, value: Value, driver: Option<Arc<Driver>>) -> Result<u64, Error> {
    let bytes = match &value {
        Value::Pool(_) | Value::Tx(_) => 3 * MAX,
        Value::Bytes(b) => b.capacity(),
        _ => 128,
    };
    let charge = OwnerCharge::new(arena, bytes + 256)?;
    let raw = NonNull::from(Box::leak(Box::new(Owner {
        value,
        driver,
        charge,
    })))
    .cast();
    let resource = OwnedResource::from_token(raw, release)
        .inspect_err(|_| unsafe { release(raw.as_ptr()) })?;
    Ok(Buffer::external(arena, resource)?.as_ptr() as u64)
}
unsafe fn owner<'a>(bits: u64) -> Result<&'a Owner, Error> {
    let buf = (bits as *const Buffer).as_ref().ok_or_else(invalid)?;
    Ok(&*buf.resource_ref()?.control_token(release)?.cast::<Owner>())
}
struct State {
    done: bool,
    result: Option<Result<Answer, Fault>>,
    wake: Option<Wake>,
}
struct Request {
    state: Arc<(Mutex<State>, Condvar)>,
    abort: tokio::task::AbortHandle,
    driver: Arc<Driver>,
    _charge: OwnerCharge,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Request {
    fn start(
        driver: Arc<Driver>,
        charge: OwnerCharge,
        permit: tokio::sync::OwnedSemaphorePermit,
        job: impl Future<Output = Result<Answer, Fault>> + Send + 'static,
    ) -> Self {
        let state = Arc::new((
            Mutex::new(State {
                done: false,
                result: None,
                wake: None,
            }),
            Condvar::new(),
        ));
        let task = driver.handle.spawn(job);
        let abort = task.abort_handle();
        let publish = state.clone();
        driver.handle.spawn(async move {
            let result = task.await.unwrap_or(Err(Fault::Unknown));
            let mut s = publish.0.lock().unwrap();
            s.done = true;
            s.result = Some(result);
            let wake = s.wake.take();
            publish.1.notify_all();
            drop(s);
            if let Some(w) = wake {
                w.wake();
            }
        });
        Self {
            state,
            abort,
            driver,
            _charge: charge,
            _permit: permit,
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.abort.abort();
        let mut s = self.state.0.lock().unwrap();
        s.wake = None;
        // Wait only for the cancelled future to drop its inputs and connection guard.
        while !s.done {
            s = self.state.1.wait(s).unwrap();
        }
        drop(s.result.take());
    }
}
unsafe extern "C" fn stop(f: *mut CpsFrame, _: u32) {
    let raw = std::mem::replace(&mut (*f).host_context, std::ptr::null_mut());
    if !raw.is_null() {
        drop(Box::from_raw(raw.cast::<Request>()));
    }
}
unsafe fn step(arena: &mut Arena, f: &mut CpsFrame, op: Operation) -> Result<Option<u64>, Error> {
    let s = &f.slots;
    let fail = |arena: &mut Arena, e| adopt(arena, Value::Fault(e), None).map(Some);
    if f.host_context.is_null() {
        // Reserve the old opcode: its first slot is a scalar, not a URL pointer.
        if op == Operation::Open {
            return fail(arena, Fault::InvalidRequest);
        }
        let request = if op == Operation::Connect {
            let timeout = std::time::Duration::from_nanos(s[2]);
            if !(1..=32).contains(&s[1])
                || timeout.is_zero()
                || std::time::Instant::now().checked_add(timeout).is_none()
            {
                return fail(arena, Fault::InvalidRequest);
            }
            let url = super::owned_text::borrow(s[0] as *const Buffer)?;
            if url.len() > 8192
                || !(url.starts_with("postgresql://") || url.starts_with("postgres://"))
            {
                return fail(arena, Fault::InvalidRequest);
            }
            let charge = OwnerCharge::new(arena, 3 * MAX)?;
            let driver = match Driver::start() {
                Ok(d) => d,
                Err(e) => return fail(arena, e),
            };
            let permit = driver
                .pending
                .clone()
                .try_acquire_owned()
                .expect("new service");
            Request::start(
                driver,
                charge,
                permit,
                db::open(url.to_owned(), s[1] as u32, timeout),
            )
        } else {
            let o = owner(s[0])?;
            match op {
                Operation::Status => {
                    return Ok(Some(if let Value::Fault(e) = &o.value {
                        e.code()
                    } else {
                        0
                    }))
                }
                Operation::Count => {
                    return if let Value::Count(n) = o.value {
                        Ok(Some(n))
                    } else {
                        Err(invalid())
                    }
                }
                Operation::Data => {
                    let b = match &o.value {
                        Value::Bytes(b) => b.as_slice(),
                        Value::Fault(Fault::Database(code) | Fault::Connect(code)) => {
                            code.as_bytes()
                        }
                        _ => return Err(invalid()),
                    };
                    return Ok(Some(Buffer::copy_bytes(arena, b)?.as_ptr() as u64));
                }
                Operation::Share => {
                    return if let Value::Pool(p) = &o.value {
                        adopt(arena, Value::Pool(p.clone()), o.driver.clone()).map(Some)
                    } else {
                        Err(invalid())
                    }
                }
                _ => {}
            }
            let driver = o.driver.as_ref().ok_or_else(invalid)?.clone();
            let charge = OwnerCharge::new(arena, 3 * MAX)?;
            let Ok(permit) = driver.pending.clone().try_acquire_owned() else {
                return fail(arena, Fault::LimitExceeded);
            };
            let (sql, params) = if op == Operation::Run {
                let sql = super::owned_text::borrow(s[2] as *const Buffer)?;
                let params = (&*(s[3] as *const Buffer)).bytes()?;
                if sql.len() > 65536 || params.len() > MAX {
                    return fail(arena, Fault::LimitExceeded);
                }
                (sql.to_owned(), params.to_vec())
            } else {
                (String::new(), Vec::new())
            };
            let target = match &o.value {
                Value::Pool(p) => Target::Pool(p.clone()),
                Value::Tx(tx) => match tx.lock().unwrap().take() {
                    Some(l) => Target::Tx(l),
                    None => return fail(arena, Fault::TransactionClosed),
                },
                _ => return Err(invalid()),
            };
            match (op, target) {
                (Operation::Begin, t @ Target::Pool(_)) => {
                    Request::start(driver, charge, permit, db::begin(t))
                }
                (Operation::Finish, Target::Tx(l)) => {
                    Request::start(driver, charge, permit, db::finish(l, s[1] != 0))
                }
                (Operation::Run, t) => {
                    let mode = match s[1] {
                        0 => db::Mode::Execute,
                        // Previously compiled callers use the positional row packet (1/2).
                        1..=4 => db::Mode::Query {
                            optional: matches!(s[1], 2 | 4),
                            named: s[1] >= 3,
                        },
                        _ => return Err(invalid()),
                    };
                    Request::start(driver, charge, permit, db::run(t, sql, params, mode))
                }
                _ => return Err(invalid()),
            }
        };
        f.host_context = Box::into_raw(Box::new(request)).cast();
        f.host_stop = Some(stop);
    }
    let req = &*f.host_context.cast::<Request>();
    let mut state = req.state.0.lock().unwrap();
    let Some(answer) = state.result.take() else {
        state.wake = Wake::current();
        if super::parallel::pool::active() {
            super::parallel::pool::wait_for_wake();
        }
        return Ok(None);
    };
    drop(state);
    let Answer { value, restore } = answer.unwrap_or_else(|e| Answer::new(Value::Fault(e)));
    let driver = matches!(&value, Value::Tx(_) | Value::Pool(_)).then(|| req.driver.clone());
    let result = adopt(arena, value, driver)?;
    if let Some(l) = restore {
        let Value::Tx(slot) = &owner(s[0])?.value else {
            return Err(invalid());
        };
        *slot.lock().unwrap() = Some(l);
    }
    Ok(Some(result))
}
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    f: &mut CpsFrame,
    op: Operation,
) -> Result<Option<u64>, Error> {
    let result = step(arena, f, op);
    match &result {
        Ok(Some(_)) => {
            f.stop_host(super::borrowed_host::STOP_COMPLETED);
            f.state = super::cps::DONE
        }
        Err(_) => {
            f.stop_host(super::borrowed_host::STOP_FAILED);
            f.state = super::cps::FAILED
        }
        _ => {}
    }
    result
}
#[export_name = "__joky_postgres_poll"]
pub(crate) unsafe extern "C" fn poll_native(frame: *mut CpsFrame, kind: u32) -> i32 {
    let result = (|| {
        let op = NAMES
            .iter()
            .find(|(_, op)| *op as u32 == kind)
            .map(|(_, op)| *op)
            .ok_or_else(invalid)?;
        let arena = super::cps::arena(frame);
        let f = &mut *frame;
        if let Some(bits) = poll(&mut *arena, f, op)? {
            if op.returns_bytes() {
                f.slots[1] = bits
            } else {
                f.result = bits
            }
            Ok(0)
        } else {
            Ok(1)
        }
    })();
    result.unwrap_or_else(|e| {
        super::error::record_error(e);
        super::cps::RUNTIME_ERROR
    })
}
