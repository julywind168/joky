use super::support::*;
use super::VmMode;
const SOURCE: &str = include_str!("../../../../tests/fixtures/map_borrow_iter.jk");

#[test]
fn map_borrow_iter_pairs_columns_and_preserves_entry_identity() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
    }
}

fn events() -> Vec<i32> {
    EVENTS.with(|events| events.borrow().clone())
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn cleaned() {
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    let events = events();
    for id in 1..=2 {
        let created = events.iter().filter(|&&event| event == -id).count();
        let dropped = events.iter().filter(|&&event| event == id).count();
        assert!(
            created <= 1 && dropped <= 1 && dropped >= created,
            "{events:?}"
        );
    }
}

#[test]
fn map_borrow_iter_preserves_owned_keys_and_values_on_exit_and_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for action in 0..3 {
            reset();
            let result = vm.call_int32_1("consume", action);
            if action == 2 {
                assert_eq!(result.unwrap_err().code(), crate::ErrorCode::RuntimeError);
            } else {
                assert_eq!(result.unwrap(), 42);
            }
            cleaned();
            for id in [1, 2] {
                assert!(events().contains(&id));
            }
            if action == 0 {
                let events = events();
                let end = events.iter().position(|&id| id == 20).unwrap();
                for id in [1, 2] {
                    assert!(events.iter().position(|&n| n == id).unwrap() > end);
                }
            }
        }
        for after_end in [0, 1] {
            reset();
            let error = vm.call_int32_1("invalid_current", after_end).unwrap_err();
            assert!(error.message().contains("successful advance"), "{error}");
            assert!(error.span().is_some());
            cleaned();
        }
    }
}
#[test]
fn map_borrow_iter_retains_entries_across_pending_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for cancel in [false, true] {
            reset();
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            let prefix = events();
            assert_eq!(prefix.len(), 4);
            let first = prefix[2];
            assert!([11, 12].contains(&first));
            assert_eq!(prefix, [-1, -2, first, first]);
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
            cleaned();
            for id in 1..=2 {
                assert!(events().contains(&id));
            }
            let mut bodies: Vec<_> = events().into_iter().filter(|&id| id > 10).collect();
            bodies.sort();
            assert_eq!(
                bodies,
                if cancel {
                    vec![first, first]
                } else {
                    vec![11, 11, 12, 12]
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
fn map_borrow_iter_cancellation_covers_view_construction_and_cleanup() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel paired cursor and entry construction, reborrows and owner cleanup"]
fn exhaustive_map_borrow_iter_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn map_borrow_iter_allocation_failures_keep_map_owners_live() {
    struct RestoreLimit(usize);
    impl Drop for RestoreLimit {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
        }
    }
    use crate::runtime::arena::LIMIT;
    let restore = RestoreLimit(LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let mut completed = false;
        for budget in (0..32768).step_by(16) {
            reset();
            LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_1("consume", 0);
            LIMIT.with(|limit| limit.set(restore.0));
            cleaned();
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    assert!(events().contains(&20));
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
fn map_borrow_iter_rejects_conflicts_and_escaping_entries() {
    let prefix = "import joky/map.{type Map, type Iter, type Entry}\n";
    for (function, message) in [
        ("fn bad(m: &mut Map<Int32, Int32>) { let i=m.iter(); m.insert(2,3); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(m: Map<Int32, Int32>) { let i=m.iter(); m.release(); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(m: &mut Map<Int32, Int32>) { let i=m.iter_mut(); m.get(1); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(m: &mut Map<Int32, Int32>) { let i=m.iter_mut(); map.advance(i); let e=map.current_mut(i); map.advance(i); map.entry_value(e); }", "conflicts with a live borrow"),
        ("fn bad(m: &mut Map<Int32, Int32>) { for e in m.iter_mut() { let k=map.entry_key(e); let v=map.entry_value_mut(e); *v = *k } }", "conflicts with a live borrow"),
        ("fn bad(m: &mut Map<Int32, Int32>) { for e in m.iter_mut() { *map.entry_key(e) = 2 } }", "shared reference"),
        ("fn bad(m: &Map<Int32, Int32>) { for e in m.iter() { map.entry_value_mut(e); } }", "expected &mut Entry"),
        ("fn bad(m: &Map<Int32, Int32>) { for e in m.iter() { let saved = (e, 1); } }", "Tuple element must be"),
        ("fn bad() -> &Iter<Int32, Int32> { let m=Map.new::<Int32,Int32>(); m.iter() }", "compatible borrowed input"),
        ("fn bad() -> &Entry<Int32, Int32> { var m=Map.new::<Int32,Int32>(); m.insert(1,2); let i=m.iter(); map.advance(i); map.current(i) }", "compatible borrowed input"),
        ("fn bad(e: Entry<Int32, Int32>) {}", "borrow-only targets"),
        ("fn bad(i: Iter<Int32, Int32>) {}", "borrow-only targets"),
        ("fn bad(m: &Map<Int32, Int32>) { for e in m.iter() { let saved = [e]; } }", "Generic type argument must be"),
        ("fn bad(m: &Map<Int32, &Int32>) { m.iter(); }", "reference"),
    ] {
        let error = frontend_error(&format!("{prefix}{function}"), &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{function}: {error}");
        assert!(error.span().is_some());
    }
}
