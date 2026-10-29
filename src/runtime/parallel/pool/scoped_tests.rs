//! Preconditions for scoped inputs. These probes do not admit reference packets.
use super::*;
use std::{sync::mpsc, time::Duration};

#[test]
fn scoped_borrow_publication_failure_never_starts_a_partial_batch() {
    for capacity in [1, 2] {
        let mut pool = ParallelExecutor::new(3, capacity).unwrap();
        let budget = super::super::TaskBudget::new(1 << 20);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        struct Capture(Arc<AtomicUsize>);
        impl Drop for Capture {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        struct Parent {
            started: Arc<AtomicUsize>,
            dropped: Arc<AtomicUsize>,
        }
        impl Job for Parent {
            fn poll(&mut self) -> Poll<Result<Output, Error>> {
                let factories: Vec<Factory> = (0..2)
                    .map(|_| {
                        let started = self.started.clone();
                        let capture = Capture(self.dropped.clone());
                        Box::new(move || {
                            started.fetch_add(1, Ordering::SeqCst);
                            drop(capture);
                            Err(invalid("An unpublished factory ran"))
                        }) as Factory
                    })
                    .collect();
                let mut group = Group::new(0);
                let error = group.publish(factories).unwrap_err();
                assert_eq!(error.code(), ErrorCode::ResourceLimit);
                assert!(group.handles.is_empty());
                assert!(lock(&group.arbiter.state).children.is_empty());
                assert!(CURRENT.with(|c| lock(&c.borrow().as_ref().unwrap().state)
                    .children
                    .is_empty()));
                Poll::Ready(Ok(Box::new(())))
            }
        }
        let parent_started = started.clone();
        let parent_dropped = dropped.clone();
        let task = pool
            .submit(
                root,
                Arc::new(()),
                Box::new(move || {
                    Ok(Box::new(Parent {
                        started: parent_started,
                        dropped: parent_dropped,
                    }))
                }),
            )
            .unwrap();
        task.wait().unwrap();
        task.take().unwrap().unwrap();
        drop(task);
        pool.wait_closed();
        assert_eq!(started.load(Ordering::SeqCst), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scoped_borrow_pool_retains_failed_parent_until_running_reader_cleanup() {
    struct Parent {
        group: Option<Group>,
        reader_started: mpsc::Sender<()>,
        release: Option<mpsc::Receiver<()>>,
        failed: mpsc::Sender<()>,
        events: Arc<Mutex<Vec<(&'static str, thread::ThreadId)>>>,
    }
    struct Reader {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        events: Arc<Mutex<Vec<(&'static str, thread::ThreadId)>>>,
    }
    impl Job for Reader {
        fn poll(&mut self) -> Poll<Result<Output, Error>> {
            self.started.send(()).unwrap();
            // Deliberately hold a polling turn, not a scheduler wait. The test
            // thread releases it even if an assertion fails or the parent stops.
            self.release.recv_timeout(Duration::from_secs(10)).unwrap();
            lock(&self.events).push(("read", thread::current().id()));
            Poll::Ready(Ok(Box::new(())))
        }
    }
    impl Drop for Reader {
        fn drop(&mut self) {
            lock(&self.events).push(("reader cleanup", thread::current().id()));
        }
    }
    impl Job for Parent {
        fn poll(&mut self) -> Poll<Result<Output, Error>> {
            if self.group.is_none() {
                lock(&self.events).push(("parent", thread::current().id()));
                let reader = Reader {
                    started: self.reader_started.clone(),
                    release: self.release.take().unwrap(),
                    events: self.events.clone(),
                };
                let mut group = Group::new(0);
                group
                    .publish(vec![Box::new(move || Ok(Box::new(reader)))])
                    .unwrap();
                // Retain the group through its parent's second polling turn.
                // Waiting for the reader's host notification avoids timing races.
                self.group = Some(group);
                wait_for_wake();
                return Poll::Pending;
            }
            self.group.take();
            self.failed.send(()).unwrap();
            Poll::Ready(Err(invalid("Parent failed with a live reader")))
        }
    }
    impl Drop for Parent {
        fn drop(&mut self) {
            lock(&self.events).push(("parent cleanup", thread::current().id()));
        }
    }
    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(send) = self.0.take() {
                let _ = send.send(());
            }
        }
    }
    let mut pool = ParallelExecutor::new(3, 8).unwrap();
    let budget = super::super::TaskBudget::new(1 << 20);
    let root = Storage::new(vec![budget.clone()]).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let parent_events = events.clone();
    let (started, reader_started) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (failed, parent_failed) = mpsc::channel();
    let task = pool
        .submit(
            root,
            Arc::new(()),
            Box::new(move || {
                Ok(Box::new(Parent {
                    group: None,
                    reader_started: started,
                    release: Some(released),
                    failed,
                    events: parent_events,
                }))
            }),
        )
        .unwrap();
    let release = Release(Some(release));
    reader_started
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    task.0.wake();
    parent_failed.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!task.cleaned());
    assert_eq!(
        lock(&events).len(),
        1,
        "source cleanup preceded reader completion"
    );
    drop(release);
    task.wait().unwrap();
    assert_eq!(
        task.take().unwrap().err().unwrap().code(),
        ErrorCode::InvalidState
    );
    drop(task);
    pool.wait_closed();
    let events = lock(&events);
    assert_eq!(
        events.iter().map(|e| e.0).collect::<Vec<_>>(),
        ["parent", "read", "reader cleanup", "parent cleanup"]
    );
    assert_eq!(events[0].1, events[3].1, "source cleanup left its worker");
    assert_ne!(events[0].1, events[1].1, "reader did not run remotely");
    assert_eq!(events[1].1, events[2].1);
    assert_eq!(budget.used(), 0);
}
