use super::support::*;
use super::VmMode;
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/range_slices.jk");

#[test]
fn range_slices_preserve_views_order_bounds_and_mutability() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [0, 1, 2]));
        for name in [
            "bad_half",
            "bad_reverse",
            "bad_closed",
            "bad_fixed",
            "bad_subslice",
            "bad_max",
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let error = vm.call_unit_0(name).unwrap_err();
            assert!(error.span().is_some(), "{name}: {error}");
            EVENTS.with(|events| {
                assert_eq!(
                    *events.borrow(),
                    if name == "bad_max" { vec![99] } else { vec![] }
                )
            });
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn range_slices_resume_once_and_release_borrows_on_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for cancel in [false, true] {
            EVENTS.with(|events| events.borrow_mut().clear());
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [-99, -1, -1]));
            if !cancel {
                READY.with(|ready| ready.set(true));
                for _ in 0..30 {
                    if task.is_finished() {
                        break;
                    }
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            EVENTS.with(|events| {
                assert_eq!(
                    *events.borrow(),
                    if cancel {
                        vec![-99, -1, -1, 99]
                    } else {
                        vec![-99, -1, -1, 0, 0, 4, 4, 99]
                    }
                )
            });
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn range_slices_reject_invalid_types_conflicting_loans_and_escapes() {
    for source in [
        "fn bad() { let a = [1, 2]; let n: Int32 = 1; let s = &a[0..n]; }",
        "fn bad() { let a = [1, 2]; let s = &a[-1..2]; }",
        "fn bad() { let a = [1, 2]; let s = &a[0.0..1.0]; }",
        "fn bad() { let a = [1, 2]; let r = 0..2; let s = &a[r]; }",
        "fn bad() { let a = [1, 2]; let s = a[0..2]; }",
        "fn bad() { let a = [1, 2]; let s = &mut a[0..2]; }",
        "fn bad() { var a = [1, 2]; let r = &a; let s = &mut r[0..2]; }",
        "fn bad() { var a = [1, 2]; let s = &a[0..2]; a.push(3); s[0]; }",
        "fn bad() { var a = [1, 2]; let s = &mut a[0..1]; let t = &mut a[1..2]; s[0] += t[0]; }",
        "fn bad() { var a = [1, 2]; let s = &mut a[0..{ a.push(3); 2 }]; s[0] += 1; }",
        "import joky/slice.{type Slice} fn bad() -> &Slice<Int32> { let a = [1, 2]; &a[0..2] }",
        "fn bad() { let s = &\"abc\"[0..2]; }",
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

#[test]
#[ignore = "exhaustive: cancel range slicing at every instruction"]
fn exhaustive_range_slices_instruction_cancellation() {
    let mut vm = parity_vm_with_prelude(SOURCE, VmMode::Interpreter, false);
    assert_cancellation(
        &mut vm,
        "pending",
        20_000,
        CancellationSweep::EveryInstruction,
        || {
            READY.with(|ready| ready.set(true));
            EVENTS.with(|events| events.borrow_mut().clear());
        },
        || EVENTS.with(|events| events.borrow().clone()),
        || {
            LIVE.with(|live| assert_eq!(live.get(), 0));
            EVENTS.with(|events| {
                let events = events.borrow();
                let drops = events.iter().filter(|&&n| n == 99).count();
                assert!(drops <= 1);
                if events.contains(&-99) {
                    assert_eq!(drops, 1);
                }
            });
        },
    );
}

#[test]
fn slice_members_preserve_alias_rejection_and_nested_diagnostics() {
    for source in [
        "import joky/slice.{type Slice} impl<T> Slice<T> { fn extra(&self) -> Int32 { 0 } }",
        "import joky/slice.{type Slice} type View<T> = Slice<T> impl<T> View<T> { fn extra(&self) -> Int32 { 0 } }",
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert!(error.message().contains("aliases and specialized"), "{error}");
    }
    for value in ["view.missing()", "keep(view.missing())"] {
        let source = format!("fn keep<T>(value: T) -> T {{ value }} fn main() {{ let values = [1, 2]; let view = &values[0..2]; println({value}) }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert_eq!(error.code(), crate::ErrorCode::UnknownName);
        assert!(
            error.message().contains("has no method 'missing'"),
            "{error}"
        );
    }
    let error = frontend_error(
        "import joky/array.{type Array} fn main() { let values = Array.new() }",
        &crate::ModuleSources::new(),
    );
    assert!(error.message().contains("Cannot infer"), "{error}");
    assert!(!error.message().contains("joky/member/"), "{error}");
}
