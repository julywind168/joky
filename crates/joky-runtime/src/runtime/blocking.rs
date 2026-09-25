//! Fixed blocking workers with asynchronous FIFO admission.
//!
//! The runnable queue is bounded. Excess provider requests remain suspended
//! until a dequeue or cancellation makes room; no CPU worker waits for capacity.
//! Waiting entries must retain only small request descriptors/shared arguments,
//! not allocate I/O buffers. Their count follows the number of suspended callers.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;

type BlockingJob = Box<dyn FnOnce() + Send + 'static>;
pub(crate) type RequestKey = (usize, u64);
const BLOCKING_QUEUE_CAPACITY: usize = 1024;

struct Request {
    #[cfg(any(test, feature = "test-support"))]
    scope: crate::runtime::scope::ScopeId,
    key: Option<RequestKey>,
    job: BlockingJob,
}

struct Queues {
    ready: VecDeque<Request>,
    waiting: VecDeque<Request>,
    // Includes runnable, waiting-for-admission and currently executing jobs.
    pending: usize,
}

struct BlockingState {
    queues: Mutex<Queues>,
    wake: Condvar,
    idle: Condvar,
    capacity: usize,
}

impl BlockingState {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            queues: Mutex::new(Queues {
                ready: VecDeque::new(),
                waiting: VecDeque::new(),
                pending: 0,
            }),
            wake: Condvar::new(),
            idle: Condvar::new(),
            capacity,
        }
    }

    fn admit(&self, queues: &mut Queues) {
        while queues.ready.len() < self.capacity {
            let Some(request) = queues.waiting.pop_front() else {
                break;
            };
            queues.ready.push_back(request);
        }
    }

    fn submit(&self, request: Request, cancelled: impl FnOnce() -> bool) {
        let mut queues = self.queues.lock().expect("blocking queues mutex");
        // Serialize registration against the provider cancellation hook. If
        // cancellation ran before registration, its persistent flag wins here.
        if cancelled() {
            drop(queues);
            drop(request);
            return;
        }
        queues.pending += 1;
        queues.waiting.push_back(request);
        self.admit(&mut queues);
        self.wake.notify_one();
    }

    fn pop(&self, queues: &mut Queues) -> Option<Request> {
        let request = queues.ready.pop_front();
        self.admit(queues);
        request
    }

    fn finished(&self) {
        let mut queues = self.queues.lock().expect("blocking queues mutex");
        queues.pending -= 1;
        if queues.pending == 0 {
            self.idle.notify_all();
        }
    }

    fn cancel(&self, key: RequestKey) {
        let mut queues = self.queues.lock().expect("blocking queues mutex");
        let removed = if let Some(index) = queues.ready.iter().position(|r| r.key == Some(key)) {
            queues.ready.remove(index)
        } else if let Some(index) = queues.waiting.iter().position(|r| r.key == Some(key)) {
            queues.waiting.remove(index)
        } else {
            None
        };
        if removed.is_some() {
            self.admit(&mut queues);
            self.wake.notify_one();
        }
        // Captures can own arbitrary destructors; release outside the mutex.
        drop(queues);
        if let Some(removed) = removed {
            drop(removed);
            self.finished();
        }
    }
}

struct BlockingPool {
    state: Arc<BlockingState>,
    #[cfg(test)]
    worker_count: usize,
}

static BLOCKING_POOL: OnceLock<BlockingPool> = OnceLock::new();

fn blocking_pool() -> &'static BlockingPool {
    BLOCKING_POOL.get_or_init(|| {
        let worker_count = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(2)
            .clamp(1, 8);
        let state = Arc::new(BlockingState::new(BLOCKING_QUEUE_CAPACITY));
        for index in 0..worker_count {
            let state = Arc::clone(&state);
            thread::Builder::new()
                .name(format!("joky-blocking-{index}"))
                .spawn(move || blocking_worker_loop(state))
                .expect("failed to start Joky blocking worker");
        }
        BlockingPool {
            state,
            #[cfg(test)]
            worker_count,
        }
    })
}

fn blocking_worker_loop(state: Arc<BlockingState>) {
    loop {
        let request = {
            let mut queues = state.queues.lock().expect("blocking queues mutex");
            while queues.ready.is_empty() {
                queues = state.wake.wait(queues).expect("blocking queues mutex");
            }
            state.pop(&mut queues).expect("blocking queue is non-empty")
        };
        (request.job)();
        state.finished();
    }
}

/// Submit a provider request, retaining its continuation while it awaits
/// admission. The caller supplies the persistent cancellation check and must
/// call `cancel` from its provider cancellation hook with the same key.
pub(crate) fn enqueue(
    key: RequestKey,
    cancelled: impl FnOnce() -> bool,
    job: impl FnOnce() + Send + 'static,
) {
    blocking_pool().state.submit(
        Request {
            #[cfg(any(test, feature = "test-support"))]
            scope: crate::runtime::scope::current_id(),
            key: Some(key),
            job: Box::new(job),
        },
        cancelled,
    );
}

/// Remove waiting/queued work immediately. A dequeued syscall owns its captures
/// until it returns; the provider discards its result if cancellation won.
pub(crate) fn cancel(key: RequestKey) {
    if let Some(pool) = BLOCKING_POOL.get() {
        pool.state.cancel(key);
    }
}

/// Native-resource destruction cannot be cancelled or run on a CPU worker.
pub(crate) fn enqueue_cleanup(job: impl FnOnce() + Send + 'static) {
    blocking_pool().state.submit(
        Request {
            #[cfg(any(test, feature = "test-support"))]
            scope: crate::runtime::scope::current_id(),
            key: None,
            job: Box::new(job),
        },
        || false,
    );
}

#[cfg(test)]
fn enqueue_test(job: impl FnOnce() + Send + 'static) {
    blocking_pool().state.submit(
        Request {
            #[cfg(test)]
            scope: crate::runtime::scope::current_id(),
            key: None,
            job: Box::new(job),
        },
        || false,
    );
}

#[cfg(test)]
fn wait_for_idle() {
    let Some(pool) = BLOCKING_POOL.get() else {
        return;
    };
    let mut queues = pool.state.queues.lock().expect("blocking queues mutex");
    while queues.pending != 0 {
        queues = pool.state.idle.wait(queues).expect("blocking queues mutex");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Barrier};
    use std::time::Duration;

    static BLOCKING_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn request(id: usize, job: impl FnOnce() + Send + 'static) -> Request {
        Request {
            #[cfg(test)]
            scope: crate::runtime::scope::current_id(),
            key: Some((id, 0)),
            job: Box::new(job),
        }
    }

    fn drain(state: &BlockingState) {
        loop {
            let next = state.pop(&mut state.queues.lock().unwrap());
            let Some(next) = next else { break };
            (next.job)();
            state.finished();
        }
    }

    #[test]
    fn executes_jobs_on_reusable_blocking_workers() {
        let _guard = BLOCKING_TEST_LOCK.lock().unwrap();
        let completed = Arc::new(AtomicUsize::new(0));
        for _ in 0..32 {
            let completed = Arc::clone(&completed);
            enqueue_test(move || {
                completed.fetch_add(1, Ordering::AcqRel);
            });
        }
        wait_for_idle();
        assert_eq!(completed.load(Ordering::Acquire), 32);
    }

    #[test]
    fn admission_is_fifo_and_cancellation_immediately_reclaims_capacity() {
        let state = Arc::new(BlockingState::new(1));
        let order = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::new(());
        for id in 0..5 {
            let order = Arc::clone(&order);
            let capture = Arc::clone(&capture);
            state.submit(
                request(id, move || {
                    order.lock().unwrap().push(id);
                    drop(capture);
                }),
                || false,
            );
        }
        assert_eq!(Arc::strong_count(&capture), 6);
        assert_eq!(state.queues.lock().unwrap().ready.len(), 1);
        assert_eq!(state.queues.lock().unwrap().waiting.len(), 4);
        state.cancel((2, 0)); // Waiting request releases captures immediately.
        state.cancel((0, 0)); // Runnable cancellation admits the oldest waiter.
        assert_eq!(Arc::strong_count(&capture), 4);
        assert_eq!(
            state.queues.lock().unwrap().ready.front().unwrap().key,
            Some((1, 0))
        );
        let producer = Arc::clone(&state);
        let producer_order = Arc::clone(&order);
        let (sent, received) = mpsc::channel();
        crate::runtime::task::enqueue_scheduler_callback(Box::new(move || {
            producer.submit(
                request(5, move || producer_order.lock().unwrap().push(5)),
                || false,
            );
            sent.send(()).unwrap();
        }));
        received
            .recv_timeout(Duration::from_secs(5))
            .expect("CPU worker must not block on admission");
        drain(&state);
        assert_eq!(*order.lock().unwrap(), [1, 3, 4, 5]);
        assert_eq!(state.queues.lock().unwrap().pending, 0);
        assert_eq!(Arc::strong_count(&capture), 1);
    }

    #[test]
    fn cancellation_racing_registration_cannot_leave_a_waiter_or_capture() {
        for _ in 0..64 {
            let state = Arc::new(BlockingState::new(1));
            state.submit(request(0, || {}), || false);
            let cancelled = Arc::new(AtomicBool::new(false));
            let barrier = Arc::new(Barrier::new(2));
            let capture = Arc::new(());
            let producer_state = Arc::clone(&state);
            let producer_cancelled = Arc::clone(&cancelled);
            let producer_barrier = Arc::clone(&barrier);
            let captured = Arc::clone(&capture);
            let producer = thread::spawn(move || {
                producer_barrier.wait();
                producer_state.submit(request(1, move || drop(captured)), || {
                    producer_cancelled.load(Ordering::Acquire)
                });
            });
            barrier.wait();
            cancelled.store(true, Ordering::Release);
            state.cancel((1, 0));
            producer.join().unwrap();
            assert_eq!(Arc::strong_count(&capture), 1);
            assert!(state.queues.lock().unwrap().waiting.is_empty());
            assert_eq!(state.queues.lock().unwrap().pending, 1);
            drain(&state);
        }
    }

    #[test]
    fn cancelled_request_destructors_run_outside_the_queue_lock() {
        struct Capture(Arc<BlockingState>, Arc<AtomicUsize>);
        impl Drop for Capture {
            fn drop(&mut self) {
                assert!(self.0.queues.try_lock().is_ok());
                self.1.fetch_add(1, Ordering::Relaxed);
            }
        }
        let state = Arc::new(BlockingState::new(1));
        let drops = Arc::new(AtomicUsize::new(0));
        for id in 0..3 {
            let capture = Capture(Arc::clone(&state), Arc::clone(&drops));
            state.submit(request(id, move || drop(capture)), || id == 2);
        }
        state.cancel((1, 0));
        state.cancel((0, 0));
        assert_eq!(drops.load(Ordering::Relaxed), 3);
        assert_eq!(state.queues.lock().unwrap().pending, 0);
    }

    struct ReleaseWorkers(Arc<(Mutex<bool>, Condvar)>);
    impl Drop for ReleaseWorkers {
        fn drop(&mut self) {
            let (released, wake) = &*self.0;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }
    }

    #[test]
    #[ignore = "saturates the process-wide pool; run alone with --ignored --exact"]
    fn sqlite_backpressure_cancels_every_operation_before_execution() {
        use crate::runtime::continuation::*;
        use crate::runtime::managed::{jk_drop, live_object_count};
        use crate::runtime::sqlite;
        use std::ffi::c_void;

        let _guard = BLOCKING_TEST_LOCK.lock().unwrap();
        let pool = blocking_pool();
        let gate = ReleaseWorkers(Arc::new((Mutex::new(false), Condvar::new())));
        let (started, started_rx) = mpsc::channel();
        for _ in 0..pool.worker_count {
            let gate = Arc::clone(&gate.0);
            let started = started.clone();
            enqueue_test(move || {
                started.send(()).unwrap();
                let (released, wake) = &*gate;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            });
        }
        for _ in 0..pool.worker_count {
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let mut registrations = Vec::new();
        // Slot order mirrors the provider's name-keyed hook table.
        let names = [
            "open",
            "prepare",
            "execute",
            "bind_text",
            "bind_i64",
            "finalize",
            "close",
            "bind_null",
            "bind_f64",
            "bind_blob",
            "query",
            "step",
        ];
        for saturated in [false, true] {
            if saturated {
                for _ in 0..pool.state.capacity {
                    enqueue_test(|| {});
                }
            }
            for (slot, name) in names.iter().enumerate() {
                let scope = crate::runtime::scope::RuntimeScope::new();
                let _scope_guard = scope.enter();
                let operation = 40 + slot as u64;
                let entries = [crate::runtime::provider::ProviderOperationEntry {
                    effect: "sqlite",
                    name,
                    operation,
                }];
                registrations.push(sqlite::register_operations(&entries).unwrap());
                let db = sqlite::SqliteConnection::open(":memory:").unwrap();
                let statement = db.prepare("select ?").unwrap();
                let text = if slot == 0 { ":memory:" } else { "select 1" };
                let string = if matches!(slot, 0 | 1 | 3) {
                    crate::runtime::string::jk_string_from_utf8(text.as_ptr(), text.len())
                } else {
                    std::ptr::null_mut()
                };
                let pointer = match slot {
                    0 => std::ptr::null_mut(),
                    1 | 6 => sqlite::allocate_handle(Arc::clone(&db), scope.id()),
                    _ => sqlite::allocate_statement_handle(Arc::clone(&statement), scope.id()),
                };
                let blob = if slot == 9 {
                    crate::runtime::bytes::jk_bytes_from_data(b"blob".as_ptr(), 4)
                } else {
                    std::ptr::null_mut()
                };
                let arguments = match slot {
                    0 => vec![string as usize, text.len()],
                    1 => vec![pointer as usize, string as usize, text.len()],
                    3 => vec![pointer as usize, 1, string as usize, text.len()],
                    4 | 8 => vec![pointer as usize, 1, 42],
                    7 => vec![pointer as usize, 1],
                    9 => vec![pointer as usize, 1, blob as usize],
                    _ => vec![pointer as usize],
                };
                let owned_offsets: &[usize] = match slot {
                    0 => &[0],
                    1 => &[1],
                    3 | 9 => &[0, 2],
                    _ => &[0],
                };
                let word = size_of::<usize>();
                let result_words = match slot {
                    10 => 5,
                    11 => 6,
                    5 | 6 => 3,
                    _ => 4,
                };
                let handle = jk_continuation_new(1);
                unsafe {
                    jk_continuation_alloc_suspend_result(handle, result_words * word);
                    for offset in owned_offsets {
                        jk_continuation_register_cleanup(
                            handle,
                            joky_runtime_abi::CONTINUATION_SUSPEND_ARGUMENT_STORAGE,
                            offset * word,
                            jk_drop as *mut c_void,
                        );
                    }
                    assert_eq!(
                        jk_continuation_start_suspend_payload(
                            handle,
                            operation,
                            arguments.as_ptr().cast(),
                            arguments.len() * word
                        ),
                        1
                    );
                    assert_eq!(jk_continuation_cancel(handle), 1);
                    jk_continuation_free(handle);
                }
                if slot == 1 {
                    jk_drop(pointer);
                }
                let queued = crate::runtime::blocking::resource_snapshot(Some(scope.id()));
                assert_eq!(
                    (queued.scope_ready, queued.scope_waiting),
                    (0, 0),
                    "slot {slot}, saturated {saturated}"
                );
                assert_eq!(live_object_count(), 0);
                assert_eq!(Arc::strong_count(&statement), 1);
                assert_eq!(Arc::strong_count(&db), 2);
            }
        }
        drop(gate);
        wait_for_idle();
        drop(registrations);
    }

    #[test]
    #[ignore = "saturates the process-wide file pool; run alone with --ignored --exact"]
    fn file_backpressure_cancels_queued_requests_while_workers_are_occupied() {
        let _guard = BLOCKING_TEST_LOCK.lock().unwrap();
        let pool = blocking_pool();
        let live_before = crate::runtime::managed::live_object_count_all_threads();
        let gate = ReleaseWorkers(Arc::new((Mutex::new(false), Condvar::new())));
        let (started, started_rx) = mpsc::channel();
        for _ in 0..pool.worker_count {
            let gate = Arc::clone(&gate.0);
            let started = started.clone();
            enqueue_test(move || {
                started.send(()).unwrap();
                let (released, wake) = &*gate;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            });
        }
        for _ in 0..pool.worker_count {
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        // Cancellation releases queued and not-yet-admitted provider arguments
        // before any blocking worker is allowed to dequeue.
        for saturated in [false, true] {
            if saturated {
                for _ in 0..pool.state.capacity {
                    enqueue_test(|| {});
                }
            }
            for (index, name) in ["read", "read_bytes", "write", "write_bytes", "open"]
                .into_iter()
                .enumerate()
            {
                use crate::runtime::continuation::*;
                let scope = crate::runtime::scope::RuntimeScope::new();
                let _scope_guard = scope.enter();
                let operation = 60 + index as u64;
                let entries = [crate::runtime::provider::ProviderOperationEntry {
                    effect: "file",
                    name,
                    operation,
                }];
                let _provider = crate::runtime::file::register_operations(&entries).unwrap();
                let handle = jk_continuation_new(1);
                let path = b"cancelled-before-opening";
                let string = crate::runtime::string::jk_string_from_utf8(path.as_ptr(), path.len());
                let mut arguments = vec![string as usize, path.len()];
                let data = match name {
                    "write" => crate::runtime::string::jk_string_from_utf8(b"content".as_ptr(), 7),
                    "write_bytes" => {
                        crate::runtime::bytes::jk_bytes_from_data(b"content".as_ptr(), 7)
                    }
                    _ => std::ptr::null_mut(),
                };
                match name {
                    "write" => arguments.extend_from_slice(&[data as usize, 7]),
                    "write_bytes" => arguments.push(data as usize),
                    "open" => arguments.push(0),
                    _ => {}
                }
                unsafe {
                    assert!(!jk_continuation_alloc_suspend_result(
                        handle,
                        (if name == "read" { 5 } else { 4 }) * size_of::<usize>()
                    )
                    .is_null());
                    assert_eq!(
                        jk_continuation_start_suspend_payload(
                            handle,
                            operation,
                            arguments.as_ptr().cast(),
                            arguments.len() * size_of::<usize>()
                        ),
                        1
                    );
                }
                let resources = scope.resources.snapshot();
                assert_eq!(resources[3], 1, "provider must retain one file request");
                assert_eq!(
                    resources[4],
                    path.len() + if data.is_null() { 0 } else { 7 },
                    "shared path bytes must be visible"
                );
                let snapshot = super::resource_snapshot(Some(scope.id()));
                assert_eq!(snapshot.scope_waiting, usize::from(saturated));
                assert_eq!(snapshot.scope_ready, usize::from(!saturated));
                if saturated {
                    assert_eq!(pool.state.queues.lock().unwrap().waiting.len(), 1);
                } else {
                    assert_eq!(pool.state.queues.lock().unwrap().ready.len(), 1);
                }
                scope.close_and_wait();
                unsafe {
                    jk_continuation_free(handle);
                }
                crate::runtime::managed::jk_drop(string);
                crate::runtime::managed::jk_drop(data);
                assert!(pool.state.queues.lock().unwrap().waiting.is_empty());
                assert_eq!(
                    pool.state.queues.lock().unwrap().ready.len(),
                    if saturated { pool.state.capacity } else { 0 }
                );
                assert_eq!(
                    crate::runtime::managed::live_object_count_all_threads(),
                    live_before
                );
            }
        }

        drop(gate);
        wait_for_idle();
        assert_eq!(
            crate::runtime::managed::live_object_count_all_threads(),
            live_before
        );
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub(crate) struct ResourceSnapshot {
    pub capacity: usize,
    pub scope_ready: usize,
    pub scope_waiting: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn resource_snapshot(scope: Option<crate::runtime::scope::ScopeId>) -> ResourceSnapshot {
    let Some(pool) = BLOCKING_POOL.get() else {
        return ResourceSnapshot::default();
    };
    let queues = pool.state.queues.lock().unwrap();
    ResourceSnapshot {
        capacity: queues.ready.capacity() + queues.waiting.capacity(),
        scope_ready: queues
            .ready
            .iter()
            .filter(|r| scope.is_none_or(|id| r.scope == id))
            .count(),
        scope_waiting: queues
            .waiting
            .iter()
            .filter(|r| scope.is_none_or(|id| r.scope == id))
            .count(),
    }
}
