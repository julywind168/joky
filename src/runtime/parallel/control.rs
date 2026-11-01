//! Root-bound Local command driver. Only copied values and proxy identities cross threads.
use super::{pool, Guard, Lease, Storage};
use crate::{
    runtime::{
        arena::Arena,
        buffer::Buffer,
        cps::{self, CpsFrame},
        driver::WaitPlan,
        executor::{BlockingContext, TaskWaker},
        resource::{OwnedResource, ResourceReleaseV1},
    },
    Error, ErrorCode, ErrorPhase, Type,
};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, Weak,
    },
    thread::JoinHandle,
};
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}
const LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    VmCreate,
    VmClose,
    VmCall,
    VmSource,
    VmMigration(crate::core::vm::MigrationOperation),
    Domain(crate::runtime::domain::Operation),
    Fs(crate::runtime::fs::Operation),
}
#[derive(Clone, Copy)]
enum Kind {
    Word,
    Bytes(usize, bool),
    Resource,
}
impl Operation {
    pub(crate) fn domain(op: crate::runtime::domain::Operation) -> Option<Self> {
        use crate::runtime::domain::Operation::*;
        matches!(
            op,
            DirectoryCreate
                | DirectoryHandle
                | DirectoryClose
                | AdmissionCreate
                | AdmissionHandle
                | AdmissionClose
                | AdmissionTry
                | AdmissionAcquire
                | AdmissionStatus
                | OutboxCreate
                | OutboxCreateDisconnect
                | OutboxHandle
                | OutboxClose
                | OutboxReceive
                | OutboxReceiveBytes
                | OutboxMessageClosed
                | OutboxMessageBytes
        )
        .then_some(Self::Domain(op))
    }
    fn layout(self) -> (Vec<Kind>, Kind) {
        use Kind::*;
        match self {
            Self::VmCreate => (vec![Word], Resource),
            Self::VmClose => (vec![Resource], Word),
            Self::VmMigration(op) => match op {
                crate::core::vm::MigrationOperation::Open => (vec![Kind::Resource], Kind::Resource),
                crate::core::vm::MigrationOperation::Diagnostic => {
                    (vec![Kind::Resource], Kind::Bytes(1, false))
                }
                crate::core::vm::MigrationOperation::Command => (
                    vec![
                        Kind::Resource,
                        Kind::Word,
                        Kind::Word,
                        Kind::Bytes(1, false),
                        Kind::Bytes(1, false),
                        Kind::Bytes(1, false),
                        Kind::Bytes(8, true),
                    ],
                    Kind::Word,
                ),
            },
            Self::VmCall => (
                vec![
                    Resource,
                    Bytes(1, false),
                    Bytes(1, false),
                    Bytes(1, false),
                    Bytes(8, true),
                    Word,
                ],
                Word,
            ),
            Self::VmSource => (
                vec![
                    Resource,
                    Bytes(1, false),
                    Bytes(1, false),
                    Word,
                    Bytes(8, true),
                ],
                Bytes(1, false),
            ),
            Self::Domain(crate::runtime::domain::Operation::OutboxReceiveBytes) => {
                (vec![Resource], Bytes(1, false))
            }
            Self::Domain(op) => {
                fn kind(ty: &Type) -> Kind {
                    match ty {
                        Type::Reference(r) => kind(&r.target),
                        Type::Resource(_) => Resource,
                        Type::Buffer(_) => Bytes(1, false),
                        _ => Word,
                    }
                }
                let sig = op.signature();
                (sig.params.iter().map(kind).collect(), kind(&sig.ret))
            }
            Self::Fs(op) => {
                use crate::runtime::fs::Operation::*;
                match op {
                    Open => (vec![Bytes(1, false), Word], Resource),
                    Process => (vec![Word, Word, Bytes(1, false)], Resource),
                    TestInput => (vec![Word, Bytes(1, false), Bytes(1, false)], Resource),
                    Stdin => (vec![], Resource),
                    Read => (vec![Resource, Word], Resource),
                    Write => (vec![Resource, Bytes(1, false)], Resource),
                    Seek => (vec![Resource, Word, Word], Resource),
                    Sync | Close | TakeFile => (vec![Resource], Resource),
                    Code | Count => (vec![Resource], Word),
                    Message | TakeBytes => (vec![Resource], Bytes(1, false)),
                }
            }
        }
    }
}
pub(crate) fn resource_type(name: &str) -> bool {
    matches!(
        name,
        "vm.Owner"
            | "vm.MigrationOwner"
            | "domain.DirectoryOwner"
            | "admission.PoolOwner"
            | "admission.PermitOwner"
            | "outbox.QueueOwner"
            | "outbox.MessageOwner"
            | "fs.FileOwner"
            | "fs.OutcomeOwner"
    )
}
enum Value {
    Word(u64),
    Bytes(Vec<u8>, usize),
    // Reply-only: an empty message is distinct from a closed mailbox.
    Received(Vec<u8>, bool),
    Resource(Arc<Proxy>),
}
struct Reply {
    value: Value,
    updates: Vec<(usize, Vec<u8>)>,
    _charge: Lease,
}
struct RequestState {
    input: Option<Vec<Value>>,
    result: Option<Result<Reply, Error>>,
    cancelled: bool,
    completed: bool,
}
struct Request {
    op: Operation,
    core: Weak<Core>,
    _receipt: Option<pool::ControlReceipt>,
    _sequence: u64,
    state: Mutex<RequestState>,
    wake: Option<pool::IoWaker>,
    done: std::sync::Condvar,
    _charge: Lease,
}
impl Drop for Request {
    fn drop(&mut self) {
        if let Some(core) = self.core.upgrade() {
            core.outstanding.fetch_sub(1, Ordering::Release);
        }
    }
}
struct Queue {
    requests: VecDeque<Arc<Request>>,
    releases: Vec<u64>,
    closing: bool,
}
pub(crate) struct Core {
    root: Storage,
    queue: Mutex<Queue>,
    wake: OnceLock<TaskWaker>,
    closed: AtomicBool,
    outstanding: AtomicUsize,
    #[cfg(test)]
    active_calls: AtomicUsize,
    next_request: AtomicU64,
    finish: Mutex<Option<pool::IoWaker>>,
    _charge: Lease,
}
pub(crate) struct Driver {
    core: Weak<Core>,
    thread: JoinHandle<()>,
}
impl Driver {
    #[cfg(test)]
    pub(crate) fn observation(&self) -> (std::thread::ThreadId, usize) {
        (
            self.thread.thread().id(),
            self.core
                .upgrade()
                .map_or(0, |c| c.active_calls.load(Ordering::Acquire)),
        )
    }
    pub(crate) fn matches(&self, root: &Storage) -> Option<Arc<Core>> {
        self.core.upgrade().filter(|c| c.root.same_root(root))
    }
    pub(crate) fn close(&self) {
        if let Some(c) = self.core.upgrade() {
            c.close();
        }
    }
    pub(crate) fn finished(&self) -> bool {
        self.thread.is_finished()
    }
    pub(crate) fn join(self) {
        self.close();
        self.thread.join().expect("Local control driver panicked");
    }
    pub(crate) fn start(
        root: Storage,
        files: Option<crate::runtime::fs::FileService>,
        spawner: pool::DomainSpawner,
    ) -> Result<(Self, Arc<Core>), Error> {
        // Charge bounded queue/driver/table capacity, including drain scratch and hash slack.
        let charge = root.accounts().reserve(
            std::mem::size_of::<Core>()
                + LIMIT
                    * (2 * std::mem::size_of::<Call>()
                        + 4 * std::mem::size_of::<(u64, LocalResource)>()
                        + 64),
        )?;
        let core = Arc::new(Core {
            root,
            queue: Mutex::new(Queue {
                requests: VecDeque::new(),
                releases: Vec::new(),
                closing: false,
            }),
            wake: OnceLock::new(),
            closed: AtomicBool::new(false),
            outstanding: AtomicUsize::new(0),
            #[cfg(test)]
            active_calls: AtomicUsize::new(0),
            next_request: AtomicU64::new(1),
            finish: Mutex::new(None),
            _charge: charge,
        });
        let driver = core.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("joky-control".into())
            .spawn(move || {
                let _storage = Guard::enter(&driver.root);
                let _files = crate::runtime::fs::enter(files);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match BlockingContext::new() {
                        Ok(context) => {
                            driver.wake.set(context.waker()).ok();
                            let _ = tx.send(Ok(()));
                            let _domains = crate::vm::state_domains::parallel::Scope::enter(
                                spawner,
                                context.waker(),
                            );
                            run(&driver, &context);
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                        }
                    }
                }));
                driver.close();
                let pending = { lock(&driver.queue).requests.drain(..).collect::<Vec<_>>() };
                for request in pending {
                    complete(
                        &request,
                        Err(invalid(if outcome.is_err() {
                            "Local control driver panicked"
                        } else {
                            "Local control driver closed"
                        })),
                    );
                }
                driver.closed.store(true, Ordering::Release);
                if let Some(wake) = lock(&driver.finish).take() {
                    wake.wake();
                }
            })
            .map_err(|e| invalid(&format!("Cannot start Local control driver: {e}")))?;
        if let Err(error) = rx
            .recv()
            .map_err(|_| invalid("Local control startup failed"))
            .and_then(|v| v)
        {
            let _ = thread.join();
            return Err(error);
        }
        Ok((
            Self {
                core: Arc::downgrade(&core),
                thread,
            },
            core,
        ))
    }
}
impl Core {
    fn signal(&self) {
        if let Some(wake) = self.wake.get() {
            wake.wake();
        }
    }
    pub(crate) fn close(&self) {
        lock(&self.queue).closing = true;
        self.signal();
    }
    pub(crate) fn finish(&self, wake: pool::IoWaker) -> bool {
        *lock(&self.finish) = Some(wake);
        self.close();
        self.closed.load(Ordering::Acquire)
    }
    fn submit(
        self: &Arc<Self>,
        op: Operation,
        input: Vec<Value>,
        charge: Lease,
    ) -> Result<Arc<Request>, Error> {
        let mut q = lock(&self.queue);
        if q.closing || self.outstanding.load(Ordering::Acquire) >= LIMIT {
            return Err(invalid("Local control admission is closed or full"));
        }
        let sequence = self
            .next_request
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| invalid("Control request identities exhausted"))?;
        self.outstanding.fetch_add(1, Ordering::Relaxed);
        let request = Arc::new(Request {
            op,
            core: Arc::downgrade(self),
            _receipt: pool::control_receipt(),
            _sequence: sequence,
            state: Mutex::new(RequestState {
                input: Some(input),
                result: None,
                cancelled: false,
                completed: false,
            }),
            wake: pool::io_waker(),
            done: std::sync::Condvar::new(),
            _charge: charge,
        });
        q.requests.push_back(request.clone());
        drop(q);
        self.signal();
        Ok(request)
    }
}
mod exclusive;
pub(super) use exclusive::ExclusiveQueue;

struct Proxy {
    id: u64,
    shared_vm: bool,
    mailbox: bool,
    exclusive: AtomicBool,
    loan: Option<Arc<exclusive::Return>>,
    core: Arc<Core>,
    _charge: Lease,
}
/// Send-safe identity only; the resource and its arena stay on the control thread.
pub(super) struct SharedVm(Arc<Proxy>);
impl SharedVm {
    pub(super) unsafe fn capture(bits: u64) -> Result<Self, Error> {
        let buffer = (bits as *const Buffer)
            .as_ref()
            .ok_or_else(|| invalid("Null shared VM"))?;
        if !buffer.scoped_resource() {
            return Err(invalid("Shared VM belongs to another task"));
        }
        let value = proxy(bits, &pool::control()?)?;
        if !value.shared_vm || value.core.closed.load(Ordering::Acquire) {
            return Err(invalid("Resource is not a live shared VM proxy"));
        }
        Ok(Self(value))
    }
    pub(super) fn install(self) -> Result<OwnedResource, Error> {
        proxy_resource(self.0)
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        // A borrowed proxy returns permission; only the original owns close.
        if self.loan.is_some() {
            return;
        }
        let mut q = lock(&self.core.queue);
        if !q.closing {
            q.releases.push(self.id);
        }
        drop(q);
        self.core.signal();
    }
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Arc<Proxy>>()));
}
pub(crate) unsafe fn transferable(token: NonNull<c_void>, callback: ResourceReleaseV1) -> bool {
    if !std::ptr::fn_addr_eq(callback, release as ResourceReleaseV1) {
        return false;
    }
    let proxy = token.cast::<Arc<Proxy>>().as_ref();
    proxy.loan.is_none()
        && !proxy.exclusive.load(Ordering::Acquire)
        && !proxy.core.closed.load(Ordering::Acquire)
}
unsafe fn proxy(bits: u64, core: &Arc<Core>) -> Result<Arc<Proxy>, Error> {
    let resource = (bits as *const Buffer)
        .as_ref()
        .ok_or_else(|| invalid("Null control proxy"))?
        .resource_ref()?;
    let token = resource.control_token(release)?;
    let proxy = &*token.cast::<Arc<Proxy>>();
    if !Arc::ptr_eq(core, &proxy.core) {
        return Err(invalid("Control proxy belongs to another task tree"));
    }
    if proxy.exclusive.load(Ordering::Acquire) {
        return Err(invalid("Control resource is exclusively borrowed"));
    }
    Ok(proxy.clone())
}
fn adopt(arena: &mut Arena, proxy: Arc<Proxy>) -> Result<u64, Error> {
    Ok(Buffer::external(arena, proxy_resource(proxy)?)?.as_ptr() as u64)
}
fn proxy_resource(proxy: Arc<Proxy>) -> Result<OwnedResource, Error> {
    let token = NonNull::new(Box::into_raw(Box::new(proxy)).cast()).unwrap();
    OwnedResource::from_token(token, release).inspect_err(|_| unsafe { release(token.as_ptr()) })
}
unsafe fn byte_len(bits: u64, stride: usize) -> Result<usize, Error> {
    let buffer = (bits as *const Buffer)
        .as_ref()
        .ok_or_else(|| invalid("Null control buffer"))?;
    usize::try_from(buffer.len())
        .ok()
        .and_then(|n| n.checked_mul(stride))
        .ok_or_else(|| invalid("Control buffer overflow"))
}
fn add_bytes(total: &mut usize, count: usize) -> Result<(), Error> {
    *total = total
        .checked_add(count)
        .ok_or_else(|| invalid("Control packet overflow"))?;
    Ok(())
}
unsafe fn copy_buffer(bits: u64, stride: usize) -> Result<Vec<u8>, Error> {
    let buffer = (bits as *const Buffer)
        .as_ref()
        .ok_or_else(|| invalid("Null control buffer"))?;
    if stride == 1 {
        return Ok(buffer.bytes()?.to_vec());
    }
    let mut bytes = Vec::with_capacity(
        (buffer.len() as usize)
            .checked_mul(stride)
            .ok_or_else(|| invalid("Control buffer overflow"))?,
    );
    for i in 0..buffer.len() {
        bytes.extend_from_slice(std::slice::from_raw_parts(buffer.load(i)?, stride));
    }
    Ok(bytes)
}
unsafe fn buffer(arena: &mut Arena, bytes: &[u8], stride: usize) -> Result<u64, Error> {
    if stride == 1 {
        return Ok(Buffer::copy_bytes(arena, bytes)?.as_ptr() as u64);
    }
    let mut b = Buffer::allocate(arena, (bytes.len() / stride) as u64, stride, stride)?;
    for item in bytes.chunks_exact(stride) {
        b.as_ref()
            .initialize_next()?
            .copy_from_nonoverlapping(item.as_ptr(), stride);
        b.as_mut().commit_next();
    }
    Ok(b.as_ptr() as u64)
}
struct Pending {
    core: Arc<Core>,
    request: Arc<Request>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        lock(&self.request.state).cancelled = true;
        self.core.signal();
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Pending>()));
    }
    (*frame).host_stop = None;
}
/// The compiler fixes the operation and complete input layout. Captures are copied;
/// no worker arena, reference, frame or Local resource enters the command queue.
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    op: Operation,
) -> Result<Option<u64>, Error> {
    if matches!(frame.state, cps::DONE | cps::FAILED) {
        return Err(invalid("Control operation is already terminal"));
    }
    if frame.host_context.is_null() {
        let core = pool::control()?;
        let (kinds, _) = op.layout();
        let mut bytes = std::mem::size_of::<Request>()
            + std::mem::size_of::<Pending>()
            + std::mem::size_of::<Call>()
            + kinds.len() * (std::mem::size_of::<Value>() + 2 * std::mem::size_of::<u64>());
        for (kind, bits) in kinds.iter().zip(&frame.slots) {
            if let Kind::Bytes(stride, _) = kind {
                add_bytes(&mut bytes, byte_len(*bits, *stride)?)?;
            }
        }
        let charge = core.root.accounts().reserve(bytes)?;
        let input = kinds
            .iter()
            .zip(&frame.slots)
            .map(|(kind, bits)| {
                Ok(match kind {
                    Kind::Word => Value::Word(*bits),
                    Kind::Bytes(stride, _) => Value::Bytes(copy_buffer(*bits, *stride)?, *stride),
                    Kind::Resource => Value::Resource(proxy(*bits, &core)?),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let request = core.submit(op, input, charge)?;
        frame.host_context = Box::into_raw(Box::new(Pending { core, request })).cast();
        frame.host_stop = Some(stop);
    }
    let pending = &*frame.host_context.cast::<Pending>();
    let response = lock(&pending.request.state).result.take();
    let Some(response) = response else {
        pool::wait_for_wake();
        return Ok(None);
    };
    let response = response?;
    for (index, bytes) in response.updates {
        let stride = match op.layout().0[index] {
            Kind::Bytes(stride, _) => stride,
            _ => unreachable!(),
        };
        if matches!(
            op,
            Operation::VmMigration(crate::core::vm::MigrationOperation::Command)
        ) {
            let packet = &*(frame.slots[index] as *const Buffer);
            debug_assert_eq!(packet.len() as usize * stride, bytes.len());
            for (i, item) in bytes.chunks_exact(stride).enumerate() {
                packet
                    .load(i as u64)?
                    .copy_from_nonoverlapping(item.as_ptr(), stride);
            }
            continue;
        }
        let replacement = buffer(arena, &bytes, stride)?;
        Buffer::replace(
            arena,
            frame.slots[index] as *mut Buffer,
            replacement as *mut Buffer,
        )?;
    }
    let result = match response.value {
        Value::Word(value) => value,
        Value::Bytes(bytes, stride) => buffer(arena, &bytes, stride)?,
        Value::Received(bytes, present) => {
            let output = buffer(arena, &bytes, 1)?;
            frame.slots[1] = u64::from(present);
            output
        }
        Value::Resource(value) => adopt(arena, value)?,
    };
    stop(frame, crate::runtime::borrowed_host::STOP_COMPLETED);
    frame.result = result;
    frame.state = cps::DONE;
    Ok(Some(result))
}

struct LocalResource {
    pointer: u64,
    frame: Box<CpsFrame>,
}
impl Drop for LocalResource {
    fn drop(&mut self) {
        unsafe {
            let _ = Buffer::release(
                &mut *cps::arena(&mut *self.frame),
                self.pointer as *mut Buffer,
            );
        }
    }
}
struct Call {
    frame: Box<CpsFrame>,
    buffers: Vec<u64>,
    keep: Vec<Arc<Proxy>>,
    prepared_reply: Option<Reply>,
    #[cfg(test)]
    counted: bool,
    request: Arc<Request>,
}
impl Drop for Call {
    fn drop(&mut self) {
        unsafe {
            self.frame
                .stop_host(crate::runtime::borrowed_host::STOP_CANCELLED);
            let arena = &mut *cps::arena(&mut *self.frame);
            for raw in self.buffers.drain(..) {
                let _ = Buffer::release(arena, raw as *mut Buffer);
            }
        }
        #[cfg(test)]
        if self.counted {
            if let Some(core) = self.request.core.upgrade() {
                core.active_calls.fetch_sub(1, Ordering::Release);
            }
        }
        complete(&self.request, Err(invalid("Local control request stopped")));
    }
}
impl Call {
    unsafe fn new(
        request: Arc<Request>,
        resources: &HashMap<u64, LocalResource>,
    ) -> Result<Self, Error> {
        let input = lock(&request.state)
            .input
            .take()
            .ok_or_else(|| invalid("Control request already started"))?;
        let mut call = Self {
            frame: Box::new(CpsFrame::try_new(input.len())?),
            buffers: Vec::new(),
            keep: Vec::new(),
            prepared_reply: None,
            #[cfg(test)]
            counted: false,
            request,
        };
        // Publish the actual preparation error before partial cleanup runs the
        // abandonment fallback in Drop. The driver's later completion is idempotent.
        call.prepare(input, resources)
            .inspect_err(|error| complete(&call.request, Err(error.clone())))?;
        Ok(call)
    }
    unsafe fn prepare(
        &mut self,
        input: Vec<Value>,
        resources: &HashMap<u64, LocalResource>,
    ) -> Result<(), Error> {
        for (i, value) in input.into_iter().enumerate() {
            self.frame.slots[i] = match value {
                Value::Word(value) => value,
                Value::Received(_, _) => unreachable!("receive values are replies only"),
                Value::Bytes(bytes, stride) => {
                    let value = buffer(&mut *cps::arena(&mut *self.frame), &bytes, stride)?;
                    self.buffers.push(value);
                    value
                }
                Value::Resource(proxy) => {
                    let value = resources
                        .get(&proxy.id)
                        .ok_or_else(|| invalid("Closed control resource"))?
                        .pointer;
                    self.keep.push(proxy);
                    value
                }
            };
        }
        if matches!(
            self.request.op,
            Operation::VmMigration(crate::core::vm::MigrationOperation::Command)
        ) {
            // Preflight all reply storage before a command can commit. The command cannot resize this packet.
            let packet = &*(self.frame.slots[6] as *const Buffer);
            if packet.len() != 4 {
                return Err(invalid("Invalid migration result packet"));
            }
            let core = self
                .request
                .core
                .upgrade()
                .ok_or_else(|| invalid("Control driver closed"))?;
            let charge = core.root.accounts().reserve(
                std::mem::size_of::<Reply>() + std::mem::size_of::<(usize, Vec<u8>)>() + 32,
            )?;
            self.prepared_reply = Some(Reply {
                value: Value::Word(0),
                updates: vec![(6, copy_buffer(self.frame.slots[6], 8)?)],
                _charge: charge,
            });
        }
        #[cfg(test)]
        if matches!(self.request.op, Operation::VmCall) {
            if let Some(core) = self.request.core.upgrade() {
                core.active_calls.fetch_add(1, Ordering::Release);
                self.counted = true;
            }
        }
        Ok(())
    }
    unsafe fn poll(&mut self) -> Result<Option<u64>, Error> {
        let frame = &mut *self.frame;
        let arena = &mut *cps::arena(frame);
        let result = match self.request.op {
            Operation::VmCreate => {
                crate::runtime::resource::invoke(arena, &crate::stdlib::VM_FACTORY, frame.slots[0])
                    .map(|r| Some(r.as_ptr() as u64))
            }
            Operation::VmClose => Ok(Some(crate::stdlib::VM_CLOSE(frame.slots[0]) as u64)),
            Operation::VmMigration(op) => crate::stdlib::migration_poll(arena, frame, op),
            Operation::VmCall => crate::stdlib::call_poll(frame),
            Operation::VmSource => {
                if crate::vm::state_domains::parallel::gate(frame) {
                    crate::stdlib::source_poll(arena, frame).map(Some)
                } else {
                    Ok(None)
                }
            }
            Operation::Domain(crate::runtime::domain::Operation::OutboxReceiveBytes) => {
                let core = self
                    .request
                    .core
                    .upgrade()
                    .ok_or_else(|| invalid("Control driver closed"))?;
                let executor = crate::runtime::executor::current_executor()
                    .ok_or_else(|| invalid("Outbox receive requires an executor"))?;
                crate::vm::state_domains::receive_outbox_bytes(
                    frame,
                    &executor.domains(),
                    |bytes| {
                        let mut size = std::mem::size_of::<Reply>();
                        add_bytes(&mut size, bytes.len())?;
                        core.root.accounts().reserve(size)
                    },
                )
                .map(|received| {
                    received.map(|received| {
                        let present = received.bytes.is_some();
                        self.prepared_reply = Some(Reply {
                            value: Value::Received(received.bytes.unwrap_or_default(), present),
                            updates: Vec::new(),
                            _charge: received.prepared,
                        });
                        0
                    })
                })
            }
            Operation::Domain(op) => crate::runtime::domain::poll(arena, frame, op),
            Operation::Fs(op) => crate::runtime::fs::poll(arena, frame, op),
        };
        // Cancellation probes wait for a registered waiter, not just submission.
        #[cfg(test)]
        if !self.counted
            && matches!(result, Ok(None))
            && matches!(
                self.request.op,
                Operation::Domain(
                    crate::runtime::domain::Operation::AdmissionAcquire
                        | crate::runtime::domain::Operation::OutboxReceive
                        | crate::runtime::domain::Operation::OutboxReceiveBytes
                )
            )
        {
            if let Some(core) = self.request.core.upgrade() {
                core.active_calls.fetch_add(1, Ordering::Release);
                self.counted = true;
            }
        }
        result
    }
    unsafe fn finish(
        &mut self,
        core: &Arc<Core>,
        value: u64,
        resources: &mut HashMap<u64, LocalResource>,
    ) -> Result<Reply, Error> {
        if let Some(mut reply) = self.prepared_reply.take() {
            if matches!(
                self.request.op,
                Operation::Domain(crate::runtime::domain::Operation::OutboxReceiveBytes)
            ) {
                return Ok(reply);
            }
            let packet = &*(self.frame.slots[6] as *const Buffer);
            for (i, item) in reply.updates[0]
                .1
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .enumerate()
            {
                item.as_mut_ptr()
                    .copy_from_nonoverlapping(packet.load(i as u64)?, 8);
            }
            reply.value = Value::Word(value);
            return Ok(reply);
        }
        let (inputs, result) = self.request.op.layout();
        let mut charge_bytes = std::mem::size_of::<Reply>();
        for (index, kind) in inputs.iter().enumerate() {
            if let Kind::Bytes(stride, true) = kind {
                add_bytes(
                    &mut charge_bytes,
                    byte_len(self.frame.slots[index], *stride)?,
                )?;
                add_bytes(&mut charge_bytes, std::mem::size_of::<(usize, Vec<u8>)>())?;
            }
        }
        if let Kind::Bytes(stride, _) = result {
            add_bytes(&mut charge_bytes, byte_len(value, stride)?)?;
        }
        let charge = core.root.accounts().reserve(charge_bytes)?;
        let mut updates = Vec::new();
        for (index, kind) in inputs.iter().enumerate() {
            if let Kind::Bytes(stride, true) = kind {
                updates.push((index, copy_buffer(self.frame.slots[index], *stride)?));
            }
        }
        let value = match result {
            Kind::Word => Value::Word(value),
            Kind::Bytes(stride, _) => {
                let bytes = copy_buffer(value, stride)?;
                Buffer::release(&mut *cps::arena(&mut *self.frame), value as *mut Buffer)?;
                Value::Bytes(bytes, stride)
            }
            Kind::Resource => {
                if resources.len() >= LIMIT {
                    return Err(invalid("Control resource limit exceeded"));
                }
                static NEXT: AtomicU64 = AtomicU64::new(1);
                let id = NEXT
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                    .map_err(|_| invalid("Control identities exhausted"))?;
                let proxy = Arc::new(Proxy {
                    id,
                    shared_vm: matches!(self.request.op, Operation::VmCreate),
                    mailbox: matches!(
                        self.request.op,
                        Operation::Domain(
                            crate::runtime::domain::Operation::OutboxCreate
                                | crate::runtime::domain::Operation::OutboxCreateDisconnect
                        )
                    ),
                    exclusive: AtomicBool::new(false),
                    loan: None,
                    core: core.clone(),
                    _charge: core.root.accounts().reserve(
                        std::mem::size_of::<Proxy>() + std::mem::size_of::<LocalResource>(),
                    )?,
                });
                // Retain the owning arena on the control thread until its proxy is released.
                for raw in self.buffers.drain(..) {
                    Buffer::release(&mut *cps::arena(&mut *self.frame), raw as *mut Buffer)?;
                }
                resources.insert(
                    id,
                    LocalResource {
                        pointer: value,
                        frame: std::mem::replace(&mut self.frame, Box::new(CpsFrame::new(0))),
                    },
                );
                Value::Resource(proxy)
            }
        };
        Ok(Reply {
            value,
            updates,
            _charge: charge,
        })
    }
}
fn complete(request: &Request, result: Result<Reply, Error>) {
    let mut state = lock(&request.state);
    if state.completed {
        return;
    }
    state.completed = true;
    if !state.cancelled {
        state.result = Some(result);
    }
    request.done.notify_all();
    drop(state);
    if let Some(wake) = &request.wake {
        wake.wake();
    }
}
fn run(core: &Arc<Core>, context: &BlockingContext) {
    let mut resources = HashMap::new();
    let mut active: Vec<Call> = Vec::new();
    loop {
        if let Err(error) = context.drive() {
            for call in active.drain(..) {
                complete(&call.request, Err(error.clone()));
            }
        }
        let mut plan = WaitPlan::default();
        let closing = context.poll(|| {
            let (requests, releases, closing) = {
                let mut q = lock(&core.queue);
                (
                    q.requests.drain(..).collect::<Vec<_>>(),
                    std::mem::take(&mut q.releases),
                    q.closing,
                )
            };
            // Accept cancellation and owner release before the domain commit pass.
            // This must remain after context.drive(): poll() consumes its wake token.
            active.retain(|call| !closing && !lock(&call.request.state).cancelled);
            for id in releases {
                resources.remove(&id);
            }
            crate::vm::state_domains::parallel::drive();
            for request in requests {
                if closing {
                    complete(&request, Err(invalid("Local control driver closed")));
                    continue;
                }
                if lock(&request.state).cancelled {
                    continue;
                }
                match unsafe { Call::new(request.clone(), &resources) } {
                    Ok(call) => active.push(call),
                    Err(e) => complete(&request, Err(e)),
                }
            }
            let mut index = 0;
            while index < active.len() {
                if closing || lock(&active[index].request.state).cancelled {
                    let call = active.swap_remove(index);
                    complete(
                        &call.request,
                        Err(invalid("Local control request cancelled")),
                    );
                    continue;
                }
                let result = crate::runtime::error::checked(|| unsafe { active[index].poll() })
                    .and_then(|v| v);
                match result {
                    Ok(None) => {
                        plan.merge(active[index].frame.wait_plan());
                        index += 1;
                    }
                    ready => {
                        let mut call = active.swap_remove(index);
                        let result = ready.and_then(|v| unsafe {
                            let value = v.unwrap();
                            let result = call.finish(core, value, &mut resources);
                            if result.is_err() && !matches!(call.request.op.layout().1, Kind::Word)
                            {
                                let _ = Buffer::release(
                                    &mut *cps::arena(&mut *call.frame),
                                    value as *mut Buffer,
                                );
                            }
                            result
                        });
                        complete(&call.request, result);
                    }
                }
            }
            closing
        });
        if closing {
            break;
        }
        let queued = {
            let q = lock(&core.queue);
            q.closing || !q.requests.is_empty() || !q.releases.is_empty()
        };
        if !queued {
            if let Err(error) = context.wait(plan) {
                for call in active.drain(..) {
                    complete(&call.request, Err(error.clone()));
                }
            }
        }
    }
    context.poll(|| {
        drop(active);
        drop(resources);
    });
    while crate::vm::state_domains::parallel::busy() {
        context.poll(crate::vm::state_domains::parallel::drive);
        if crate::vm::state_domains::parallel::busy() {
            let _ = context.wait(WaitPlan::default());
        }
    }
}

/// The synchronous VM constructor only creates a Local registry entry; it cannot
/// execute user code or await another worker. All subsequent calls are asynchronous.
pub(crate) fn create_vm(arena: &mut Arena, mode: u64) -> Result<NonNull<Buffer>, Error> {
    let core = pool::control()?;
    let charge = core
        .root
        .accounts()
        .reserve(std::mem::size_of::<Request>() + std::mem::size_of::<Value>())?;
    let request = core.submit(Operation::VmCreate, vec![Value::Word(mode)], charge)?;
    let mut state = lock(&request.state);
    while state.result.is_none() {
        state = request.done.wait(state).unwrap_or_else(|e| e.into_inner());
    }
    let response = state.result.take().unwrap()?;
    drop(state);
    let Value::Resource(proxy) = response.value else {
        return Err(invalid("Invalid VM constructor reply"));
    };
    NonNull::new(adopt(arena, proxy)? as *mut Buffer).ok_or_else(|| invalid("Null VM proxy"))
}
pub(crate) fn vm_factory(factory: &crate::runtime::resource::ResourceFactoryV1) -> bool {
    factory
        .create
        .zip(crate::stdlib::VM_FACTORY.create)
        .is_some_and(|(a, b)| std::ptr::fn_addr_eq(a, b))
        && factory
            .release
            .zip(crate::stdlib::VM_FACTORY.release)
            .is_some_and(|(a, b)| std::ptr::fn_addr_eq(a, b))
}
pub(crate) struct Files(pub(crate) Option<crate::runtime::fs::FileService>);

pub(crate) unsafe fn share_vm(arena: &mut Arena, bits: u64) -> Result<u64, Error> {
    let core = pool::control()?;
    adopt(arena, proxy(bits, &core)?)
}
pub(crate) unsafe fn close_vm(bits: u64) -> Result<u64, Error> {
    let core = pool::control()?;
    let input = proxy(bits, &core)?;
    let charge = core
        .root
        .accounts()
        .reserve(std::mem::size_of::<Request>() + std::mem::size_of::<Value>())?;
    let request = core.submit(Operation::VmClose, vec![Value::Resource(input)], charge)?;
    let mut state = lock(&request.state);
    while state.result.is_none() {
        state = request.done.wait(state).unwrap_or_else(|e| e.into_inner());
    }
    let response = state.result.take().unwrap()?;
    match response.value {
        Value::Word(value) => Ok(value),
        _ => Err(invalid("Invalid VM close reply")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_borrow_vm_rejects_other_proxy_kinds_and_control_trees() {
        struct Probe;
        impl pool::Job for Probe {
            fn poll(&mut self) -> std::task::Poll<Result<pool::Output, Error>> {
                let result = (|| unsafe {
                    let root = super::super::current().unwrap();
                    let mut arena = Arena::with_storage(root.clone());
                    let core = pool::control()?;
                    let owner = create_vm(&mut arena, 1)?.as_ptr() as u64;
                    let shared = SharedVm::capture(owner)?.install()?;
                    let alias = Buffer::external(&mut arena, shared)?.as_ptr() as u64;
                    assert!(SharedVm::capture(alias).is_ok());
                    let (driver, other) = Driver::start(root.clone(), None, Default::default())?;
                    for (tree, shared_vm) in [(core.clone(), false), (other.clone(), true)] {
                        let forged = adopt(
                            &mut arena,
                            Arc::new(Proxy {
                                id: u64::MAX,
                                shared_vm,
                                mailbox: false,
                                exclusive: AtomicBool::new(false),
                                loan: None,
                                core: tree,
                                _charge: root.accounts().reserve(std::mem::size_of::<Proxy>())?,
                            }),
                        )?;
                        assert!(SharedVm::capture(forged).is_err());
                        Buffer::release(&mut arena, forged as *mut Buffer)?;
                    }
                    driver.join();
                    assert!(SharedVm::capture(0).is_err());
                    Buffer::release(&mut arena, owner as *mut Buffer)?;
                    assert!(SharedVm::capture(owner).is_err());
                    // Dropping the source alias does not invalidate the copied identity.
                    assert!(SharedVm::capture(alias).is_ok());
                    Buffer::release(&mut arena, alias as *mut Buffer)?;
                    Ok(Box::new(()) as pool::Output)
                })();
                std::task::Poll::Ready(result)
            }
        }
        let budget = super::super::TaskBudget::new(32 << 20);
        let mut executor = pool::ParallelExecutor::new(1, 8).unwrap();
        let task = executor
            .submit(
                Storage::new(vec![budget.clone()]).unwrap(),
                Arc::new(()),
                Box::new(|| Ok(Box::new(Probe))),
            )
            .unwrap();
        task.wait().unwrap();
        task.take().unwrap().unwrap();
        drop(task);
        executor.wait_closed();
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn parallel_control_partial_call_preparation_preserves_budget_error() {
        let budget = super::super::TaskBudget::new(16 << 10);
        let storage = Storage::new(vec![budget.clone()]).unwrap();
        let guard = Guard::enter(&storage);
        let request = Arc::new(Request {
            op: Operation::VmSource,
            core: Weak::new(),
            _receipt: None,
            _sequence: 0,
            state: Mutex::new(RequestState {
                // The second buffer cannot fit after a frame and first buffer
                // have been prepared. Destruction must not mask this failure.
                input: Some(vec![
                    Value::Bytes(vec![0; 16], 1),
                    Value::Bytes(vec![0; 1 << 20], 1),
                ]),
                result: None,
                cancelled: false,
                completed: false,
            }),
            wake: None,
            done: std::sync::Condvar::new(),
            _charge: storage
                .accounts()
                .reserve(std::mem::size_of::<Request>())
                .unwrap(),
        });
        let error = unsafe { Call::new(request.clone(), &HashMap::new()) }
            .err()
            .unwrap();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        // Match the driver's completion path after the partial Call was dropped.
        complete(&request, Err(error));
        let result = lock(&request.state).result.take().unwrap();
        assert_eq!(result.err().unwrap().code(), ErrorCode::ResourceLimit);
        drop((request, guard, storage));
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn migration_command_reply_is_reserved_before_execution() {
        let budget = super::super::TaskBudget::new(32 << 20);
        let storage = Storage::new(vec![budget.clone()]).unwrap();
        let guard = super::super::Guard::enter(&storage);
        let (driver, core) = Driver::start(storage.clone(), None, Default::default()).unwrap();
        core.outstanding.fetch_add(1, Ordering::Relaxed);
        let request = Arc::new(Request {
            op: Operation::VmMigration(crate::core::vm::MigrationOperation::Command),
            core: Arc::downgrade(&core),
            _receipt: None,
            _sequence: 0,
            state: Mutex::new(RequestState {
                input: Some(vec![
                    Value::Word(0),
                    Value::Word(7),
                    Value::Word(0),
                    Value::Bytes(vec![], 1),
                    Value::Bytes(vec![], 1),
                    Value::Bytes(vec![], 1),
                    Value::Bytes(vec![0; 32], 8),
                ]),
                result: None,
                cancelled: false,
                completed: false,
            }),
            wake: None,
            done: std::sync::Condvar::new(),
            _charge: storage
                .accounts()
                .reserve(std::mem::size_of::<Request>())
                .unwrap(),
        });
        let mut resources = HashMap::new();
        let mut call = unsafe { Call::new(request, &resources) }.unwrap();
        let held = storage
            .accounts()
            .reserve((32 << 20) - budget.used())
            .unwrap();
        assert!(storage.accounts().reserve(1).is_err());
        unsafe {
            (&*(call.frame.slots[6] as *const Buffer))
                .load(0)
                .unwrap()
                .cast::<u64>()
                .write(42);
        }
        let reply = unsafe { call.finish(&core, 0, &mut resources) }.unwrap();
        assert!(matches!(reply.value, Value::Word(0)));
        assert_eq!(&reply.updates[0].1[..8], &42_u64.to_ne_bytes());
        drop((held, reply, call));
        driver.join();
        drop((core, guard, storage));
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn parallel_control_proxy_identity_rejects_another_tree_and_refunds() {
        let budget = super::super::TaskBudget::new(32 << 20);
        let first = Storage::new(vec![budget.clone()]).unwrap();
        let second = Storage::new(vec![budget.clone()]).unwrap();
        let (one, core) = Driver::start(first.clone(), None, Default::default()).unwrap();
        let (two, other) = Driver::start(second.clone(), None, Default::default()).unwrap();
        let request = core
            .submit(
                Operation::VmCreate,
                vec![Value::Word(1)],
                first
                    .accounts()
                    .reserve(std::mem::size_of::<Request>())
                    .unwrap(),
            )
            .unwrap();
        let mut state = lock(&request.state);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while state.result.is_none() {
            assert!(std::time::Instant::now() < deadline);
            state = request
                .done
                .wait_timeout(state, std::time::Duration::from_millis(10))
                .unwrap()
                .0;
        }
        let response = state.result.take().unwrap().unwrap();
        drop(state);
        let Value::Resource(value) = response.value else {
            panic!("VM constructor must return a proxy")
        };
        let mut arena = Arena::with_storage(first.clone());
        let bits = adopt(&mut arena, value).unwrap();
        unsafe {
            assert!(proxy(bits, &core).is_ok());
            assert!(proxy(bits, &other).is_err());
            Buffer::release(&mut arena, bits as *mut Buffer).unwrap();
        }
        drop((request, response._charge, arena));
        one.join();
        two.join();
        drop((core, other, first, second));
        assert_eq!(budget.used(), 0);
    }
}
