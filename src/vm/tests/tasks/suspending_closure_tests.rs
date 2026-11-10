//! Owned and borrowed suspending environments preserve captures across CPS calls.
use super::{support::*, VmMode};
use crate::runtime::{arena::LIVE, suspension::ContinuationStatus};

const SOURCE: &str = include_str!("../../../../tests/fixtures/suspending_closures.jk");
const CASES: &[(&str, &[i32])] = &[
    ("closure_direct", &[-1, 100, 40]),
    ("closure_transfer", &[-1, 40]),
    ("closure_nested", &[-1, -2, 40]),
    ("closure_argument", &[-1, 39, 40]),
    ("closure_pointer", &[41]),
    ("closure_named", &[41]),
    ("closure_unused", &[-1, 41]),
    ("closure_reference_argument", &[40]),
    ("closure_factory", &[41]),
    ("closure_borrowed", &[-1, 40]),
    ("closure_borrowed_mut", &[-1, 40]),
    ("closure_borrowed_reference", &[-1, 40]),
    ("closure_borrowed_unused", &[-1, 41]),
    ("closure_borrowed_empty", &[41]),
    ("closure_contextual_pointer", &[41]),
];

fn cleaned() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn suspending_closures_resume_transfer_and_cancel_owned_environments() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        for &(entry, expected) in CASES {
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
        for entry in ["closure_race", "closure_borrowed_race"] {
            EVENTS.with(|e| e.borrow_mut().clear());
            READY.with(|r| r.set(false));
            let task = vm.call_async(entry, &[]).unwrap();
            assert_eq!(task.cps_result(), Some(42));
            drop(task);
            cleaned();
        }
        for entry in ["closure_failure", "closure_borrowed_failure"] {
            EVENTS.with(|e| e.borrow_mut().clear());
            READY.with(|r| r.set(false));
            let mut task = vm.call_async(entry, &[]).unwrap();
            READY.with(|r| r.set(true));
            assert!(task.resume().is_err());
            assert!(matches!(task.status(), ContinuationStatus::Failed(_)));
            drop(task);
            cleaned();
        }
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    for entry in [
        "closure_nested",
        "closure_transfer",
        "closure_argument",
        "closure_unused",
        "closure_borrowed",
        "closure_borrowed_mut",
        "closure_borrowed_reference",
        "closure_borrowed_race",
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
fn suspending_closures_cancel_at_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel at every suspending closure instruction boundary"]
fn exhaustive_suspending_closures_cancel_every_instruction() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn suspending_closures_reject_effect_erasure_borrows_and_reuse() {
    for source in [
        "fn bad() { let f = || @suspends {}; f() }",
        "fn bad() { let n = 1; let r = &n; let f = move || @suspends { *r } }",
        "fn bad() { let f = move || @suspends {}; f() }",
        "fn bad() @suspends { let f = move || @suspends {}; f(); f() }",
        "fn bad() @suspends { let f = move || @suspends {}; let r = &f; r() }",
        "fn bad<F: FnOnce() @suspends -> Unit>(f: F) { f() }",
        "fn bad<F: FnOnce() @suspends -> Unit>(f: F) @suspends { f(); f() }",
        "fn apply<F: FnOnce() -> Unit>(f: F) { f() } fn bad() { apply(move || @suspends {}) }",
        "fn apply<F: FnOnce() @suspends -> Unit>(f: F) @suspends { f() } fn bad() @suspends { let f = move || {}; apply(f) }",
        "fn bad() { let f: fn() -> Unit = move || @suspends {} }",
        "fn bad() { let f = || {}; let g: fn() @suspends -> Unit = f }",
        "extern fn pending() @suspends fn bad() @suspends { let f = || { pending() }; f() }",
        "fn ready() {} fn apply<F: FnOnce() @suspends -> Unit>(f: F) @suspends { f() } fn bad() @suspends { apply(ready) }",
        "fn bad<F: FnOnce() -> Unit + FnOnce() @suspends -> Unit>(f: F) {}",
        "fn bad<F: Fn() @suspends -> Unit>(f: F) {}",
        "fn bad<F: FnMut() @suspends -> Unit>(f: F) {}",
        "fn bad() { let f = move || @suspends @suspends {} }",
        "fn bad() { let f = move || @unknown {} }",
        "fn bad() @suspends { let s = \"owned\"; let f = || @suspends { s }; f(); }",
        "fn bad() @suspends { var s = \"owned\"; let f = || @suspends { s.byte_length() }; s = \"changed\"; f(); }",
        "fn bad() @suspends { var n = 1; let f = || @suspends { n += 1 }; println(n); f(); }",
        "fn bad() @suspends { let n = 1; let f = || @suspends { n }; f(); f(); }",
        "fn bad() @suspends { let n = 1; let f = || @suspends { &n }; f(); }",
        "fn bad() @suspends { let n = 1; let f = { let s = \"local\"; || @suspends { s.byte_length() + uint64(n) } }; f(); }",
        "fn bad() @suspends { var n = 1; let f = || @suspends { n += 1 }; let g = || @suspends { n += 1 }; join { f(), g() } }",
        "fn bad() @suspends { let n = 1; let f = || @suspends { n }; let values = [f]; }",
    ] {
        frontend_error(source, &crate::ModuleSources::new());
    }
}

#[test]
fn suspending_closures_keep_hidden_entries_alive_after_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(SOURCE, mode);
        let mut old = vm.call_async("closure_direct", &[]).unwrap();
        let cancelled = vm.call_async("closure_direct", &[]).unwrap();
        vm.load_source(&SOURCE.replace("answer + owner.id", "answer + owner.id + 9"))
            .unwrap();
        let mut new = vm.call_async("closure_direct", &[]).unwrap();
        assert_ne!(old.module_version(), new.module_version());
        drop(vm);
        drop(cancelled);
        READY.with(|r| r.set(true));
        old.resume().unwrap();
        new.resume().unwrap();
        assert_eq!(old.cps_result(), Some(42));
        assert_eq!(new.cps_result(), Some(51));
        drop((old, new));
        EVENTS.with(|events| {
            let events = events.borrow();
            assert_eq!(events.iter().filter(|&&v| v == -1).count(), 3);
        });
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
