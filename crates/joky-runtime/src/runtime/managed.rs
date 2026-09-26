//! Runtime implementation of managed values and strings/lists
//!
//! A hidden header in front of the payload records the ABI version, value kind,
//! layout, and reference count. String, plain ADT, and List use shared
//! reference counting; class and native handles keep unique ownership
//! semantics. Aggregate values containing class fields are still handled by
//! MIR with per-field ownership

use std::alloc::{self, Layout};
#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
use std::mem::{align_of, size_of};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
#[cfg(any(test, feature = "test-support"))]
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
use super::list::*;
pub(crate) use super::string::*;
pub(crate) use super::{bytes, list, map, mut_list, mut_map};

pub(crate) const RUNTIME_ABI_VERSION: u32 = 8;

static NEXT_DEBUG_ID: AtomicU64 = AtomicU64::new(1);

const OBJECT_MAGIC: u32 = 0x4a4f_4b59;

#[cfg(test)]
pub(crate) fn managed_abi_descriptor() -> crate::runtime::abi::ManagedAbiDescriptor {
    crate::runtime::abi::ManagedAbiDescriptor {
        version: RUNTIME_ABI_VERSION,
        header_size: std::mem::size_of::<ObjectHeader>() as u32,
        cown_state_size: std::mem::size_of::<CownState>() as u32,
        cown_id_offset: std::mem::offset_of!(CownState, id) as i32,
        cown_payload_offset: std::mem::offset_of!(CownState, payload) as i32,
    }
}

/// Runtime state stored inside a Cown object. The payload itself remains
/// uniquely owned; the atomic lease bit is the first single-Cown scheduler
/// primitive and makes concurrent `when` bodies mutually exclusive.
#[repr(C)]
pub(crate) struct CownState {
    pub(crate) id: u64,
    pub(crate) payload: *mut u8,
    // Consumes the entire payload, including its allocation. Class drop glue
    // must run before the JIT module that owns this callback is unloaded.
    payload_drop: Option<unsafe extern "C" fn(*mut u8)>,
    lease: super::cown::Lease,
}

static NEXT_COWN_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static LIVE_OBJECTS: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
}

#[cfg(any(test, feature = "test-support"))]
struct LiveObjectOwners {
    thread: Arc<AtomicUsize>,
    scope: Arc<AtomicUsize>,
    /// Allocation backtrace, recorded only under JOKY_TRACE_MANAGED for
    /// leak triage.
    alloc_trace: Option<String>,
    allocation_thread: std::thread::ThreadId,
}

#[cfg(any(test, feature = "test-support"))]
fn live_payloads() -> &'static Mutex<HashMap<usize, LiveObjectOwners>> {
    static PAYLOADS: OnceLock<Mutex<HashMap<usize, LiveObjectOwners>>> = OnceLock::new();
    PAYLOADS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether per-allocation leak tracking is active. Tracking costs a global
/// mutex round trip per allocation and per validation, so test binaries that
/// carry this feature but do not ask for a resource report must stay on the
/// cheap path. The flag is monotonic: entries are only inserted while it is
/// set, and releases only remove what was inserted, so a one-way flip keeps
/// the registry consistent without a transition window. The off decision is
/// cached too: an environment probe per allocation would cost more than the
/// tracking it avoids.
#[cfg(any(test, feature = "test-support"))]
static OBJECT_TRACKING_STATE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);

#[cfg(any(test, feature = "test-support"))]
const TRACKING_UNDECIDED: u8 = 0;
#[cfg(any(test, feature = "test-support"))]
const TRACKING_OFF: u8 = 1;
#[cfg(any(test, feature = "test-support"))]
const TRACKING_ON: u8 = 2;

#[cfg(any(test, feature = "test-support"))]
fn object_tracking_enabled() -> bool {
    match OBJECT_TRACKING_STATE.load(Ordering::Relaxed) {
        TRACKING_ON => true,
        TRACKING_OFF => false,
        _ => {
            let default_on = cfg!(test)
                || std::env::var_os("JOKY_TEST_RESOURCE_REPORT").is_some()
                || std::env::var_os("JOKY_TRACE_MANAGED").is_some();
            let state = if default_on { TRACKING_ON } else { TRACKING_OFF };
            OBJECT_TRACKING_STATE.store(state, Ordering::Relaxed);
            default_on
        }
    }
}

/// Turn leak tracking on for the rest of the process. Call before any managed
/// allocation exists (e.g. before running a program in host unit tests);
/// payloads allocated while tracking was off are invisible to the registry.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn enable_object_tracking() {
    OBJECT_TRACKING_STATE.store(TRACKING_ON, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn live_object_count() -> usize {
    LIVE_OBJECTS.with(|objects| objects.load(Ordering::Acquire))
}

/// Number of managed objects alive across every thread right now.
///
/// Per-program leak checks must use this process-wide count rather than the
/// thread-local count: provider completions (reactor,
/// blocking pool) free payloads on the thread that resumed them, not on the
/// thread that started the program. Use only in isolated tests: unrelated
/// tests can allocate on other threads.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn live_object_count_all_threads() -> usize {
    live_payloads()
        .lock()
        .expect("managed payload registry mutex")
        .len()
}

/// Leak triage: describe every payload still registered, by address. Entries
/// whose header magic no longer matches point at freed memory, which means the
/// registry itself leaked a stale entry rather than an object being alive.
/// Allocation backtraces are only recorded under JOKY_TRACE_MANAGED; enabling
/// it perturbs heap layout, which can mask layout-sensitive leaks.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn live_payload_report() -> Vec<String> {
    live_payloads()
        .lock()
        .expect("managed payload registry mutex")
        .iter()
        .map(|(address, owners)| {
            let payload = *address as *mut u8;
            let header_pointer = unsafe {
                payload
                    .sub(size_of::<*mut ObjectHeader>())
                    .cast::<*mut ObjectHeader>()
                    .read()
            };
            let thread_owners = owners.thread.load(Ordering::Acquire);
            let scope_owners = owners.scope.load(Ordering::Acquire);
            if header_pointer.is_null() || unsafe { (*header_pointer).magic } != OBJECT_MAGIC {
                return format!(
                    "payload {address:#x}: STALE ENTRY (freed memory or corrupt back-pointer), \
                     allocated_on={:?} thread_owners={thread_owners}, scope_owners={scope_owners}",
                    owners.allocation_thread
                );
            }
            let header = unsafe { &*header_pointer };
            format!(
                "kind={:?} ref_count={} payload_size={} address={address:#x} \
                 allocated_on={:?} thread_owners={thread_owners} scope_owners={scope_owners}\n\
                 alloc trace:\n{}",
                RuntimeValueKind::from_raw(header.kind),
                header.ref_count.load(Ordering::Acquire),
                header.payload_size,
                owners.allocation_thread,
                owners.alloc_trace.as_deref().unwrap_or("(tracing off)")
            )
        })
        .collect()
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeValueKind {
    String = 1,
    Class = 2,
    Adt = 3,
    NativeHandle = 4,
    List = 5,
    Map = 6,
    MutList = 7,
    MutMap = 8,
    Cown = 9,
    Bytes = 10,
    MutBytes = 11,
    Batch = 12,
    Hasher = 13,
    BytesCursor = 14,
    MapCursor = 15,
    SeqBuilder = 16,
    MutListCursor = 17,
    MutMapCursor = 18,
}

impl RuntimeValueKind {
    pub(crate) fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::String),
            2 => Some(Self::Class),
            3 => Some(Self::Adt),
            4 => Some(Self::NativeHandle),
            5 => Some(Self::List),
            6 => Some(Self::Map),
            7 => Some(Self::MutList),
            8 => Some(Self::MutMap),
            9 => Some(Self::Cown),
            10 => Some(Self::Bytes),
            11 => Some(Self::MutBytes),
            12 => Some(Self::Batch),
            13 => Some(Self::Hasher),
            14 => Some(Self::BytesCursor),
            15 => Some(Self::MapCursor),
            16 => Some(Self::SeqBuilder),
            17 => Some(Self::MutListCursor),
            18 => Some(Self::MutMapCursor),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnershipKind {
    Owned = 1,
    Shared = 2,
    #[allow(dead_code)] // Reserved for non-owning values in the managed ABI.
    Borrowed = 3,
}

#[repr(C)]
pub(crate) struct ObjectHeader {
    pub(crate) debug_id: u64,
    pub(crate) magic: u32,
    pub(crate) abi_version: u32,
    pub(crate) kind: u8,
    pub(crate) ownership: u8,
    pub(crate) reserved: u16,
    pub(crate) ref_count: AtomicUsize,
    pub(crate) payload_size: usize,
    pub(crate) payload_align: usize,
    pub(crate) payload_offset: usize,
    pub(crate) allocation_size: usize,
    pub(crate) allocation_align: usize,
    pub(crate) drop_callback: Option<unsafe extern "C" fn(*mut u8)>,
    #[cfg(test)]
    allocation_thread: std::thread::ThreadId,
}

fn object_layout(payload_size: usize, payload_align: usize) -> Option<(Layout, usize)> {
    if payload_align == 0 || !payload_align.is_power_of_two() {
        return None;
    }
    let payload_offset = size_of::<ObjectHeader>()
        .checked_add(size_of::<*mut ObjectHeader>())?
        .checked_add(payload_align - 1)?
        & !(payload_align - 1);
    let allocation_size = payload_offset.checked_add(payload_size)?.max(1);
    let allocation_align = align_of::<ObjectHeader>().max(payload_align);
    let layout = Layout::from_size_align(allocation_size, allocation_align).ok()?;
    Some((layout, payload_offset))
}

pub(crate) unsafe fn valid_header<'a>(payload: *mut u8) -> Option<&'a ObjectHeader> {
    if payload.is_null() {
        return None;
    }
    #[cfg(any(test, feature = "test-support"))]
    if object_tracking_enabled()
        && !live_payloads()
            .lock()
            .expect("managed payload registry mutex")
            .contains_key(&(payload as usize))
    {
        return None;
    }
    let back_pointer = payload
        .sub(size_of::<*mut ObjectHeader>())
        .cast::<*mut ObjectHeader>();
    let header_pointer = back_pointer.read();
    if header_pointer.is_null() {
        return None;
    }
    if !(header_pointer as usize).is_multiple_of(align_of::<ObjectHeader>()) {
        return None;
    }
    let header = &*header_pointer;
    if header.magic != OBJECT_MAGIC
        || header.abi_version != RUNTIME_ABI_VERSION
        || RuntimeValueKind::from_raw(header.kind).is_none()
        || !matches!(header.ownership, 1..=3)
        || header.ref_count.load(Ordering::Acquire) == 0
    {
        return None;
    }
    Some(header)
}

/// Allocate an opaque managed object. The returned pointer addresses payload;
/// a hidden back-pointer immediately before it recovers the object header.
pub(crate) extern "C" fn jk_alloc_object(
    kind: u8,
    payload_size: usize,
    payload_align: usize,
) -> *mut u8 {
    let Some(kind) = RuntimeValueKind::from_raw(kind) else {
        return std::ptr::null_mut();
    };
    allocate_object(kind, payload_size, payload_align, None)
}

/// Allocate a closure environment whose generated drop glue is invoked before
/// the runtime frees the payload.
pub(crate) extern "C" fn jk_alloc_closure_environment(
    payload_size: usize,
    payload_align: usize,
    drop_callback: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut u8 {
    allocate_object(
        RuntimeValueKind::NativeHandle,
        payload_size,
        payload_align,
        drop_callback,
    )
}

/// Allocate a uniquely owned opaque native resource. The callback runs once
/// when the resource leaves the language ownership graph.
pub(crate) extern "C" fn jk_alloc_native_handle(
    payload_size: usize,
    payload_align: usize,
    drop_callback: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut u8 {
    allocate_object(
        RuntimeValueKind::NativeHandle,
        payload_size,
        payload_align,
        drop_callback,
    )
}

pub(super) fn allocate_object(
    kind: RuntimeValueKind,
    payload_size: usize,
    payload_align: usize,
    drop_callback: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut u8 {
    allocate_object_with_padding(kind, payload_size, payload_align, 0, drop_callback)
}

/// Allocate a managed object whose allocation extends `padding` bytes past
/// `payload_size`. The header keeps the logical `payload_size`; String uses
/// this to hide a NUL terminator beyond the UTF-8 length.
pub(super) fn allocate_object_with_padding(
    kind: RuntimeValueKind,
    payload_size: usize,
    payload_align: usize,
    padding: usize,
    drop_callback: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut u8 {
    let allocated_size = match payload_size.checked_add(padding) {
        Some(size) => size,
        None => return std::ptr::null_mut(),
    };
    let Some((layout, payload_offset)) = object_layout(allocated_size, payload_align) else {
        return std::ptr::null_mut();
    };
    let header_pointer = unsafe { alloc::alloc(layout) };
    if header_pointer.is_null() {
        return header_pointer;
    }
    let payload = unsafe { header_pointer.add(payload_offset) };
    unsafe {
        header_pointer.cast::<ObjectHeader>().write(ObjectHeader {
            debug_id: if kind == RuntimeValueKind::NativeHandle {
                NEXT_DEBUG_ID.fetch_add(1, Ordering::Relaxed)
            } else {
                0
            },
            magic: OBJECT_MAGIC,
            abi_version: RUNTIME_ABI_VERSION,
            kind: kind as u8,
            ownership: match kind {
                RuntimeValueKind::Class
                | RuntimeValueKind::Hasher
                | RuntimeValueKind::MapCursor
                | RuntimeValueKind::MutListCursor
                | RuntimeValueKind::MutMapCursor
                | RuntimeValueKind::NativeHandle
                | RuntimeValueKind::MutList
                | RuntimeValueKind::MutBytes => OwnershipKind::Owned,
                RuntimeValueKind::MutMap => OwnershipKind::Owned,
                RuntimeValueKind::Cown | RuntimeValueKind::Batch | RuntimeValueKind::SeqBuilder => {
                    OwnershipKind::Shared
                }
                RuntimeValueKind::String
                | RuntimeValueKind::Adt
                | RuntimeValueKind::List
                | RuntimeValueKind::Map
                | RuntimeValueKind::Bytes
                | RuntimeValueKind::BytesCursor => OwnershipKind::Shared,
            } as u8,
            reserved: 0,
            ref_count: AtomicUsize::new(1),
            payload_size,
            payload_align,
            payload_offset,
            allocation_size: layout.size(),
            allocation_align: layout.align(),
            drop_callback,
            #[cfg(test)]
            allocation_thread: std::thread::current().id(),
        });
        payload
            .sub(size_of::<*mut ObjectHeader>())
            .cast::<*mut ObjectHeader>()
            .write(header_pointer.cast::<ObjectHeader>());
    }
    #[cfg(any(test, feature = "test-support"))]
    if object_tracking_enabled() {
        let thread = LIVE_OBJECTS.with(Arc::clone);
        let scope = Arc::clone(&super::scope::current_or_default().managed_objects);
        thread.fetch_add(1, Ordering::Relaxed);
        scope.fetch_add(1, Ordering::Relaxed);
        let alloc_trace = if std::env::var_os("JOKY_TRACE_MANAGED").is_some()
            && kind == RuntimeValueKind::Bytes
        {
            Some(std::backtrace::Backtrace::force_capture().to_string())
        } else {
            None
        };
        live_payloads()
            .lock()
            .expect("managed payload registry mutex")
            .insert(
                payload as usize,
                LiveObjectOwners {
                    thread,
                    scope,
                    alloc_trace,
                    allocation_thread: std::thread::current().id(),
                },
            );
    }
    payload
}

unsafe extern "C" fn drop_cown(payload: *mut u8) {
    let state = payload.cast::<CownState>();
    let owned_payload = (*state).payload;
    if !owned_payload.is_null() {
        drop_cown_owned_payload(owned_payload, (*state).payload_drop);
    }
    std::ptr::drop_in_place(state);
}

fn drop_cown_owned_payload(payload: *mut u8, drop: Option<unsafe extern "C" fn(*mut u8)>) {
    if let Some(drop) = drop {
        // Unlike an ObjectHeader callback, this destructor also frees the
        // payload allocation. Do not follow it with a second jk_drop.
        unsafe { drop(payload) };
    } else {
        jk_drop(payload);
    }
}

/// Wrap a uniquely owned payload in a shared Cown capability.
pub(crate) extern "C" fn jk_cown_new(
    payload: *mut u8,
    payload_drop: Option<unsafe extern "C" fn(*mut u8)>,
) -> *mut u8 {
    if payload.is_null() {
        return std::ptr::null_mut();
    }
    let cown = allocate_object(
        RuntimeValueKind::Cown,
        size_of::<CownState>(),
        align_of::<CownState>(),
        Some(drop_cown),
    );
    if cown.is_null() {
        drop_cown_owned_payload(payload, payload_drop);
        return cown;
    }
    unsafe {
        cown.cast::<CownState>().write(CownState {
            id: NEXT_COWN_ID.fetch_add(1, Ordering::Relaxed),
            payload,
            payload_drop,
            lease: super::cown::Lease::default(),
        });
    }
    super::region::current().register(cown);
    cown.map_addr(|addr| addr | 1)
}

/// Tagged Cown handles can be discarded after region cleanup without reading
/// reclaimed memory (e.g. a later container cleanup). All dereferences untag.
fn cown_pointer(cown: *mut u8) -> *mut u8 {
    cown.map_addr(|addr| addr & !1)
}

#[cfg(test)]
pub(crate) fn cown_waiter_count(cown: *mut u8) -> usize {
    unsafe {
        (*cown_pointer(cown).cast::<CownState>())
            .lease
            .waiter_count()
    }
}

pub(crate) unsafe fn drop_region_cown_payload(cown: *mut u8) {
    unsafe {
        assert!(!(*cown.cast::<CownState>()).lease.is_leased());
        let state = &*cown.cast::<CownState>();
        drop_cown_owned_payload(state.payload, state.payload_drop);
        (*cown.cast::<CownState>()).payload = std::ptr::null_mut();
    }
}

fn cown_wait_cancelled() -> bool {
    crate::runtime::task::current_task_context()
        .is_some_and(crate::runtime::task::task_context_is_cancelled)
}

fn register_current_cown_leases(cowns: &[*mut u8]) {
    let Some(context) = crate::runtime::task::current_task_context() else {
        return;
    };
    crate::runtime::task::register_cown_leases(context, cowns);
}

/// Enter a single-Cown lease. Contention parks on this Cown, while workers
/// can still help queued work. Release and cancellation wake exact waiters.
pub(crate) extern "C" fn jk_cown_acquire(cown: *mut u8) -> *mut u8 {
    let Some(header) = (unsafe { valid_header(cown_pointer(cown)) }) else {
        return std::ptr::null_mut();
    };
    if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::Cown) {
        return std::ptr::null_mut();
    }
    let state = cown_pointer(cown).cast::<CownState>();
    loop {
        if cown_wait_cancelled() {
            // Cancellation may arrive after this task was selected and left
            // the queue, but before it retried acquisition.
            unsafe { (*state).lease.notify_if_available() };
            jk_drop(cown);
            return std::ptr::null_mut();
        }
        if unsafe { (*state).lease.try_acquire() } {
            break;
        }
        if !crate::runtime::task::help_current_worker() {
            unsafe { (*state).lease.wait() };
        }
    }
    let payload = unsafe { (*state).payload };
    register_current_cown_leases(&[cown]);
    // MIR models the acquire argument as consumed, so release that capability
    // after borrowing the payload for the lease body.
    jk_drop(cown);
    payload
}

/// Attempt a whole set once. On contention return the actual blocking Cown;
/// null means invalid input. No partial lease survives a failed attempt.
pub(crate) fn try_cown_acquire_many(handles: &[*mut u8]) -> Result<(), *mut u8> {
    if handles.is_empty() {
        return Err(std::ptr::null_mut());
    }
    if handles.len() == 1 {
        let handle = handles[0];
        let Some(header) = (unsafe { valid_header(cown_pointer(handle)) }) else {
            return Err(std::ptr::null_mut());
        };
        if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::Cown) {
            return Err(std::ptr::null_mut());
        }
        if !try_cown_lease_briefly(unsafe { cown_lease(handle) }) {
            return Err(handle);
        }
        register_current_cown_leases(handles);
        return Ok(());
    }
    let mut ordered = Vec::with_capacity(handles.len());
    for &handle in handles {
        let Some(header) = (unsafe { valid_header(cown_pointer(handle)) }) else {
            return Err(std::ptr::null_mut());
        };
        if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::Cown) {
            return Err(std::ptr::null_mut());
        }
        ordered.push((
            unsafe { (*cown_pointer(handle).cast::<CownState>()).id },
            handle,
        ));
    }
    ordered.sort_unstable_by_key(|entry| entry.0);
    if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(std::ptr::null_mut());
    }
    for (index, &(_, handle)) in ordered.iter().enumerate() {
        if !try_cown_lease_briefly(unsafe { cown_lease(handle) }) {
            for &(_, acquired) in &ordered[..index] {
                unsafe {
                    cown_lease(acquired).release_quiet();
                }
            }
            return Err(handle);
        }
    }
    register_current_cown_leases(handles);
    Ok(())
}

fn try_cown_lease_briefly(lease: &super::cown::Lease) -> bool {
    if lease.try_acquire() {
        return true;
    }
    // A short body can release before a continuation is saved and enqueued.
    // This is bounded CPU work, never a park, a helper call, or a timed wait.
    for _ in 0..32 {
        std::hint::spin_loop();
        if !lease.is_leased() && lease.try_acquire() {
            return true;
        }
    }
    false
}

/// The caller must keep a validated Cown's region alive for this borrow.
pub(crate) unsafe fn cown_lease<'a>(cown: *mut u8) -> &'a super::cown::Lease {
    unsafe { &(*cown_pointer(cown).cast::<CownState>()).lease }
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_cown_try_acquire_many(
    cowns: *const *mut u8,
    count: usize,
) -> u8 {
    use crate::runtime::abi::FunctionCallStatus;
    if cown_wait_cancelled() {
        return FunctionCallStatus::Cancelled as u8;
    }
    if cowns.is_null() || count == 0 {
        return FunctionCallStatus::Failed as u8;
    }
    match try_cown_acquire_many(unsafe { std::slice::from_raw_parts(cowns, count) }) {
        Ok(()) => FunctionCallStatus::Ready as u8,
        Err(blocked) if !blocked.is_null() => FunctionCallStatus::Pending as u8,
        Err(_) => FunctionCallStatus::Failed as u8,
    }
}

/// Atomically acquire a set of Cowns. Handles are ordered by their stable
/// runtime id, so competing `when (a, b)` and `when (b, a)` operations cannot
/// deadlock. A failed attempt rolls back every lease acquired so far.
pub(crate) extern "C" fn jk_cown_acquire_many(
    cowns: *const *mut u8,
    count: usize,
    payloads: *mut *mut u8,
) -> u8 {
    if count == 0 || cowns.is_null() || payloads.is_null() {
        return 0;
    }
    if count == 1 {
        let payload = jk_cown_acquire(unsafe { *cowns });
        unsafe {
            *payloads = payload;
        }
        return u8::from(!payload.is_null());
    }
    let handles = unsafe { std::slice::from_raw_parts(cowns, count) };
    let mut ordered = Vec::with_capacity(count);
    for (index, &handle) in handles.iter().enumerate() {
        let Some(header) = (unsafe { valid_header(cown_pointer(handle)) }) else {
            for &capability in handles {
                jk_drop(capability);
            }
            return 0;
        };
        if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::Cown) {
            for &capability in handles {
                jk_drop(capability);
            }
            return 0;
        }
        let state = cown_pointer(handle).cast::<CownState>();
        ordered.push((unsafe { (*state).id }, index, state, handle));
    }
    ordered.sort_unstable_by_key(|entry| entry.0);
    if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        for &capability in handles {
            jk_drop(capability);
        }
        return 0;
    }
    loop {
        if cown_wait_cancelled() {
            for (_, _, state, _) in &ordered {
                unsafe { (**state).lease.notify_if_available() };
            }
            return 0;
        }
        let mut acquired = 0;
        for (_, _, state, _) in &ordered {
            if !unsafe { (**state).lease.try_acquire() } {
                break;
            }
            acquired += 1;
        }
        if acquired == ordered.len() {
            let output = unsafe { std::slice::from_raw_parts_mut(payloads, count) };
            for (_, index, state, _) in &ordered {
                output[*index] = unsafe { (**state).payload };
            }
            register_current_cown_leases(handles);
            return 1;
        }
        for (_, _, state, _) in ordered.iter().take(acquired) {
            unsafe { (**state).lease.release_quiet() };
        }
        if !crate::runtime::task::help_current_worker() {
            unsafe { (*ordered[acquired].2).lease.wait() };
        }
    }
}

pub(crate) extern "C" fn jk_cown_payload(cown: *mut u8) -> *mut u8 {
    let Some(header) = (unsafe { valid_header(cown_pointer(cown)) }) else {
        return std::ptr::null_mut();
    };
    if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::Cown) {
        return std::ptr::null_mut();
    }
    unsafe { (*cown_pointer(cown).cast::<CownState>()).payload }
}

/// Cleanup entry point used by the task runtime. It does not consult the
/// task-local registry because that registry is being drained already.
pub(crate) extern "C" fn jk_cown_release_cleanup(cown: *mut u8) {
    let Some(header) = (unsafe { valid_header(cown_pointer(cown)) }) else {
        return;
    };
    if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::Cown) {
        return;
    }
    let state = cown_pointer(cown).cast::<CownState>();
    unsafe { (*state).lease.release() };
    jk_drop(cown);
}

/// A false guard retains the caller's Cown capability and removes only the
/// task registry's lease capability. Registration precedes every quiet release.
pub(crate) fn release_cown_for_condition(cown: *mut u8) {
    if let Some(context) = crate::runtime::task::current_task_context() {
        if crate::runtime::task::unregister_cown_lease(context, cown) {
            jk_drop(cown);
        }
    }
    unsafe { cown_lease(cown).release_quiet() };
}

/// Leave a Cown lease and consume the capability retained for the release.
pub(crate) extern "C" fn jk_cown_release(cown: *mut u8) {
    let Some(header) = (unsafe { valid_header(cown_pointer(cown)) }) else {
        return;
    };
    if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::Cown) {
        return;
    }
    let state = cown_pointer(cown).cast::<CownState>();
    if let Some(context) = crate::runtime::task::current_task_context() {
        if crate::runtime::task::unregister_cown_lease(context, cown) {
            // Remove the capability retained by the cleanup registry. The
            // caller's capability is consumed separately below.
            jk_drop(cown);
        }
    }
    unsafe { (*state).lease.release() };
    jk_drop(cown);
}

pub(crate) extern "C" fn jk_dup(object: *mut u8) -> *mut u8 {
    if object.addr() & 1 != 0 {
        return object;
    }
    let Some(header) = (unsafe { valid_header(object) }) else {
        return std::ptr::null_mut();
    };
    if header.ownership != OwnershipKind::Shared as u8 {
        return std::ptr::null_mut();
    }
    let mut count = header.ref_count.load(Ordering::Relaxed);
    loop {
        if count == usize::MAX {
            return std::ptr::null_mut();
        }
        match header.ref_count.compare_exchange_weak(
            count,
            count + 1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return object,
            Err(current) => count = current,
        }
    }
}

pub(crate) extern "C" fn jk_drop(object: *mut u8) {
    if object.addr() & 1 != 0 {
        return;
    }
    let Some(header) = (unsafe { valid_header(object) }) else {
        return;
    };
    if header.ownership == OwnershipKind::Shared as u8
        && header.ref_count.fetch_sub(1, Ordering::AcqRel) != 1
    {
        return;
    }
    if header.kind == RuntimeValueKind::List as u8 {
        list::drop_list_object(object, header);
    }
    if header.kind == RuntimeValueKind::Map as u8 {
        map::drop_map_object(object, header);
    }
    if header.kind == RuntimeValueKind::MutList as u8 {
        unsafe { mut_list::drop_mut_list(object, header) };
    }
    if header.kind == RuntimeValueKind::MutMap as u8 {
        unsafe { mut_map::drop_mut_map(object, header) };
    }
    if header.kind == RuntimeValueKind::MutBytes as u8 {
        unsafe { bytes::drop_mut_bytes(object, header) };
    }
    if let Some(drop_callback) = header.drop_callback {
        unsafe { drop_callback(object) };
    }
    let layout = match Layout::from_size_align(header.allocation_size, header.allocation_align) {
        Ok(layout) => layout,
        Err(_) => return,
    };
    let header_pointer = unsafe {
        object
            .sub(size_of::<*mut ObjectHeader>())
            .cast::<*mut ObjectHeader>()
            .read()
    };
    #[cfg(any(test, feature = "test-support"))]
    // Remove the address before returning it to the allocator. Otherwise
    // an immediately reused payload can be inserted by another test
    // thread and then accidentally removed by this old destructor.
    if object_tracking_enabled() {
        if let Some(owners) = live_payloads()
            .lock()
            .expect("managed payload registry mutex")
            .remove(&(object as usize))
        {
            owners.thread.fetch_sub(1, Ordering::Release);
            owners.scope.fetch_sub(1, Ordering::Release);
        }
    }
    unsafe { alloc::dealloc(header_pointer.cast::<u8>(), layout) };
}

pub(crate) fn mask_count(word_count: usize) -> Option<usize> {
    word_count.checked_add(63).map(|count| count / 64)
}

fn is_managed_word(masks: &[u64], index: usize) -> bool {
    masks[index / 64] & (1_u64 << (index % 64)) != 0
}

pub(crate) fn drop_list_words(words: &[u64], masks: &[u64]) {
    for (index, word) in words.iter().enumerate() {
        if is_managed_word(masks, index) {
            jk_drop(*word as *mut u8);
        }
    }
}

pub(crate) fn clone_managed_words(words: &[u64], masks: &[u64], offset: usize) -> Option<Vec<u64>> {
    let mut cloned = words.to_vec();
    let mut duplicated = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if !is_managed_word(masks, offset + index) || *word == 0 {
            continue;
        }
        let pointer = jk_dup(*word as *mut u8);
        if pointer.is_null() {
            for index in duplicated {
                jk_drop(cloned[index] as *mut u8);
            }
            return None;
        }
        cloned[index] = pointer as u64;
        duplicated.push(index);
    }
    Some(cloned)
}

#[cfg(test)]
#[path = "managed/tests.rs"]
mod tests;
