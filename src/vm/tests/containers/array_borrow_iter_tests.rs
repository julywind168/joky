use super::support::*;
use super::VmMode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/array_borrow_iter.jk");

fn events() -> Vec<i32> {
    EVENTS.with(|events| events.borrow().clone())
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn cleaned() {
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    let events = events();
    for id in 1..=4 {
        let created = events.iter().filter(|&&event| event == -id).count();
        let dropped = events.iter().filter(|&&event| event == id).count();
        assert!(
            created <= 1 && dropped <= 1 && dropped >= created,
            "{events:?}"
        );
    }
}
fn all_dropped() {
    cleaned();
    for id in 1..=4 {
        assert!(events().contains(&id));
    }
}

#[test]
fn array_borrow_iter_preserves_owners_and_checks_current_state() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for (action, expected) in [(0, 10), (1, 9), (2, 1), (3, 1), (4, 0)] {
            reset();
            let result = vm.call_int32_1("consume", action);
            if action == 4 {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), expected);
            }
            all_dropped();
            let events = events();
            if action < 3 {
                let end = events.iter().position(|&n| n == 20).unwrap();
                for id in 1..=4 {
                    assert!(events.iter().position(|&n| n == id).unwrap() > end);
                }
            }
        }
        for after_end in [0, 1] {
            let error = vm.call_int32_1("invalid_current", after_end).unwrap_err();
            assert!(error.message().contains("successful advance"), "{error}");
            assert!(error.span().is_some());
            cleaned();
        }
    }
}

#[test]
fn array_borrow_iter_keeps_elements_live_across_pending_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for entry in ["pending", "pending_shared"] {
            for cancel in [false, true] {
                reset();
                READY.with(|ready| ready.set(false));
                let mut task = vm.call_async(entry, &[]).unwrap();
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                assert_eq!(events(), [-1, -2, -3, -4, 11, 11]);
                if !cancel {
                    READY.with(|ready| ready.set(true));
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42));
                }
                drop(task);
                all_dropped();
                let bodies: Vec<_> = events().into_iter().filter(|&id| id > 10).collect();
                assert_eq!(
                    bodies,
                    if cancel {
                        vec![11, 11]
                    } else {
                        vec![11, 11, 12, 12]
                    }
                );
            }
        }
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm_with_prelude(SOURCE, VmMode::Interpreter, false);
    assert_cancellation(
        &mut vm,
        "pending",
        20_000,
        sweep,
        || {
            reset();
            READY.with(|ready| ready.set(true));
        },
        events,
        cleaned,
    );
}
#[test]
fn array_borrow_iter_cancellation_covers_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel borrowed cursor construction, element reborrows and owner cleanup"]
fn exhaustive_array_borrow_iter_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn array_borrow_iter_rejects_conflicting_loans_and_escaping_views() {
    let prefix = "import joky/array.{type Array}\nimport joky/iter as iter.{type Iter}\n";
    for (body, message) in [
        ("let p = alloc(1); let a = [&*p]; for _ in a.iter() {}", "nested references"),
        ("var a = [1, 2]; let i = a.iter_mut(); for _ in i { break }; iter.advance(i);", "exclusive reference"),
        ("var a = [1, 2]; let i = a.iter(); a.push(3); for _ in i {}", "conflicts with a live borrow"),
        ("var a = [1, 2]; let i = a.iter_mut(); a.get(0); for _ in i {}", "conflicts with a live borrow"),
        ("var a = [1, 2]; let i = a.iter_mut(); iter.advance(i); let r = iter.current_mut(i); iter.advance(i); *r = 3;", "conflicts with a live borrow"),
        ("var a = [1, 2]; let i = a.iter_mut(); iter.advance(i); let r = iter.current_mut(i); let s = iter.current_mut(i); *r = *s;", "conflicts with a live borrow"),
        ("var a = [1, 2]; let i = a.iter(); iter.current_mut(i);", "expected &mut Iter"),
        ("let a = [1, 2]; let i = a.iter(); let moved = a; for _ in i {}", "conflicts with a live borrow"),
        ("let a = [1, 2]; let i = a.iter(); let stored = (i, 1);", "Tuple element must be"),
        ("let a = [1, 2]; let i = a.iter(); let stored = [i];", "Generic type argument must be"),
        ("var a = [1, 2]; for r in a.iter_mut() { a.push(*r) }", "conflicts with a live borrow"),
        ("let a = [1, 2]; for r in a.iter() { *r = 5 }", "shared reference"),
    ] {
        let error = frontend_error(&format!("{prefix}fn bad() {{ {body} }}"), &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{body}: {error}");
        assert!(error.span().is_some());
    }
    for (function, message) in [
        ("fn bad() -> &Iter<Int32> { let a = [1, 2]; a.iter() }", "compatible borrowed input"),
        ("fn bad() -> &Int32 { let a = [1, 2]; let i = a.iter(); iter.advance(i); iter.current(i) }", "outlive its owner"),
        ("fn bad(i: Iter<Int32>) {}", "borrow-only targets"),
    ] {
        let error = frontend_error(&format!("{prefix}{function}"), &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{function}: {error}");
        assert!(error.span().is_some());
    }
}

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
    }
}
#[test]
fn array_borrow_iter_allocation_failures_keep_cleanup_responsibility() {
    use crate::runtime::arena::LIMIT;
    let restore = RestoreLimit(LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let mut completed = false;
        for budget in (0..32768).step_by(64) {
            reset();
            LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_1("consume", 0);
            LIMIT.with(|limit| limit.set(restore.0));
            cleaned();
            match result {
                Ok(value) => {
                    assert_eq!(value, 10);
                    all_dropped();
                    completed = true;
                    break;
                }
                Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
            }
        }
        assert!(completed);
    }
}
