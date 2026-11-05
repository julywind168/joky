use super::support::*;
use super::VmMode;
use crate::runtime::arena::LIVE;
use crate::ErrorCode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/range_patterns.jk");

#[test]
fn range_patterns_cover_integer_domains_nested_payloads_and_first_match() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [21, 31, 22, 23, 2]));
        EVENTS.with(|events| events.borrow_mut().clear());
        let error = vm.call_unit_0("failure").unwrap_err();
        assert!(error.to_string().contains("range failure"));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [3, 99]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
        // String patterns alone must load their comparison with no prelude or imports.
        let vm = parity_vm_with_prelude(
            r#"fn compute() -> Int32 { match "text" { "other" => 0, "text" => 42, _ => 1 } }"#,
            mode,
            false,
        );
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
    }
}

#[test]
fn range_patterns_resume_selected_arms_without_replaying_the_scrutinee() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for cancel in [false, true] {
            EVENTS.with(|events| events.borrow_mut().clear());
            READY.with(|ready| ready.set(false));
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [-99, 1, 1]));
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
                        vec![-99, 1, 1, 99]
                    } else {
                        vec![-99, 1, 1, 40, 40, 99]
                    }
                )
            });
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn range_patterns_reject_empty_ranges_holes_and_fully_covered_arms() {
    for (source, code) in [
        (r#"fn bad(x: String) { match x { "a" => {}, r"a" => {}, _ => {} } }"#, ErrorCode::UnreachableCode),
        (r#"fn bad(x: String) { match x { "a" if true => {} } }"#, ErrorCode::NonExhaustiveMatch),
        ("fn bad(x: Bool) { match x { true if true => {}, false => {} } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(x: Int32) { match x { _ if true => {} } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(x: Int32) { match x { 1 => {}, 1 if true => {}, _ => {} } }", ErrorCode::UnreachableCode),
        ("fn bad(x: Int32) { match x { _ if 1 => {}, _ => {} } }", ErrorCode::TypeMismatch),
        (r#"fn take(s: String) -> Bool { true } fn bad() { match Some("kept") { Some(s) if take(s) => {}, _ => {} } }"#, ErrorCode::TypeMismatch),
        (r#"struct Item { text: String } fn bad() { match Some(Item { text: "kept" }) { Some(item) if { item.text = "changed"; false } => {}, _ => {} } }"#, ErrorCode::InvalidAssignment),
        ("fn bad(x: Int32) -> Int32 { match x { 1..1 => 0, _ => 1 } }", ErrorCode::InvalidPattern),
        ("fn bad(x: Int32) -> Int32 { match x { 2..=1 => 0, _ => 1 } }", ErrorCode::InvalidPattern),
        ("fn bad(x: UInt64) -> Int32 { match x { 0..18446744073709551615 => 0 } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(x: Int8) -> Int32 { match x { -128..0 => 0, 1..=127 => 1 } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(x: UInt8) -> Int32 { match x { 0..=255 => 0, _ => 1 } }", ErrorCode::UnreachableCode),
        ("fn bad(x: Int32) -> Int32 { match x { 0..=10 => 0, 5 => 1, _ => 2 } }", ErrorCode::UnreachableCode),
        ("fn bad(x: Int8) -> Int32 { match x { -5..0 => 0, -1 => 1, _ => 2 } }", ErrorCode::UnreachableCode),
        ("fn bad(x: Int32) -> Int32 { match x { 0..5 => 0, 5..10 => 1, 2..8 => 2, _ => 3 } }", ErrorCode::UnreachableCode),
        ("fn bad(x: (Bool, UInt8)) -> Int32 { match x { (true, 0..128) => 0, (false, 128..=255) => 1 } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(x: Bool) -> Int32 { match x { 0..=1 => 0, _ => 1 } }", ErrorCode::InvalidPattern),
        ("fn bad(x: Float64) -> Int32 { match x { 0..=1 => 0, _ => 1 } }", ErrorCode::TypeMismatch),
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.span().is_some(), "{source}: {error}");
    }
    for source in [
        "fn bad(x: UInt8) { match x { 0..=256 => {}, _ => {} } }",
        "fn bad(x: Int8) { match x { -129..=1 => {}, _ => {} } }",
        "fn bad(x: UInt8) { match x { -1..=2 => {}, _ => {} } }",
        "fn bad(x: Int32) { match x { 0..= => {}, _ => {} } }",
        "fn bad(x: Int32) { match x { ..3 => {}, _ => {} } }",
        "fn bad(x: Int32) { match x { 0..1..2 => {}, _ => {} } }",
        "fn bad(x: Int32, n: Int32) { match x { 0..n => {}, _ => {} } }",
        "fn bad(x: Rune) { match x { 'a'..='z' => {}, _ => {} } }",
        "fn bad() { let 0..3 = 2 }",
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

#[test]
fn range_patterns_bound_interval_coverage_work() {
    let mut source = "fn bad(x: (Int32, Bool)) -> Int32 { match x {".to_string();
    for n in 0..300 {
        source.push_str(&format!("({}..={}, true) => 0,", n * 2, n * 2));
    }
    source.push_str("_ => 1 } }");
    let error = frontend_error(&source, &crate::ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.to_string().contains("coverage"));
}

#[test]
#[ignore = "exhaustive: cancel integer range patterns at every instruction"]
fn exhaustive_range_patterns_instruction_cancellation() {
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
