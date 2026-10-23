use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/ranges.jk");

#[test]
fn ranges_cover_integer_boundaries_inference_and_expression_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 3]));
        EVENTS.with(|events| events.borrow_mut().clear());
        assert!(vm.call_unit_0("endpoint_failure").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [7, 99]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn ranges_resume_endpoints_once_and_release_owners_on_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for cancel in [false, true] {
            EVENTS.with(|events| events.borrow_mut().clear());
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [-99, 0, 0]));
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
                        vec![-99, 0, 0, 99]
                    } else {
                        vec![-99, 0, 0, 2, 2, 11, 11, 12, 12, 13, 13, 99]
                    }
                )
            });
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn ranges_fix_standard_identity_and_preserve_imported_types() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "app/ranges",
            "import joky/iter.{type Range} pub fn small() -> Range<UInt16> { 1..=uint16(3) }",
        )
        .unwrap();
    let source = r#"
        import app/ranges
        import joky/iter as other
        fn range(a: Int32, b: Int32) -> Bool { false }
        fn range_inclusive(a: Int32, b: Int32) -> Bool { false }
        fn compute() -> Int32 {
            var n = 0
            for v in 1..=3 { n += v }
            for v in ranges.small() { n += int32(v) }
            for v in other.range(0, 3) { n += v }
            let text = "{for _ in 0..2 {}}"
            if n == 15 { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
    }
}

#[test]
fn ranges_reject_mixed_or_unsupported_endpoints_and_incomplete_syntax() {
    for body in [
        "let r = int8(0)..uint8(2)",
        "let r = false..true",
        "let r = 0.0..=1.0",
        "let r = 'a'..='z'",
        "let r = \"a\"..\"z\"",
        "let r = range::<UInt8>(0, 256)",
        "let r = range(0, uint64(3)); let wrong: Range<Int32> = r",
        "for v in 0..2 { let wrong: UInt64 = v; }",
        "let r = 0..",
        "let r = ..3",
        "let r = 0..=",
        "let r = ..=3",
        "let r = 0..1..2",
        "let r = 0..=1..=2",
        "let r = [..=3]",
        "match 1 { 0..=3 => {}, _ => {} }",
        "let r = [1, 2]; r[0..1];",
    ] {
        let source = format!("import joky/iter.{{range, type Range}} fn bad() {{ {body} }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

#[test]
#[ignore = "exhaustive: cancel range endpoint evaluation and iteration at every instruction"]
fn exhaustive_ranges_instruction_cancellation() {
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
