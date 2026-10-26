//! Opt-in Local scheduling for tests and embedding diagnostics. Live OS events
//! and remote workers must be replaced by explicit inputs for reproducible runs.
use super::*;
use std::time::Duration;

const VERSION: u32 = 1;

/// Executor-local task ordinals are stable across runs; process TaskIds are not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Poll {
        max_polls: usize,
        ready: Vec<u64>,
        selected: Vec<u64>,
    },
    Advance(Duration),
    Inject(u64),
}

/// An in-memory replay script. Preserve the build, backend, fixtures and inputs
/// alongside this trace; it is not a cross-version or cross-backend format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    pub version: u32,
    pub seed: u64,
    pub steps: Vec<Step>,
}

enum Mode {
    Record { random: u64, capacity: usize },
    Replay { cursor: usize },
}
pub(super) struct Schedule {
    trace: Trace,
    mode: Mode,
    next_task: u64,
    failure: Option<Error>,
    in_event: bool,
}
impl Schedule {
    fn healthy(&self) -> Result<(), Error> {
        self.failure.clone().map_or(Ok(()), Err)
    }
    fn fail<T>(&mut self, message: impl std::fmt::Display) -> Result<T, Error> {
        let error = invalid(&format!(
            "Controlled schedule: {message}; seed={}",
            self.trace.seed
        ));
        self.failure.get_or_insert(error.clone());
        Err(error)
    }
    pub(super) fn check_poll(&self) -> Result<(), Error> {
        self.healthy()?;
        if self.in_event {
            return Err(invalid("Cannot poll during controlled event injection"));
        }
        Ok(())
    }
    pub(super) fn next_task(&mut self) -> Result<u64, Error> {
        self.healthy()?;
        let id = self.next_task;
        self.next_task = id
            .checked_add(1)
            .ok_or_else(|| invalid("Controlled task ordinals exhausted"))?;
        Ok(id)
    }
    fn accept(&mut self, step: Step) -> Result<(), Error> {
        self.healthy()?;
        match self.mode {
            Mode::Record { capacity, .. } => {
                if self.trace.steps.len() == capacity {
                    return self.fail(format_args!("trace capacity {capacity} exhausted"));
                }
                self.trace.steps.push(step);
            }
            Mode::Replay { cursor } => {
                if self.trace.steps.get(cursor) != Some(&step) {
                    return self.fail(format_args!(
                        "replay diverged at step {cursor}: observed {step:?}"
                    ));
                }
                self.mode = Mode::Replay { cursor: cursor + 1 };
            }
        }
        Ok(())
    }
    pub(super) fn select(
        &mut self,
        max_polls: usize,
        mut available: Vec<(TaskId, u64)>,
    ) -> Result<Vec<TaskId>, Error> {
        self.check_poll()?;
        available.sort_unstable_by_key(|(_, ordinal)| *ordinal);
        let ready: Vec<_> = available.iter().map(|(_, ordinal)| *ordinal).collect();
        let count = max_polls.min(ready.len());
        let (selected, random) = match self.mode {
            Mode::Record { mut random, .. } => {
                let mut selected = ready.clone();
                // SplitMix64 and Fisher-Yates; no platform RNG or hash iteration.
                for i in (1..selected.len()).rev() {
                    random = random.wrapping_add(0x9e3779b97f4a7c15);
                    let mut z = random;
                    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
                    z ^= z >> 31;
                    selected.swap(i, (z % (i as u64 + 1)) as usize);
                }
                selected.truncate(count);
                (selected, Some(random))
            }
            Mode::Replay { cursor } => match self.trace.steps.get(cursor) {
                Some(Step::Poll { selected, .. }) => (selected.clone(), None),
                _ => return self.fail(format_args!("expected a poll at step {cursor}")),
            },
        };
        let mut unique = selected.clone();
        unique.sort_unstable();
        unique.dedup();
        if selected.len() != count
            || unique.len() != count
            || unique.iter().any(|id| ready.binary_search(id).is_err())
        {
            return self.fail("replay selected an invalid ready batch");
        }
        self.accept(Step::Poll {
            max_polls,
            ready,
            selected: selected.clone(),
        })?;
        if let (Some(next), Mode::Record { random, .. }) = (random, &mut self.mode) {
            *random = next;
        }
        Ok(selected
            .into_iter()
            .map(|id| available[available.binary_search_by_key(&id, |(_, n)| *n).unwrap()].0)
            .collect())
    }
}

struct VirtualClock {
    origin: Instant,
    elapsed: Cell<Duration>,
}
impl Clock for VirtualClock {
    fn now(&self) -> Instant {
        self.origin + self.elapsed.get()
    }
}

/// A bounded, reproducible Local driver. Events and time advance only between
/// polls; injected callbacks publish their fixture data before waking a task.
/// Root scheduling is controlled, not instructions inside a native poll.
pub struct ControlledExecutor {
    executor: HostExecutor,
    clock: Rc<VirtualClock>,
    schedule: Rc<RefCell<Schedule>>,
}
impl ControlledExecutor {
    pub fn new(seed: u64, max_tasks: usize, trace_capacity: usize) -> Result<Self, Error> {
        let mut steps = Vec::new();
        steps
            .try_reserve_exact(trace_capacity)
            .map_err(|_| invalid("Controlled trace allocation failed"))?;
        Self::build(
            Trace {
                version: VERSION,
                seed,
                steps,
            },
            Mode::Record {
                random: seed,
                capacity: trace_capacity,
            },
            max_tasks,
        )
    }
    pub fn replay(trace: Trace, max_tasks: usize) -> Result<Self, Error> {
        if trace.version != VERSION {
            return Err(invalid("Unsupported controlled trace version"));
        }
        Self::build(trace, Mode::Replay { cursor: 0 }, max_tasks)
    }
    fn build(trace: Trace, mode: Mode, max_tasks: usize) -> Result<Self, Error> {
        if max_tasks == 0 {
            return Err(invalid(
                "Controlled executor requires a positive task limit",
            ));
        }
        let clock = Rc::new(VirtualClock {
            origin: Instant::now(),
            elapsed: Cell::new(Duration::ZERO),
        });
        let schedule = Rc::new(RefCell::new(Schedule {
            trace,
            mode,
            next_task: 1,
            failure: None,
            in_event: false,
        }));
        let executor =
            HostExecutor::with_schedule(clock.clone(), max_tasks, Some(schedule.clone()));
        Ok(Self {
            executor,
            clock,
            schedule,
        })
    }
    /// Clones of this executor use the same clock, scheduler and trace.
    pub fn executor(&self) -> &HostExecutor {
        &self.executor
    }
    pub fn trace(&self) -> Trace {
        self.schedule.borrow().trace.clone()
    }
    pub fn elapsed(&self) -> Duration {
        self.clock.elapsed.get()
    }
    fn boundary(&self) -> Result<(), Error> {
        if self.executor.inner.polling.get() || self.schedule.borrow().in_event {
            return Err(invalid(
                "Controlled inputs require a boundary between polls",
            ));
        }
        self.schedule.borrow().healthy()
    }
    pub fn advance(&self, by: Duration) -> Result<(), Error> {
        self.boundary()?;
        let elapsed = self
            .clock
            .elapsed
            .get()
            .checked_add(by)
            .filter(|d| self.clock.origin.checked_add(*d).is_some())
            .ok_or_else(|| invalid("Controlled clock overflow"))?;
        self.schedule.borrow_mut().accept(Step::Advance(by))?;
        self.clock.elapsed.set(elapsed);
        Ok(())
    }
    /// `event` identifies fixture-owned input; replay must supply the same data.
    /// A mismatch is rejected before invoking `apply`. No callback is retained.
    pub fn inject<T>(&self, event: u64, apply: impl FnOnce() -> T) -> Result<T, Error> {
        self.boundary()?;
        self.schedule.borrow_mut().accept(Step::Inject(event))?;
        self.schedule.borrow_mut().in_event = true;
        struct Reset<'a>(&'a RefCell<Schedule>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.borrow_mut().in_event = false;
            }
        }
        let _reset = Reset(&self.schedule);
        Ok(apply())
    }
    /// Require the complete replay script to have been consumed. Cleanup via
    /// cancel/cancel_group remains possible after divergence or trace exhaustion.
    pub fn finish(&self) -> Result<(), Error> {
        self.boundary()?;
        let schedule = self.schedule.borrow();
        if let Mode::Replay { cursor } = schedule.mode {
            if cursor != schedule.trace.steps.len() {
                return Err(invalid(&format!(
                    "Controlled replay ended at step {cursor} of {}; seed={}",
                    schedule.trace.steps.len(),
                    schedule.trace.seed
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
