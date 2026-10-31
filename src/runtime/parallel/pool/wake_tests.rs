//! Completion waiters and worker queues have independent notification predicates.
use super::*;
use std::{sync::mpsc, time::Duration};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Waiting {
    finish: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
    started: Option<mpsc::Sender<()>>,
}
impl Job for Waiting {
    fn poll(&mut self) -> Poll<Result<Output, Error>> {
        if let Some(started) = self.started.take() {
            started.send(()).unwrap();
        }
        if self.finish.load(Ordering::Acquire) {
            Poll::Ready(Ok(Box::new(42usize)))
        } else {
            wait_for_wake();
            Poll::Pending
        }
    }
}
impl Drop for Waiting {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::Release);
    }
}

#[test]
fn task_pool_completion_notifies_all_waiters_after_success_cancel_and_failure() {
    for outcome in 0..3 {
        let budget = super::super::TaskBudget::new(1 << 20);
        let mut pool = ParallelExecutor::new(2, 8).unwrap();
        let finish = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (started, start) = mpsc::channel();
        let job = Waiting {
            finish: finish.clone(),
            dropped: dropped.clone(),
            started: Some(started),
        };
        let task = pool
            .submit(
                Storage::new(vec![budget.clone()]).unwrap(),
                Arc::new(()),
                Box::new(move || Ok(Box::new(job))),
            )
            .unwrap();
        start.recv_timeout(TIMEOUT).unwrap();
        let (registered, registration) = mpsc::channel();
        let (done, completion) = mpsc::channel();
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let task = task.clone();
                let registered = registered.clone();
                let done = done.clone();
                let dropped = dropped.clone();
                thread::spawn(move || {
                    // This hook runs with the predicate mutex held. Completion
                    // may race the transition into wait, but cannot be lost.
                    BEFORE_COMPLETION_WAIT.with(|hook| {
                        *hook.borrow_mut() = Some(Box::new(move || registered.send(()).unwrap()));
                    });
                    task.wait().unwrap();
                    assert_eq!(dropped.load(Ordering::Acquire), 1);
                    done.send(()).unwrap();
                })
            })
            .collect();
        for _ in &waiters {
            registration.recv_timeout(TIMEOUT).unwrap();
        }
        match outcome {
            0 => {
                finish.store(true, Ordering::Release);
                task.0.wake();
            }
            1 => task.cancel(),
            _ => task.0.fail(invalid("Injected terminal failure")),
        }
        for _ in &waiters {
            completion.recv_timeout(TIMEOUT).unwrap();
        }
        for waiter in waiters {
            waiter.join().unwrap();
        }
        // A waiter arriving after publication must not depend on a future signal.
        task.wait().unwrap();
        let result = task.take().unwrap();
        match outcome {
            0 => assert_eq!(*result.unwrap().downcast::<usize>().unwrap(), 42),
            1 => assert_eq!(result.err().unwrap().code(), ErrorCode::Cancelled),
            _ => assert_eq!(result.err().unwrap().code(), ErrorCode::InvalidState),
        }
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn task_pool_worker_notifications_cover_batch_timer_and_global_close() {
    struct Scheduled {
        mode: Arc<AtomicUsize>,
        deadline: Option<Instant>,
    }
    impl Job for Scheduled {
        fn poll(&mut self) -> Poll<Result<Output, Error>> {
            match self.mode.load(Ordering::Acquire) {
                0 => wait_for_wake(),
                1 => {
                    let deadline = *self
                        .deadline
                        .get_or_insert_with(|| Instant::now() + Duration::from_millis(20));
                    if Instant::now() >= deadline {
                        return Poll::Ready(Ok(Box::new(42usize)));
                    }
                    wait_until(deadline);
                }
                _ => return Poll::Ready(Ok(Box::new(42usize))),
            }
            Poll::Pending
        }
    }
    let budget = super::super::TaskBudget::new(1 << 20);
    let mut pool = ParallelExecutor::new(3, 8).unwrap();
    let modes: Vec<_> = (0..4).map(|_| Arc::new(AtomicUsize::new(0))).collect();
    let factories: Vec<Factory> = modes
        .iter()
        .map(|mode| {
            let mode = mode.clone();
            Box::new(move || {
                Ok(Box::new(Scheduled {
                    mode,
                    deadline: None,
                }) as Box<dyn Job>)
            }) as Factory
        })
        .collect();
    // More tasks than workers also exercises a batch that wraps its destinations.
    let handles = publish(
        &pool.shared,
        None,
        Storage::new(vec![budget.clone()]).unwrap(),
        Arc::new(()),
        factories,
        None,
    )
    .unwrap();
    let limit = Instant::now() + TIMEOUT;
    while !handles.iter().all(Handle::waiting) {
        assert!(Instant::now() < limit, "batch destinations did not start");
        thread::yield_now();
    }
    modes[1].store(1, Ordering::Release);
    handles[1].0.wake();
    modes[3].store(2, Ordering::Release);
    handles[3].0.wake();
    let limit = Instant::now() + TIMEOUT;
    while !handles[1].cleaned() || !handles[3].cleaned() {
        assert!(Instant::now() < limit, "targeted wake or timer was lost");
        thread::yield_now();
    }
    assert!(handles[0].waiting() && handles[2].waiting());
    // Close must release idle workers as well as workers with parked live tasks.
    pool.wait_closed();
    for (index, handle) in handles.iter().enumerate() {
        handle.wait().unwrap();
        let result = handle.take().unwrap();
        if index == 1 || index == 3 {
            assert_eq!(*result.unwrap().downcast::<usize>().unwrap(), 42);
        } else {
            assert_eq!(result.err().unwrap().code(), ErrorCode::Cancelled);
        }
    }
    drop(handles);
    assert_eq!(budget.used(), 0);
}
