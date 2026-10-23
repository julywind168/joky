use super::support::*;
use super::VmMode;
use crate::runtime::arena::{LIMIT, LIVE};

const SOURCE: &str = include_str!("../../../../tests/fixtures/borrowed_enumerate.jk");

#[test]
fn borrowed_enumerate_preserves_indices_snapshots_and_mutable_access() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        for which in 0..4 {
            let error = vm.call_int32_1("invalid_current", which).unwrap_err();
            assert!(error.message().contains("requires a successful advance"));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn events() -> Vec<i32> {
    EVENTS.with(|events| events.borrow().clone())
}
fn cleaned() {
    LIVE.with(|live| assert_eq!(live.get(), 0));
    let events = events();
    for id in 1..=3 {
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
    let events = events();
    for id in 1..=3 {
        assert!(events.contains(&-id) && events.contains(&id), "{events:?}");
    }
}
#[test]
fn borrowed_enumerate_cleans_owners_after_early_exit_and_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for action in 0..5 {
            reset();
            let result = vm.call_int32_1("consume", action);
            if action == 4 {
                assert!(result.unwrap_err().message().contains("Division by zero"));
            } else {
                assert_eq!(result.unwrap(), 42);
            }
            all_dropped();
        }
    }
}
#[test]
fn borrowed_enumerate_resumes_live_items_and_cancels_without_replaying() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for cancel in [false, true] {
            reset();
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            assert_eq!(events(), [-1, -2, -3, 11, 11]);
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
            let bodies: Vec<_> = events().into_iter().filter(|&event| event > 10).collect();
            assert_eq!(
                bodies,
                if cancel {
                    vec![11, 11]
                } else {
                    vec![11, 11, 12, 12, 13, 13]
                }
            );
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
fn borrowed_enumerate_cancellation_covers_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel borrowed enumeration descriptors, item loans and owner cleanup"]
fn exhaustive_borrowed_enumerate_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}
struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        LIMIT.with(|limit| limit.set(self.0));
    }
}
#[test]
fn borrowed_enumerate_allocation_failure_cleans_borrowed_owners() {
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
                    assert_eq!(value, 42);
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
#[test]
fn borrowed_enumerate_rejects_conflicts_escapes_and_mutable_keys() {
    let prefix = "import joky/iter import joky/map.{type Map} import joky/array.{type Array} ";
    for body in [
        "let a = [1]; let c = iter.enumerate_mut(a.iter());",
        "var a = [1]; let c = iter.enumerate_mut(a.iter_mut()); iter.enumerate_advance(c); let i = iter.enumerate_current_mut(c); iter.enumerate_advance(c); *iter.value_mut(i) = 2;",
        "var a = [1]; let c = iter.enumerate_mut(a.iter_mut()); iter.enumerate_advance(c); let i = iter.enumerate_current_mut(c); let x = iter.value_mut(i); let y = iter.value_mut(i); *x = *y;",
        "var a = [1]; let c = iter.enumerate_ref(a.iter()); a.push(2); for _ in c {}",
        "var a = [1]; let source = a.iter_mut(); let c = iter.enumerate_mut(&mut *source); iter.advance(source); for _ in c {}",
        "var a = [1]; let c = iter.enumerate_mut(a.iter_mut()); for _ in c {}; for _ in c {}",
        "let c = { let a = [1]; iter.enumerate_ref(a.iter()) }; for _ in c {}",
        "let a = [1]; let c = iter.enumerate_ref(a.iter()); let xs = [c];",
        "let a = [1]; let c = iter.enumerate_ref(a.iter()); iter.enumerate_advance(c); let i = iter.enumerate_current(c); let xs = (i, 0);",
        "var m = Map.new::<Int32, Int32>(); m.insert(1, 2); for i in iter.enumerate_entries_mut(m.iter_mut()) { *map.entry_key(iter.entry_mut(i)) = 3; }",
        "var m = Map.new::<Int32, Int32>(); m.insert(1, 2); for i in iter.enumerate_entries_mut(m.iter_mut()) { let e = iter.entry_mut(i); let key = map.entry_key(e); *map.entry_value_mut(e) = *key; }",
        "var m = Map.new::<Int32, Int32>(); m.insert(1, 2); let c = iter.enumerate_entries_mut(m.iter_mut()); iter.enumerate_entries_advance(c); let i = iter.enumerate_entries_current_mut(c); iter.enumerate_entries_advance(c); map.entry_value_mut(iter.entry_mut(i));",
        "let a = [1]; let c = iter.enumerate_ref(a.iter()); let e = iter.enumerate(c);",
    ] {
        let source = format!("{prefix} fn bad() {{ {body} }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{body}: {error}");
    }
    for declaration in [
        "fn bad(x: iter.EnumerateIter<Int32>) {}",
        "fn bad(x: iter.Indexed<Int32>) {}",
        "fn bad(x: iter.EnumerateEntries<Int32, Int32>) {}",
        "fn bad(x: iter.IndexedEntry<Int32, Int32>) {}",
    ] {
        let error = frontend_error(
            &format!("{prefix} {declaration}"),
            &crate::ModuleSources::new(),
        );
        assert!(error.message().contains("borrow-only"), "{error}");
    }
}
