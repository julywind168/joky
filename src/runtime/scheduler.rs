//! Single-threaded cooperative scheduling with externally signaled readiness.
use super::suspension::{Continuation, ContinuationStatus, ResumeError};

/// Task priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Normal,
}

/// Queued task.
pub struct Task {
    /// Self-contained task entry.
    pub entry: Box<dyn FnOnce() + Send>,
    pub priority: Priority,
}

/// Runs queued work serially and retains pending continuations.
pub struct Scheduler {
    queue: Vec<Task>,
    suspended: Vec<Option<Continuation>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            queue: Vec::new(),
            suspended: Vec::new(),
        }
    }

    /// Enqueue a task.
    pub fn spawn(&mut self, task: Task) {
        self.queue.push(task);
    }

    /// Run all queued tasks in insertion order.
    pub fn run_all(&mut self) {
        let tasks = std::mem::take(&mut self.queue);
        for task in tasks {
            (task.entry)();
        }
    }

    /// Store a continuation and return its stable ID.
    pub fn suspend(&mut self, continuation: Continuation) -> usize {
        let id = self.suspended.len();
        self.suspended.push(Some(continuation));
        id
    }

    /// Poll a continuation on the current thread.
    pub fn resume(&mut self, id: usize) -> Result<(), SchedulerError> {
        let continuation = self
            .suspended
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(SchedulerError::UnknownContinuation(id))?;
        continuation.resume().map_err(SchedulerError::Resume)
    }

    pub fn continuation(&self, id: usize) -> Result<&Continuation, SchedulerError> {
        self.suspended
            .get(id)
            .and_then(Option::as_ref)
            .ok_or(SchedulerError::UnknownContinuation(id))
    }

    /// Take a successfully completed task. IDs are never reused.
    pub fn take_completed(&mut self, id: usize) -> Result<Continuation, SchedulerError> {
        match self.continuation(id)?.status() {
            ContinuationStatus::Pending => return Err(SchedulerError::NotCompleted(id)),
            ContinuationStatus::Failed(error) => return Err(SchedulerError::Resume(error)),
            ContinuationStatus::Completed => {}
        }
        Ok(self.suspended[id].take().unwrap())
    }

    /// Remove a failed task while retaining its original error for inspection.
    pub fn take_failed(&mut self, id: usize) -> Result<Continuation, SchedulerError> {
        if !matches!(
            self.continuation(id)?.status(),
            ContinuationStatus::Failed(_)
        ) {
            return Err(SchedulerError::NotFailed(id));
        }
        Ok(self.suspended[id].take().unwrap())
    }

    /// Drop the frame tree and release this task's code owners.
    pub fn cancel(&mut self, id: usize) -> Result<(), SchedulerError> {
        self.continuation(id)?;
        self.suspended[id] = None;
        Ok(())
    }

    pub fn suspended_count(&self) -> usize {
        self.suspended
            .iter()
            .flatten()
            .filter(|c| !c.is_resumed())
            .count()
    }

    /// Poll every pending continuation and return the number polled.
    pub fn resume_all(&mut self) -> Result<usize, SchedulerError> {
        let mut resumed = 0;
        for continuation in self.suspended.iter_mut().flatten() {
            if !continuation.is_resumed() {
                continuation.resume().map_err(SchedulerError::Resume)?;
                resumed += 1;
            }
        }
        Ok(resumed)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SchedulerError {
    UnknownContinuation(usize),
    Resume(ResumeError),
    NotCompleted(usize),
    NotFailed(usize),
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn runs_tasks_in_order() {
        let mut sched = Scheduler::new();
        let log = Arc::new(Mutex::new(Vec::<i32>::new()));

        for i in 0..3 {
            let log = Arc::clone(&log);
            sched.spawn(Task {
                entry: Box::new(move || log.lock().unwrap().push(i)),
                priority: Priority::Normal,
            });
        }

        sched.run_all();
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn native_continuation_resumes_once() {
        use super::super::suspension::{Continuation, ModuleId};
        // A static counter allows a plain function pointer to record execution.
        static HIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn resume() {
            HIT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let mut sched = Scheduler::new();
        let id = sched.suspend(Continuation::new_native(resume, ModuleId(1)));
        assert_eq!(sched.suspended_count(), 1);
        sched.resume(id).unwrap();
        assert_eq!(HIT.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(sched.suspended_count(), 0);
        assert_eq!(
            sched.resume(id),
            Err(SchedulerError::Resume(ResumeError::AlreadyResumed))
        );
    }
}
