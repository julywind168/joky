//! An uninstalled language value retains a worker cleanup obligation.
use super::*;

pub(super) type Work = Box<dyn FnOnce() -> Result<(), Error> + Send>;

/// A return belongs to its consumer's task; otherwise waiting for the producer
/// to finish before taking its result would deadlock its cleanup barrier.
pub(crate) struct CleanupReceipt {
    task: Arc<Task>,
    _charge: Lease,
}
impl CleanupReceipt {
    pub(crate) fn new(returned: bool) -> Result<Self, Error> {
        let task = CURRENT
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| invalid("Portable cleanup requires a worker task"))?;
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
            _charge: charge,
        })
    }

    pub(crate) fn defer(self, work: Work) {
        let task = self.task.clone();
        lock(&task.cleanups).push(Box::new(move || {
            let _receipt = self;
            work()
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
