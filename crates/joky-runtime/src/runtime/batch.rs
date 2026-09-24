//! Shared, scope-local iteration state. Inputs are claimed under a short lock;
//! user code and Pending I/O always execute after releasing it.
use super::managed::{self, jk_drop, RuntimeValueKind};
use std::sync::Mutex;

struct Batch {
    state: Mutex<State>,
}

struct State {
    cursor: usize,
    next_index: u64,
    workers_left: u64,
    results: Vec<(u64, usize)>,
}

impl Drop for State {
    fn drop(&mut self) {
        jk_drop(self.cursor as *mut u8);
        for (_, value) in self.results.drain(..) {
            jk_drop(value as *mut u8);
        }
    }
}

unsafe extern "C" fn drop_batch(pointer: *mut u8) {
    unsafe { pointer.cast::<Batch>().drop_in_place() };
}

unsafe fn batch<'a>(pointer: *mut u8) -> &'a Batch {
    unsafe { &*pointer.cast::<Batch>() }
}

/// Takes ownership of input. Zero dynamic limits fail the current task/run.
pub(crate) extern "C" fn jk_batch_new(input: *mut u8, limit: u64) -> *mut u8 {
    if limit == 0 {
        let message = b"parallel limit must be greater than zero";
        super::io::jk_panic(message.as_ptr(), message.len());
        super::scope::current_or_default().record_function_failure(2);
        super::task::jk_task_abort(u64::MAX - 1);
    }
    let workers = limit.min(super::list::jk_list_length(input) as u64);
    let pointer = managed::allocate_object(
        RuntimeValueKind::Batch,
        std::mem::size_of::<Batch>(),
        std::mem::align_of::<Batch>(),
        Some(drop_batch),
    );
    if pointer.is_null() {
        std::alloc::handle_alloc_error(std::alloc::Layout::new::<Batch>());
    }
    unsafe {
        pointer.cast::<Batch>().write(Batch {
            state: Mutex::new(State {
                cursor: input as usize,
                next_index: 0,
                workers_left: workers,
                results: Vec::new(),
            }),
        });
    }
    pointer
}

pub(crate) unsafe extern "C" fn jk_batch_worker(pointer: *mut u8) -> u8 {
    let mut state = unsafe { batch(pointer) }.state.lock().expect("batch mutex");
    let available = state.workers_left != 0;
    state.workers_left = state.workers_left.saturating_sub(1);
    u8::from(available)
}

/// Returns an owned reference to the claimed List node, or null at exhaustion.
/// The node keeps its element alive until generated code extracts the head.
pub(crate) unsafe extern "C" fn jk_batch_next(pointer: *mut u8, index: *mut u64) -> *mut u8 {
    unsafe { index.write(0) };
    if super::task::current_task_context().is_some_and(super::task::task_context_is_cancelled) {
        return std::ptr::null_mut();
    }
    let mut state = unsafe { batch(pointer) }.state.lock().expect("batch mutex");
    let node = state.cursor as *mut u8;
    if node.is_null() {
        return node;
    }
    unsafe { index.write(state.next_index) };
    state.next_index = state
        .next_index
        .checked_add(1)
        .expect("iteration index exhausted");
    state.cursor = super::list::jk_list_tail(node) as usize;
    node
}

/// Takes a singleton result List. Skipped items create no result metadata.
pub(crate) unsafe extern "C" fn jk_batch_push(pointer: *mut u8, index: u64, result: *mut u8) {
    unsafe { batch(pointer) }
        .state
        .lock()
        .expect("batch mutex")
        .results
        .push((index, result as usize));
}

/// Called only after all workers drain; returns results in input order.
pub(crate) unsafe extern "C" fn jk_batch_finish(pointer: *mut u8) -> *mut u8 {
    let mut results = std::mem::take(
        &mut unsafe { batch(pointer) }
            .state
            .lock()
            .expect("batch mutex")
            .results,
    );
    results.sort_unstable_by_key(|(index, _)| *index);
    let mut output = std::ptr::null_mut();
    for (_, node) in results.into_iter().rev() {
        // Transfer each singleton into the output by linking its currently
        // empty tail. No payload clone or JIT drop callback is needed.
        unsafe { super::list::prepend_singleton(node as *mut u8, output) };
        output = node as *mut u8;
    }
    output
}
