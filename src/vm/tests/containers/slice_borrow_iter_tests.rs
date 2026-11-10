use super::support::*;
use super::VmMode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/slice_borrow_iter.jk");

fn events() -> Vec<i32> {
    EVENTS.with(|events| events.borrow().clone())
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn cleaned() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn slice_borrow_iter_retains_subslice_elements_across_pending_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for cancel in [false, true] {
            reset();
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert_eq!(events(), [-1, -2, -3, -4, 12, 12]);
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
            let bodies: Vec<_> = events().into_iter().filter(|&id| id > 10).collect();
            assert_eq!(
                bodies,
                if cancel {
                    vec![12, 12]
                } else {
                    vec![12, 12, 13, 13]
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
fn slice_borrow_iter_cancellation_covers_view_construction_and_cleanup() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel nested slice cursor construction, reborrows and owner cleanup"]
fn exhaustive_slice_borrow_iter_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn slice_borrow_iter_rejects_conflicts_through_parent_views() {
    let prefix = "import joky/array.{type Array}\nimport joky/slice as slice.{type Slice}\nimport joky/map.{type Map}\nimport joky/iter as iter.{type Iter}\n";
    for (function, message) in [
        ("fn bad(a: &mut Array<Int32>) { let i = slice.iter(a.slice(0, 1)); a.push(2); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(a: &mut Slice<Int32>) { let i = slice.iter_mut(slice.slice_mut(a, 0, 1)); slice.set(a, 0, 2); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(m: &mut Map<Int32, Int32>) { let i = slice.iter(m.keys()); m.insert(2, 3); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(m: &mut Map<Int32, Int32>) { let i = slice.iter_mut(m.values_mut()); m.get(1); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(m: &mut Map<Int32, Int32>) { slice.iter_mut(m.keys()); }", "expected &mut Slice"),
        ("fn bad(m: Map<Int32, Int32>) { let i = slice.iter(m.values()); m.release(); for _ in i {} }", "conflicts with a live borrow"),
        ("fn bad(m: &Map<Int32, Int32>) { for key in slice.iter(m.keys()) { *key = 2 } }", "shared reference"),
        ("fn bad(s: &Slice<&Int32>) { slice.iter(s); }", "reference-free elements"),
        ("fn bad() -> &Iter<Int32> { let a = [1]; slice.iter(a.slice(0, 1)) }", "compatible borrowed input"),
        ("fn bad() -> &Int32 { let a = [1]; let i = slice.iter(a.slice(0, 1)); iter.advance(i); iter.current(i) }", "outlive its owner"),
    ] {
        let error = frontend_error(&format!("{prefix}{function}"), &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{function}: {error}");
        assert!(error.span().is_some());
    }
}

#[test]
fn slice_borrow_iter_allocation_failures_keep_map_owners_live() {
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
        for budget in (0..32768).step_by(32) {
            reset();
            LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_1("map_owners", 0);
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
