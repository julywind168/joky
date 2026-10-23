use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::arena::{LIMIT, LIVE};

const SOURCE: &str = include_str!("../../../../tests/fixtures/iterator_enumerate.jk");

#[test]
fn iterator_enumerate_is_lazy_generic_and_preserves_non_fused_inputs() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [100, 101, 102, 103]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
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
fn iterator_enumerate_moves_items_and_cleans_unconsumed_owners() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for (action, expected) in [(0, 6), (1, 5), (2, 1), (3, 1), (4, 0)] {
            reset();
            let result = vm.call_int32_1("consume", action);
            if action == 4 {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), expected);
            }
            all_dropped();
            let bodies: Vec<_> = events().into_iter().filter(|&event| event > 10).collect();
            assert_eq!(
                bodies,
                if action < 2 {
                    vec![11, 12, 13]
                } else {
                    vec![11]
                }
            );
        }
        reset();
        vm.call_unit_0("unstarted").unwrap();
        all_dropped();
        assert!(events().iter().all(|&event| event < 10));
        reset();
        assert_eq!(vm.call_int32_0("retained").unwrap(), 2);
        all_dropped();
        let events = events();
        let used = events.iter().position(|&event| event == 11).unwrap();
        for id in [2, 3] {
            assert!(events.iter().position(|&event| event == id).unwrap() < used);
        }
        assert!(events.iter().position(|&event| event == 1).unwrap() > used);
    }
}

#[test]
fn iterator_enumerate_resumes_without_replaying_items_or_indices() {
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
fn iterator_enumerate_cancellation_covers_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel enumerate item transfer, tuple construction and owner cleanup"]
fn exhaustive_iterator_enumerate_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        LIMIT.with(|limit| limit.set(self.0));
    }
}
#[test]
fn iterator_enumerate_allocation_failure_cleans_transferred_items() {
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
                assert_eq!(value, 6);
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
fn iterator_enumerate_rejects_borrowed_cursors_reuse_and_private_fields() {
    for body in [
        "var a = [1, 2]; let e = iter.enumerate(a.iter_mut());",
        "let a = [1, 2]; let e = iter.enumerate(a.iter());",
        "let a = [1, 2]; let e = iter.enumerate(a);",
        "let i = [1, 2].into_iter(); let e = i.enumerate(); for _ in i {}",
        "let e = [1, 2].into_iter().enumerate(); for _ in e {}; for _ in e {}",
        "let e = range(0, 2).enumerate(); e.index;",
        "let e = range(0, 2).enumerate(); e.iterator;",
        "let e = iter.test_enumerate_from(range(0, 2), 0);",
    ] {
        let source = format!("import joky/iter.{{type Iterator, range}} fn bad() {{ {body} }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{body}: {error}");
    }
}

#[test]
fn iterator_enumerate_maximum_index_and_overflow_cleanup() {
    extern "C" fn record_int(value: i32) {
        EVENTS.with(|events| events.borrow_mut().push(value));
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
        registry
            .types()
            .standard
            .set(crate::core::imports::iter_test_library())
            .unwrap();
        let mut vm = VmEngine::with_mode_and_registry(mode, registry);
        vm.set_prelude_enabled(false);
        // SAFETY: The callback matches the declared synchronous Int32 -> Unit ABI.
        unsafe {
            vm.register_fn(
                "record_int",
                crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Unit),
                record_int as *const u8,
            )
            .unwrap();
        }
        vm.load_source(include_str!(
            "../../../../tests/fixtures/iterator_enumerate_boundaries.jk"
        ))
        .unwrap();
        for (entry, failure, mut expected) in [
            ("maximum", false, vec![1, 2]),
            ("overflow", true, vec![1, 2, 3, 10]),
            ("after_none", true, vec![101, 102, 103]),
        ] {
            reset();
            let result = vm.call_int32_0(entry);
            if failure {
                assert!(result
                    .unwrap_err()
                    .message()
                    .contains("Enumeration index overflow"));
            } else {
                assert_eq!(result.unwrap(), 42);
            }
            let mut actual = events();
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected, "{mode:?} {entry}");
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}
