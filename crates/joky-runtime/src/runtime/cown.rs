//! Per-Cown contention queues. The atomic word joins the lease bit and waiter
//! publication, so release cannot miss a waiter between its check and parking.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, Thread};

const LEASED: u8 = 1;
const WAITERS: u8 = 2;
const CONDITION_WAITERS: u8 = 4;

pub(crate) struct Waiter {
    notified: AtomicBool,
    target: WakeTarget,
}

enum WakeTarget {
    Thread(Thread),
    Continuation(Box<dyn Fn(usize) + Send + Sync>),
}

impl Waiter {
    fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
            target: WakeTarget::Thread(thread::current()),
        }
    }

    pub(crate) fn for_continuation(wake: impl Fn(usize) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            notified: AtomicBool::new(false),
            target: WakeTarget::Continuation(Box::new(wake)),
        })
    }

    pub(crate) fn notify(&self) {
        match &self.target {
            WakeTarget::Thread(thread) => {
                self.notified.store(true, Ordering::Release);
                thread.unpark();
            }
            WakeTarget::Continuation(wake) => {
                if !self.notified.swap(true, Ordering::AcqRel) {
                    wake(self as *const Self as usize);
                }
            }
        }
    }

    fn wait(&self) {
        // Scheduler enqueues and an outer waiter on this same thread can also
        // unpark us. Retry acquisition after any park return: a selected outer
        // waiter cannot resume until its helped task has finished.
        while !self.notified.load(Ordering::Acquire) {
            let _helper = super::task::register_cown_helper();
            if !self.notified.load(Ordering::Acquire) && !super::task::help_current_worker() {
                thread::park();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn false_guards_park_until_a_completed_lease_and_cancel_cleanly() {
        let lease = Lease::default();
        let first = Arc::new(Waiter::new());
        let second = Arc::new(Waiter::new());
        assert!(lease.try_acquire());
        lease.register_condition(&first);
        lease.release_quiet();
        assert!(!first.notified.load(Ordering::Acquire));
        assert!(lease.try_acquire());
        lease.register_condition(&second);
        lease.release_quiet();
        assert!(!first.notified.load(Ordering::Acquire));
        assert!(!second.notified.load(Ordering::Acquire));
        lease.unregister_condition(&first);
        assert!(lease.try_acquire());
        lease.release();
        assert!(!first.notified.load(Ordering::Acquire));
        assert!(second.notified.load(Ordering::Acquire));
        assert!(lease.condition_waiters.lock().unwrap().is_empty());
    }

    #[test]
    fn release_before_registration_or_park_is_not_lost() {
        let lease = Lease::default();
        assert!(lease.try_acquire());
        lease.release();
        let waiter = Arc::new(Waiter::new());
        assert!(!lease.register(&waiter));
        assert!(lease.try_acquire());
        assert!(lease.register(&waiter));
        lease.release();
        assert!(waiter.notified.load(Ordering::Acquire));
        waiter.wait();
        lease.unregister(&waiter, false);
        assert_eq!(lease.waiter_count(), 0);
        assert_eq!(lease.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cancelled_selected_waiter_passes_the_available_lease_to_the_next() {
        let lease = Lease::default();
        assert!(lease.try_acquire());
        let first = Arc::new(Waiter::new());
        let second = Arc::new(Waiter::new());
        assert!(lease.register(&first));
        assert!(lease.register(&second));
        lease.release();
        lease.unregister(&first, true);
        assert!(second.notified.load(Ordering::Acquire));
        lease.unregister(&second, false);
        assert_eq!(lease.waiter_count(), 0);
        assert!(lease.try_acquire());
        lease.release();
    }

    #[test]
    fn cancellation_after_leaving_the_queue_notifies_the_next_waiter() {
        let lease = Lease::default();
        assert!(lease.try_acquire());
        let first = Arc::new(Waiter::new());
        let second = Arc::new(Waiter::new());
        assert!(lease.register(&first));
        assert!(lease.register(&second));
        lease.release();
        lease.unregister(&first, false);
        assert!(!second.notified.load(Ordering::Acquire));
        // The acquire retry observes cancellation after wait() has returned.
        lease.notify_if_available();
        assert!(second.notified.load(Ordering::Acquire));
        lease.unregister(&second, false);
        assert_eq!(lease.waiter_count(), 0);
    }

    #[test]
    fn outer_waiter_notification_allows_inner_waiter_to_retry() {
        let (done, completion) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let lease = Lease::default();
            assert!(lease.try_acquire());
            let outer = Arc::new(Waiter::new());
            let inner = Arc::new(Waiter::new());
            assert!(lease.register(&outer));
            assert!(lease.register(&inner));
            lease.release();
            assert!(outer.notified.load(Ordering::Acquire));
            assert!(!inner.notified.load(Ordering::Acquire));
            inner.wait();
            lease.unregister(&inner, false);
            assert!(lease.try_acquire());
            lease.release();
            lease.unregister(&outer, false);
            done.send(()).unwrap();
        });
        completion
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn release_selects_one_waiter_and_does_not_wake_another_cown() {
        let lease = Lease::default();
        let other = Lease::default();
        assert!(lease.try_acquire());
        assert!(other.try_acquire());
        let first = Arc::new(Waiter::new());
        let second = Arc::new(Waiter::new());
        let unrelated = Arc::new(Waiter::new());
        assert!(lease.register(&first));
        assert!(lease.register(&second));
        assert!(other.register(&unrelated));
        lease.release();
        assert!(first.notified.load(Ordering::Acquire));
        assert!(!second.notified.load(Ordering::Acquire));
        assert!(!unrelated.notified.load(Ordering::Acquire));
        lease.unregister(&first, false);
        assert!(!second.notified.load(Ordering::Acquire));
        assert!(lease.try_acquire());
        lease.release();
        assert!(second.notified.load(Ordering::Acquire));
        lease.unregister(&second, false);
        other.release();
        assert!(unrelated.notified.load(Ordering::Acquire));
        other.unregister(&unrelated, false);
    }
}

#[derive(Default)]
pub(crate) struct Lease {
    state: AtomicU8,
    waiters: Mutex<VecDeque<Arc<Waiter>>>,
    condition_waiters: Mutex<Vec<Arc<Waiter>>>,
}

impl Lease {
    pub(crate) fn try_acquire(&self) -> bool {
        self.state
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |state| {
                (state & LEASED == 0).then_some(state | LEASED)
            })
            .is_ok()
    }

    pub(crate) fn is_leased(&self) -> bool {
        self.state.load(Ordering::Acquire) & LEASED != 0
    }

    pub(crate) fn release(&self) {
        // Capture the completed lease's observers before allowing another lease.
        // A false guard uses release_quiet and never wakes itself or other guards.
        let observers = if self.state.load(Ordering::Acquire) & CONDITION_WAITERS != 0 {
            let mut waiters = self.condition_waiters.lock().expect("Cown conditions");
            self.state.fetch_and(!CONDITION_WAITERS, Ordering::Relaxed);
            std::mem::take(&mut *waiters)
        } else {
            Vec::new()
        };
        self.release_quiet();
        for waiter in observers {
            waiter.notify();
        }
    }

    pub(crate) fn register_condition(&self, waiter: &Arc<Waiter>) {
        debug_assert!(self.is_leased());
        let mut waiters = self.condition_waiters.lock().expect("Cown conditions");
        waiters.push(Arc::clone(waiter));
        self.state.fetch_or(CONDITION_WAITERS, Ordering::Release);
    }

    pub(crate) fn unregister_condition(&self, waiter: &Arc<Waiter>) {
        let mut waiters = self.condition_waiters.lock().expect("Cown conditions");
        waiters.retain(|entry| !Arc::ptr_eq(entry, waiter));
        if waiters.is_empty() {
            self.state.fetch_and(!CONDITION_WAITERS, Ordering::Relaxed);
        }
    }

    pub(crate) fn release_quiet(&self) {
        if self.state.fetch_and(!LEASED, Ordering::Release) & WAITERS != 0 {
            self.notify_next();
        }
    }

    pub(crate) fn notify_if_available(&self) {
        if !self.is_leased() {
            self.notify_next();
        }
    }

    fn notify_next(&self) {
        let waiter = {
            let mut waiters = self.waiters.lock().expect("Cown waiters");
            let next = waiters.pop_front();
            if waiters.is_empty() {
                self.state.fetch_and(!WAITERS, Ordering::Relaxed);
            }
            next
        };
        if let Some(waiter) = waiter {
            waiter.notify();
        }
    }

    pub(crate) fn register(&self, waiter: &Arc<Waiter>) -> bool {
        let mut waiters = self.waiters.lock().expect("Cown waiters");
        // Release either sees WAITERS and takes this mutex after insertion,
        // or precedes this RMW and we observe an available lease immediately.
        if self.state.fetch_or(WAITERS, Ordering::AcqRel) & LEASED == 0 {
            if waiters.is_empty() {
                self.state.fetch_and(!WAITERS, Ordering::Relaxed);
            }
            return false;
        }
        waiters.push_back(Arc::clone(waiter));
        true
    }

    pub(crate) fn unregister(&self, waiter: &Arc<Waiter>, abandoned: bool) {
        {
            let mut waiters = self.waiters.lock().expect("Cown waiters");
            waiters.retain(|entry| !Arc::ptr_eq(entry, waiter));
            if waiters.is_empty() {
                self.state.fetch_and(!WAITERS, Ordering::Relaxed);
            }
        }
        // A selected waiter can be cancelled before acquiring. Do not strand
        // the rest of the queue when nobody owns the Cown to release it again.
        if abandoned && !self.is_leased() {
            self.notify_next();
        }
    }

    pub(crate) fn wait(&self) {
        let waiter = Arc::new(Waiter::new());
        let _cancellation = super::task::register_cown_waiter(&waiter);
        if !self.register(&waiter) {
            return;
        }
        struct Registration<'a>(&'a Lease, Arc<Waiter>);
        impl Drop for Registration<'_> {
            fn drop(&mut self) {
                let cancelled = super::task::current_task_context()
                    .is_some_and(super::task::task_context_is_cancelled);
                self.0.unregister(&self.1, cancelled || thread::panicking());
            }
        }
        let _registration = Registration(self, Arc::clone(&waiter));
        waiter.wait();
    }

    #[cfg(test)]
    pub(crate) fn condition_waiter_count(&self) -> usize {
        self.condition_waiters.lock().unwrap().len()
    }

    #[cfg(test)]
    pub(crate) fn waiter_count(&self) -> usize {
        self.waiters.lock().unwrap().len()
    }
}
