//! Owning dynamic receivers retain their loans and concrete entries while pending.
use super::{support::*, VmMode};
use crate::runtime::{arena::LIVE, suspension::ContinuationStatus};
const SOURCE: &str = include_str!("../../../../tests/fixtures/suspending_dyn.jk");

fn cleaned() {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in [1, 2] {
            let drops = events.iter().filter(|&&v| v == id).count();
            assert!(drops <= 1, "{events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "{events:?}");
            }
        }
    });
    EVENTS.with(|events| {
        let events = events.borrow();
        if let (Some(child), Some(parent)) = (
            events.iter().position(|&v| v == 2),
            events.iter().position(|&v| v == 1),
        ) {
            assert!(child < parent, "{events:?}");
        }
    });
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn suspending_dyn_resume_mutate_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        for (entry, expected) in [
            ("dyn_remote", &[-1, 19, 19, 20, 0, 1][..]),
            ("dyn_memory", &[19, 19, 20, 0][..]),
            ("dyn_result", &[-1, -2, 40, 2, 1][..]),
            ("dyn_nested", &[-1, -2, 40, 2, 1][..]),
            ("dyn_text", &[-1, 40, 1][..]),
            ("dyn_argument", &[-1, 39, 40, 1][..]),
            ("dyn_place", &[-1, 40, 1][..]),
        ] {
            for cancel in [false, true] {
                READY.with(|r| r.set(false));
                EVENTS.with(|e| e.borrow_mut().clear());
                let mut task = vm.call_async(entry, &[]).unwrap();
                let initial = EVENTS.with(|e| e.borrow().clone());
                for _ in 0..3 {
                    assert_eq!(task.status(), ContinuationStatus::Pending, "{entry}");
                    task.resume().unwrap();
                    EVENTS.with(|e| assert_eq!(*e.borrow(), initial, "{entry}"));
                }
                if !cancel {
                    READY.with(|r| r.set(true));
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42), "{mode:?} {entry}");
                    EVENTS.with(|e| assert_eq!(*e.borrow(), expected, "{entry}"));
                }
                drop(task);
                cleaned();
            }
        }
        EVENTS.with(|e| e.borrow_mut().clear());
        READY.with(|r| r.set(false));
        let mut task = vm.call_async("dyn_failure", &[]).unwrap();
        READY.with(|r| r.set(true));
        assert!(task.resume().is_err());
        drop(task);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [-1, -2, 40, 2, 1]));
        cleaned();
        EVENTS.with(|e| e.borrow_mut().clear());
        READY.with(|r| r.set(false));
        let task = vm.call_async("dyn_race", &[]).unwrap();
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        cleaned();
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    for entry in [
        "dyn_remote",
        "dyn_result",
        "dyn_nested",
        "dyn_place",
        "dyn_race",
    ] {
        assert_cancellation(
            &mut vm,
            entry,
            100_000,
            sweep,
            || {
                READY.with(|r| r.set(true));
                EVENTS.with(|e| e.borrow_mut().clear());
            },
            || EVENTS.with(|e| e.borrow().clone()),
            cleaned,
        );
    }
}
#[test]
fn suspending_dyn_cancel_at_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel at every suspending dynamic dispatch instruction"]
fn exhaustive_suspending_dyn_cancel_every_instruction() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn suspending_dyn_reject_borrowed_receivers_and_effect_erasure() {
    let base = "trait Store { fn input(&self, n: Int32) @suspends @noescape -> Int32 { n } fn get(&self) @suspends @noescape -> Int32 { 42 } fn set(&mut self) @suspends @noescape {} fn peek(&self) @noescape -> Int32 { 42 } } struct Data { n: Int32 } impl Store for Data {} fn open() -> dyn Store { Data { n: 1 } }";
    for (body, diagnostic) in [
        ("fn bad() { let s = open(); s.get() }", "@suspends caller"),
        (
            "fn bad() @suspends { let s = open(); let v = &s; v.get() }",
            "owning dyn receiver",
        ),
        (
            "fn bad() @suspends { var s = open(); let v = &mut s; v.set() }",
            "owning dyn receiver",
        ),
        ("fn bad(s: &dyn Store) @suspends {}", "synchronous function"),
        (
            "fn bad() @suspends { var s = open(); let v = &s; s.set(); v.peek(); }",
            "borrow",
        ),
        (
            "fn bad() @suspends { var s = open(); let v = &mut s; s.get(); v.peek(); }",
            "borrow",
        ),
        (
            "fn bad() @suspends { let s = open(); s.get; }",
            "function values",
        ),
        (
            "fn consume(s: dyn Store) -> Int32 { 1 } fn bad() @suspends { let s = open(); s.input(consume(s)); }",
            "borrow",
        ),
        (
            "fn bad() @suspends { let s = open(); let v = &s; s.get(); v.peek(); }",
            "dyn view",
        ),
    ] {
        let source = format!("{base} {body}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(
            error.message().contains(diagnostic),
            "{diagnostic}: {error}\n{source}"
        );
    }
    for (method, diagnostic) in [
        (
            "fn get(&self, value: &Int32) @suspends @noescape -> Int32",
            "reference-free explicit parameters",
        ),
        (
            "fn get(&self) @suspends @noescape -> &Int32",
            "cannot return references",
        ),
        ("fn get(&self) @suspends -> Int32", "declared @noescape"),
    ] {
        let source = format!("trait Store {{ {method} }} fn unused(s: dyn Store) {{}}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.message().contains(diagnostic), "{error}");
    }
}

#[test]
fn suspending_dyn_keep_tables_alive_after_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(SOURCE, mode);
        READY.with(|r| r.set(false));
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut old = vm.call_async("dyn_remote", &[]).unwrap();
        let cancelled = vm.call_async("dyn_remote", &[]).unwrap();
        vm.load_source(&SOURCE.replace("+ 20\n", "+ 29\n")).unwrap();
        let mut new = vm.call_async("dyn_remote", &[]).unwrap();
        assert_ne!(old.module_version(), new.module_version());
        drop(vm);
        drop(cancelled);
        READY.with(|r| r.set(true));
        for task in [&mut old, &mut new] {
            for _ in 0..20 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
        }
        assert_eq!(old.cps_result(), Some(42));
        assert_eq!(new.cps_result(), Some(51));
        drop((old, new));
        EVENTS.with(|e| {
            let e = e.borrow();
            assert_eq!(e.iter().filter(|&&v| v == -1).count(), 3);
            assert_eq!(e.iter().filter(|&&v| v == 1).count(), 3);
        });
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
