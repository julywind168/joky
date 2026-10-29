//! An uninstalled language value retains a worker cleanup obligation.
use super::*;

pub(super) type Work = Box<dyn FnOnce() -> Result<(), Error> + Send>;

thread_local! { static CAPTURE: RefCell<Option<Arc<Arbiter>>> = const { RefCell::new(None) }; }
/// Bind captures prepared before publication to their recoverable group too.
/// The guard stays on the source worker; only immutable arbitration crosses.
pub(crate) struct CaptureGuard(Option<Arc<Arbiter>>);
impl CaptureGuard {
    pub(super) fn enter(boundary: Option<Arc<Arbiter>>) -> Self {
        Self(CAPTURE.with(|slot| slot.replace(boundary)))
    }
}
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE.with(|slot| slot.replace(self.0.take()));
    }
}

/// A return belongs to its consumer's task; otherwise waiting for the producer
/// to finish before taking its result would deadlock its cleanup barrier.
pub(crate) struct CleanupReceipt {
    task: Arc<Task>,
    boundary: Option<Arc<Arbiter>>,
    _charge: Lease,
}
impl CleanupReceipt {
    pub(crate) fn new(returned: bool) -> Result<Self, Error> {
        let task = CURRENT
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| invalid("Portable cleanup requires a worker task"))?;
        let boundary = if returned {
            task.group
                .as_ref()
                .filter(|(arbiter, _)| !arbiter.propagate_failure)
                .map(|(arbiter, _)| arbiter.clone())
        } else {
            CAPTURE.with(|slot| slot.borrow().clone())
        };
        let task = if returned {
            task.parent
                .as_ref()
                .and_then(Weak::upgrade)
                .ok_or_else(|| invalid("Host results cannot retain language destructors"))?
        } else {
            task
        };
        let charge = task
            .root
            .accounts()
            .reserve(std::mem::size_of::<Self>() + 128)?;
        task.cleanups_pending.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            task,
            boundary,
            _charge: charge,
        })
    }

    pub(crate) fn defer(self, work: Work) {
        let task = self.task.clone();
        lock(&task.cleanups).push(Box::new(move || {
            let receipt = self;
            match (work(), &receipt.boundary) {
                (Err(error), Some(boundary)) => {
                    boundary.cleanup_failed(error);
                    Ok(())
                }
                (result, _) => result,
            }
        }));
        task.wake();
    }
}
impl Drop for CleanupReceipt {
    fn drop(&mut self) {
        self.task.cleanups_pending.fetch_sub(1, Ordering::Release);
        self.task.wake();
    }
}

pub(super) fn drain(task: &Arc<Task>) -> Result<(), Error> {
    let _cleaning = CleanupGuard::enter();
    let mut failure = None;
    loop {
        let work = std::mem::take(&mut *lock(&task.cleanups));
        if work.is_empty() {
            break;
        }
        for work in work {
            if let Err(error) = work() {
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}
