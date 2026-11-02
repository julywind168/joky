//! Bounded Argon2id jobs. Only owned Rust bytes enter background threads.
mod hashing;
use super::{
    arena::{Arena, ExternalCharge},
    borrowed_host::{STOP_COMPLETED, STOP_FAILED},
    buffer::Buffer,
    cps::{CpsFrame, DONE, FAILED},
    executor::{current_waker, TaskWaker},
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, SourceId, Span, Type};
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
};
use zeroize::Zeroizing;

const MAX_PASSWORD: usize = 1024;
const MAX_ENCODED: usize = 512;
const STACK_BYTES: usize = 256 * 1024;
const MAX_ACTIVE: usize = 4;
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
const INVALID_HASH: u64 = 1;
const UNSUPPORTED: u64 = 2;
const PARAMETERS: u64 = 3;
const PASSWORD_TOO_LONG: u64 = 4;
const BUSY: u64 = 5;
const RANDOM_UNAVAILABLE: u64 = 6;

fn failure(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::HostError, message)
}
fn allocation_error() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::ResourceLimit,
        "Password hashing allocation failed",
    )
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}
pub(crate) fn validate(signature: &FunctionSignature, suspends: bool) -> Result<(), Error> {
    let bytes = matches!(&signature.ret, Type::Record(r) if r.name == "joky/array.Array"
        && r.arguments == [Type::UInt8] && r.fields.len() == 1
        && matches!(&r.fields[0].ty, Type::Buffer(t) if **t == Type::UInt8));
    if !suspends
        || !bytes
        || signature.params
            != [
                Type::UInt64,
                Type::String,
                Type::String,
                Type::reference(Type::UInt64, true),
            ]
    {
        return Err(Error::new_with_code(
            ErrorPhase::Abi,
            ErrorCode::AbiError,
            "Password hashing requires its exact compiler-owned signature",
        ));
    }
    Ok(())
}
struct Packet {
    code: u64,
    len: usize,
    bytes: [u8; MAX_ENCODED],
}
impl Packet {
    fn error(code: u64) -> Self {
        Self {
            code,
            len: 0,
            bytes: [0; MAX_ENCODED],
        }
    }
    fn bytes(bytes: &[u8]) -> Self {
        let mut packet = Self::error(0);
        packet.bytes[..bytes.len()].copy_from_slice(bytes);
        packet.len = bytes.len();
        packet
    }
}
impl std::fmt::Write for Packet {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self
            .len
            .checked_add(text.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or(std::fmt::Error)?;
        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}
struct Permit;
impl Permit {
    fn acquire() -> Option<Self> {
        ACTIVE
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_ACTIVE).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
enum Wake {
    Local(TaskWaker),
    Parallel(super::parallel::pool::IoWaker),
}
impl Wake {
    fn current() -> Option<Self> {
        super::parallel::pool::io_waker()
            .map(Self::Parallel)
            .or_else(|| current_waker().map(Self::Local))
    }
    fn wake(&self) {
        match self {
            Self::Local(w) => w.wake(),
            Self::Parallel(w) => w.wake(),
        }
    }
}
struct State {
    cancelled: bool,
    result: Option<Result<Packet, Error>>,
    wake: Option<Wake>,
}
struct Request {
    state: Arc<Mutex<State>>,
    thread: Option<JoinHandle<()>>,
    // Thread-local counters stay on the polling task. Drop joins the worker
    // before refunding this charge, including cancellation and output failure.
    _charge: ExternalCharge,
}
impl Drop for Request {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.state);
            state.cancelled = true;
            state.wake = None;
            state.result = None;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Request {
    fn start(
        arena: &mut Arena,
        password: &[u8],
        plan: hashing::Plan,
        permit: Permit,
    ) -> Result<Self, Error> {
        let bytes = plan.memory_bytes()
            + password.len()
            + STACK_BYTES
            + std::mem::size_of::<Request>()
            + std::mem::size_of::<hashing::Plan>()
            + std::mem::size_of::<Mutex<State>>()
            + 2 * std::mem::size_of::<usize>();
        let charge = arena.reserve_external(bytes)?;
        let mut owned = Zeroizing::new(Vec::new());
        owned
            .try_reserve_exact(password.len())
            .map_err(|_| allocation_error())?;
        owned.extend_from_slice(password);
        let state = Arc::new(Mutex::new(State {
            cancelled: false,
            result: None,
            wake: Wake::current(),
        }));
        let job = state.clone();
        #[cfg(test)]
        let before_compute = tests::before_compute();
        let thread = thread::Builder::new()
            .name("joky-password".into())
            .stack_size(STACK_BYTES)
            .spawn(move || {
                let _permit = permit;
                let result = catch_unwind(AssertUnwindSafe(|| {
                    if lock(&job).cancelled {
                        return Ok(Packet::error(0));
                    }
                    #[cfg(test)]
                    if let Some(hook) = before_compute {
                        hook();
                    }
                    plan.compute(&owned)
                }))
                .unwrap_or_else(|_| Err(failure("Password hashing worker failed")));
                // Workspaces and the private password copy are erased before
                // publishing completion. Stop revokes publication under this lock.
                drop(owned);
                let mut state = lock(&job);
                if !state.cancelled {
                    state.result = Some(result);
                    if let Some(wake) = &state.wake {
                        wake.wake();
                    }
                }
            })
            .map_err(|_| failure("Could not start password hashing worker"))?;
        Ok(Self {
            state,
            thread: Some(thread),
            _charge: charge,
        })
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Request>()));
    }
}
unsafe fn output(
    arena: &mut Arena,
    frame: &CpsFrame,
    packet: &Packet,
) -> Result<NonNull<Buffer>, Error> {
    let bytes = Buffer::copy_bytes(arena, &packet.bytes[..packet.len])?;
    *(frame.slots[3] as usize as *mut u64) = packet.code;
    Ok(bytes)
}
/// # Safety
/// The checked ABI supplies operation, two live owned Strings and an exclusive
/// UInt64 status slot. Workers receive copies only; output is written by this task.
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
) -> Result<Option<NonNull<Buffer>>, Error> {
    if matches!(frame.state, DONE | FAILED) {
        return Err(failure("Password operation has terminated"));
    }
    let result = (|| {
        if frame.host_context.is_null() {
            let operation = frame.slots[0];
            if operation > 1 {
                return Err(failure("Invalid password operation"));
            }
            let password = super::owned_text::borrow(frame.slots[1] as usize as *const Buffer)?;
            let encoded = super::owned_text::borrow(frame.slots[2] as usize as *const Buffer)?;
            if password.len() > MAX_PASSWORD {
                return output(arena, frame, &Packet::error(PASSWORD_TOO_LONG)).map(Some);
            }
            let plan = if operation == 0 {
                hashing::Plan::generate(getrandom::fill)
            } else {
                hashing::Plan::parse(encoded.as_bytes())
            };
            let plan = match plan {
                Ok(plan) => plan,
                Err(code) => return output(arena, frame, &Packet::error(code)).map(Some),
            };
            let Some(permit) = Permit::acquire() else {
                return output(arena, frame, &Packet::error(BUSY)).map(Some);
            };
            let request = Request::start(arena, password.as_bytes(), plan, permit)?;
            frame.host_context = Box::into_raw(Box::new(request)).cast();
            frame.host_stop = Some(stop);
        }
        let request = &*frame.host_context.cast::<Request>();
        let mut state = lock(&request.state);
        if let Some(wake) = Wake::current() {
            state.wake = Some(wake);
        }
        let Some(result) = state.result.take() else {
            return Ok(None);
        };
        output(arena, frame, &result?).map(Some)
    })();
    match &result {
        Ok(None) => {}
        Ok(Some(_)) => {
            frame.stop_host(STOP_COMPLETED);
            frame.state = DONE;
        }
        Err(_) => {
            frame.stop_host(STOP_FAILED);
            frame.state = FAILED;
        }
    }
    result
}
/// # Safety
/// Frame obeys the checked password ABI and is live on its serialized task thread.
#[export_name = "__joky_password_poll"]
pub(crate) unsafe extern "C" fn poll_native(frame: *mut CpsFrame) -> i32 {
    match poll(&mut *super::cps::arena(frame), &mut *frame) {
        Ok(Some(owner)) => {
            (&mut (*frame).slots)[4] = owner.as_ptr() as usize as u64;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}
pub(crate) fn needs_rehash(encoded: &[u8]) -> u64 {
    match hashing::Plan::parse(encoded) {
        Ok(plan) => u64::from(plan.needs_rehash()),
        Err(code) => code + 2,
    }
}
/// # Safety
/// Input is a live shared initialized byte view. No reference escapes this call.
#[export_name = "__joky_buffer_password_needs_rehash"]
pub(crate) unsafe extern "C" fn needs_rehash_native(
    input: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> u64 {
    match (*input).bytes() {
        Ok(bytes) => needs_rehash(bytes),
        Err(error) => {
            super::error::record_error(
                error.with_span(Span::new(start, end).with_source(SourceId::from_raw(source))),
            );
            0
        }
    }
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());
#[cfg(test)]
pub(crate) mod tests;
