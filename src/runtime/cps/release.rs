//! Destruction of native activation trees without an allocated traversal stack.
//! Parents retain borrowed storage and code until their children have stopped.
use super::CpsFrame;

#[derive(Default)]
pub(super) struct Cursor {
    active: bool,
    parent: *mut CpsFrame,
    child: usize,
    group_finished: bool,
    scope: usize,
    scope_child: usize,
}

impl CpsFrame {
    pub(crate) fn release(&mut self) {
        if self.release_cursor.active {
            return;
        }
        self.release_cursor.active = true;
        self.begin_release();
        let root = self as *mut Self;
        let mut current = root;
        // Only this walk owns the detached boxes. Back links refer to live,
        // stationary ancestors; no reference into a registry spans a callback.
        unsafe {
            loop {
                if let Some(child) = (*current).next_release_child() {
                    let child = Box::into_raw(child);
                    (*child).release_cursor = Cursor {
                        active: true,
                        parent: current,
                        ..Default::default()
                    };
                    (*child).begin_release();
                    current = child;
                    continue;
                }
                (*current).finish_release();
                if current == root {
                    // finish() may restore a returned owner after this pass.
                    // A later Drop must still release that final result.
                    (*current).release_cursor = Cursor::default();
                    return;
                }
                let parent = (*current).release_cursor.parent;
                // The active flag prevents Drop from starting a second pass.
                // All executable descendants have already been detached.
                drop(Box::from_raw(current));
                current = parent;
            }
        }
    }

    fn next_release_child(&mut self) -> Option<Box<Self>> {
        while let Some(child) = self.children.get_mut(self.release_cursor.child) {
            self.release_cursor.child += 1;
            if let Some(child) = child.take() {
                return Some(child);
            }
        }
        self.children.clear();
        if !self.release_cursor.group_finished {
            self.release_cursor.group_finished = true;
            if let Some(group) = &mut self.concurrent {
                if !group.drain_finished() {
                    let cleanup = crate::runtime::error::current_error().map_or(Ok(()), Err);
                    let _ = group.finish_drain(cleanup);
                }
            }
        }
        while let Some(scope) = self.task_scopes.get_mut(self.release_cursor.scope) {
            if let Some(child) = scope.take_next_child(&mut self.release_cursor.scope_child) {
                return Some(child);
            }
            let cleanup = crate::runtime::error::current_error().map_or(Ok(()), Err);
            let _ = scope.finish_drain(cleanup);
            self.release_cursor.scope += 1;
            self.release_cursor.scope_child = 0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, process::Command};

    thread_local! {
        static REMAINING: Cell<usize> = const { Cell::new(0) };
    }

    unsafe extern "C" fn stopped(frame: *mut CpsFrame, reason: u32) {
        assert_eq!(reason, crate::runtime::borrowed_host::STOP_CANCELLED);
        REMAINING.with(|remaining| {
            assert_eq!((*frame).result as usize, remaining.get());
            remaining.set(remaining.get() - 1);
        });
    }

    #[test]
    fn cps_deep_static_and_dynamic_cleanup_is_iterative() {
        const CHILD: &str = "JOKY_CPS_RELEASE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let name = std::thread::current().name().unwrap().to_owned();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                const DEPTH: usize = 10_000;
                let mut root = None;
                for depth in 1..=DEPTH {
                    let mut parent = Box::new(CpsFrame::new(0));
                    parent.result = depth as u64;
                    parent.host_stop = Some(stopped);
                    if let Some(child) = root {
                        if depth % 2 == 0 {
                            let mut scope = crate::runtime::task_scope::LocalTaskScope::default();
                            assert!(scope.publish(child).is_ok());
                            parent.task_scopes.push(scope);
                        } else {
                            parent.children.push(Some(child));
                        }
                    }
                    root = Some(parent);
                }
                REMAINING.set(DEPTH);
                crate::runtime::error::checked(|| drop(root)).unwrap();
                assert_eq!(REMAINING.get(), 0);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
