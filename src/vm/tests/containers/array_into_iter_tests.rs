use super::support::*;
use super::VmMode;
use crate::runtime::arena::LIMIT;

const SOURCE: &str = include_str!("../../../../tests/fixtures/array_into_iter.jk");

fn events() -> Vec<i32> {
    EVENTS.with(|events| events.borrow().clone())
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn cleaned() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
fn all_dropped() {
    cleaned();
    let events = events();
    for id in 1..=4 {
        assert!(events.contains(&-id), "{events:?}");
    }
}

#[test]
fn array_into_iter_preserves_order_ownership_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        cleaned();
        for (action, expected) in [(0, 10), (1, 9), (2, 1), (3, 1), (4, 0)] {
            reset();
            let result = vm.call_int32_1("consume", action);
            if action == 4 {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), expected);
            }
            all_dropped();
            let bodies: Vec<_> = events().into_iter().filter(|&id| id > 10).collect();
            assert_eq!(
                bodies,
                if action < 2 {
                    vec![11, 12, 13, 14]
                } else {
                    vec![11]
                }
            );
        }
        reset();
        assert_eq!(vm.call_int32_0("retained").unwrap(), 2);
        all_dropped();
        let events = events();
        assert!(events.contains(&11));
        reset();
        vm.call_unit_0("unstarted").unwrap();
        all_dropped();
    }
}

fn pending_source() -> String {
    format!(
        r#"{SOURCE}
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pending() @suspends -> Int32 {{
            for value in values() {{
                delayed_value(record(10 + value.id))
                if value.id == 1 {{ continue }}
                break
            }}
            42
        }}
    "#
    )
}

#[test]
fn array_into_iter_cleans_current_and_remaining_items_after_pending() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(&pending_source(), mode, false);
        for cancel in [false, true] {
            reset();
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async("pending", &[]).unwrap();
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
            let bodies: Vec<_> = events().into_iter().filter(|&event| event > 10).collect();
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

fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm_with_prelude(&pending_source(), VmMode::Interpreter, false);
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
fn array_into_iter_cancellation_covers_conversion_and_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel array reversal, item transfer and remaining-owner cleanup"]
fn exhaustive_array_into_iter_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn array_into_iter_allocation_failures_release_initialized_items() {
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
            if let Ok(value) = result {
                assert_eq!(value, 10);
                all_dropped();
                completed = true;
                break;
            } else {
                assert_eq!(result.unwrap_err().code(), crate::ErrorCode::ResourceLimit);
            }
        }
        assert!(completed);
    }
}

#[test]
fn array_into_iter_rejects_reuse_live_borrows_and_private_storage() {
    for body in [
        "let a = [1, 2]; let i = a.into_iter(); a.len();",
        "let a = [1, 2]; let i = a.into_iter(); let j = i; for _ in i {}",
        "let a = [1, 2]; let r = a.borrow_ref(0); let i = a.into_iter(); let n = *r;",
        "let i = [1, 2].into_iter(); i.values;",
        "let a = [1, 2]; for _ in a {}; a.len();",
    ] {
        let error = frontend_error(
            &format!("fn bad() {{ {body} }}"),
            &crate::ModuleSources::new(),
        );
        assert!(error.span().is_some(), "{body}: {error}");
    }
}
