//! Region ownership is independent of scheduler groups. Tasks and continuations
//! capture the current region, while only an explicit region group closes it.
use std::cell::RefCell;
use std::sync::{Arc, Condvar, Mutex, Weak};

pub(crate) struct Region {
    parent: Option<Arc<Region>>,
    state: Mutex<State>,
    closed: Condvar,
}

#[derive(Default)]
struct State {
    phase: u8,
    cowns: Vec<usize>,
    children: Vec<Weak<Region>>,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Region>>> = const { RefCell::new(None) };
}

pub(crate) struct Guard(Option<Arc<Region>>);

impl Drop for Guard {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.0.take());
    }
}

pub(crate) fn install(region: Arc<Region>) -> Guard {
    Guard(CURRENT.with(|current| current.replace(Some(region))))
}

pub(crate) fn current() -> Arc<Region> {
    CURRENT
        .with(|current| current.borrow().clone())
        .unwrap_or_else(|| super::scope::current_or_default().region.clone())
}

impl Region {
    pub(crate) fn root() -> Arc<Self> {
        Arc::new(Self {
            parent: None,
            state: Mutex::default(),
            closed: Condvar::new(),
        })
    }

    pub(crate) fn enter() -> Arc<Self> {
        let parent = current();
        let region = Arc::new(Self {
            parent: Some(parent.clone()),
            state: Mutex::default(),
            closed: Condvar::new(),
        });
        let mut state = parent.state.lock().expect("region state");
        assert!(state.phase == 0, "allocation in a closed region");
        state.children.retain(|child| child.strong_count() != 0);
        state.children.push(Arc::downgrade(&region));
        drop(state);
        CURRENT.with(|current| *current.borrow_mut() = Some(region.clone()));
        region
    }

    pub(crate) fn register(&self, cown: *mut u8) {
        let mut state = self.state.lock().expect("region state");
        assert!(state.phase == 0, "allocation in a closed region");
        state.cowns.push(cown as usize);
    }

    /// Caller has drained all users. Destructors run without the registry lock.
    pub(crate) fn close(&self) {
        let (cowns, children) = {
            let mut state = self.state.lock().expect("region state");
            while state.phase == 1 {
                state = self.closed.wait(state).expect("region close");
            }
            if state.phase == 2 {
                return;
            }
            state.phase = 1;
            (
                std::mem::take(&mut state.cowns),
                std::mem::take(&mut state.children),
            )
        };
        for child in children.into_iter().filter_map(|child| child.upgrade()) {
            child.close();
        }
        for &cown in &cowns {
            unsafe { super::managed::drop_region_cown_payload(cown as *mut u8) };
        }
        for cown in cowns {
            super::managed::jk_drop(cown as *mut u8);
        }
        self.state.lock().expect("region state").phase = 2;
        self.closed.notify_all();
        CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            if current
                .as_ref()
                .is_some_and(|region| std::ptr::eq(region.as_ref(), self))
            {
                *current = self.parent.clone();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::managed::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[repr(C)]
    struct Payload {
        next: *mut u8,
        drops: *const AtomicUsize,
    }
    unsafe extern "C" fn drop_payload(pointer: *mut u8) {
        let payload = unsafe { &*pointer.cast::<Payload>() };
        unsafe { &*payload.drops }.fetch_add(1, Ordering::SeqCst);
        jk_drop(payload.next);
        jk_drop(pointer);
    }
    fn node(drops: &AtomicUsize) -> (*mut u8, *mut u8) {
        let payload = jk_alloc_object(
            RuntimeValueKind::Class as u8,
            std::mem::size_of::<Payload>(),
            std::mem::align_of::<Payload>(),
        );
        unsafe {
            payload.cast::<Payload>().write(Payload {
                next: std::ptr::null_mut(),
                drops,
            });
        }
        (jk_cown_new(payload, Some(drop_payload)), payload)
    }

    #[test]
    fn cycles_are_freed_once_at_region_exit_without_handle_rc() {
        let scope = crate::runtime::scope::RuntimeScope::new();
        let _scope = scope.enter();
        let drops = AtomicUsize::new(0);
        let region = Region::enter();
        let (a, pa) = node(&drops);
        let (b, pb) = node(&drops);
        unsafe {
            (*pa.cast::<Payload>()).next = jk_dup(b);
            (*pb.cast::<Payload>()).next = jk_dup(a);
        }
        for _ in 0..100 {
            jk_drop(jk_dup(a));
        }
        jk_drop(a);
        jk_drop(b);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(scope.managed_objects.load(Ordering::Acquire), 4);
        region.close();
        region.close();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
        // Compiler cleanups can discard inert capabilities after bulk destruction.
        jk_drop(a);
        jk_drop(b);
        scope.close_and_wait();
    }

    #[test]
    fn nested_regions_and_worker_inheritance_preserve_ancestor() {
        let scope = crate::runtime::scope::RuntimeScope::new();
        let _scope = scope.enter();
        let drops = Arc::new(AtomicUsize::new(0));
        let (outer, _) = node(&drops);
        let inner = Region::enter();
        let worker_scope = scope.clone();
        let worker_region = inner.clone();
        let worker_drops = drops.clone();
        let outer_handle = outer as usize;
        std::thread::spawn(move || {
            let _scope = worker_scope.enter();
            let _region = install(worker_region);
            let (handle, payload) = node(&worker_drops);
            unsafe {
                (*payload.cast::<Payload>()).next = outer_handle as *mut u8;
            }
            jk_drop(handle);
        })
        .join()
        .unwrap();
        inner.close();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!jk_cown_payload(outer).is_null());
        scope.close_and_wait();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
    }
}
