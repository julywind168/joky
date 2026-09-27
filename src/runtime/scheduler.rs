//! Cooperative scheduling with externally signaled readiness.
use super::suspension::{Continuation, ContinuationStatus, ResumeError};
use std::collections::BTreeMap;

/// Monotonic IDs prevent stale events from addressing newly created tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

#[derive(Default)]
pub struct Scheduler {
    queue: Vec<Box<dyn FnOnce()>>,
    // An ordered map keeps polling order deterministic without retaining tombstones.
    tasks: BTreeMap<TaskId, Continuation>,
    next_id: u64,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn spawn(&mut self, task: impl FnOnce() + 'static) {
        self.queue.push(Box::new(task));
    }
    pub fn run_all(&mut self) {
        for task in std::mem::take(&mut self.queue) {
            task();
        }
    }
    pub fn suspend(&mut self, task: Continuation) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("Task IDs exhausted");
        self.tasks.insert(id, task);
        id
    }
    pub fn resume(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        self.tasks
            .get_mut(&id)
            .ok_or(SchedulerError::UnknownContinuation(id))?
            .resume()
            .map_err(SchedulerError::Resume)
    }
    pub fn continuation(&self, id: TaskId) -> Result<&Continuation, SchedulerError> {
        self.tasks
            .get(&id)
            .ok_or(SchedulerError::UnknownContinuation(id))
    }
    pub fn take_completed(&mut self, id: TaskId) -> Result<Continuation, SchedulerError> {
        match self.continuation(id)?.status_ref() {
            ContinuationStatus::Pending => return Err(SchedulerError::NotCompleted(id)),
            ContinuationStatus::Failed(error) => {
                return Err(SchedulerError::Resume(error.clone()))
            }
            ContinuationStatus::Completed => {}
        }
        Ok(self.tasks.remove(&id).unwrap())
    }
    pub fn take_failed(&mut self, id: TaskId) -> Result<Continuation, SchedulerError> {
        if !matches!(
            self.continuation(id)?.status_ref(),
            ContinuationStatus::Failed(_)
        ) {
            return Err(SchedulerError::NotFailed(id));
        }
        Ok(self.tasks.remove(&id).unwrap())
    }
    pub fn cancel(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        self.tasks
            .remove(&id)
            .ok_or(SchedulerError::UnknownContinuation(id))?;
        Ok(())
    }
    /// Number of stored tasks, including terminal tasks awaiting collection.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }
    pub fn suspended_count(&self) -> usize {
        self.tasks.values().filter(|c| !c.is_finished()).count()
    }
    /// Poll in ID order, stopping at the first failure. The failure remains
    /// available through the task's status until it is collected or cancelled.
    pub fn resume_all(&mut self) -> Result<usize, SchedulerError> {
        let mut polled = 0;
        for task in self.tasks.values_mut() {
            if !task.is_finished() {
                task.resume().map_err(SchedulerError::Resume)?;
                polled += 1;
            }
        }
        Ok(polled)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SchedulerError {
    UnknownContinuation(TaskId),
    Resume(ResumeError),
    NotCompleted(TaskId),
    NotFailed(TaskId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[test]
    fn runs_tasks_in_order() {
        let mut scheduler = Scheduler::new();
        let events = Rc::new(RefCell::new(Vec::new()));
        for i in 0..3 {
            let events = events.clone();
            scheduler.spawn(move || events.borrow_mut().push(i));
        }
        scheduler.run_all();
        assert_eq!(*events.borrow(), [0, 1, 2]);
    }
    #[test]
    fn completed_and_cancelled_tasks_do_not_accumulate() {
        let mut vm = crate::vm::VmEngine::new();
        vm.load_source("fn f() @suspends -> Int32 { 42 }").unwrap();
        let mut scheduler = Scheduler::new();
        let old = scheduler.suspend(vm.call_async("f", &[]).unwrap());
        scheduler.cancel(old).unwrap();
        for i in 0..10_000 {
            let id = scheduler.suspend(vm.call_async("f", &[]).unwrap());
            assert_ne!(id, old);
            if i % 2 == 0 {
                scheduler.cancel(id).unwrap();
            } else {
                assert_eq!(scheduler.take_completed(id).unwrap().cps_result(), Some(42));
            }
            assert_eq!(scheduler.task_count(), 0);
        }
        assert_eq!(
            scheduler.resume(old),
            Err(SchedulerError::UnknownContinuation(old))
        );
    }
}
