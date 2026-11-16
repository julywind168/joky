use super::{support::*, VmEngine, VmMode};
use crate::ErrorCode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/local_suspension_segments.jk");
const ENTRIES: &[&str] = &[
    "local_closures",
    "local_owners",
    "local_consumed_owners",
    "local_control_flow",
    "local_pipelines",
    "local_without_suspension",
    "local_borrowed_records",
    "borrowed_record_views",
    "borrowed_sequence_views",
];

fn cleaned() {
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn local_suspension_segments_execute_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{error}"));
        for &entry in ENTRIES {
            for cancel in [false, true] {
                let mut task = vm.call_async(entry, &[]).unwrap();
                if !cancel {
                    for _ in 0..16 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42), "{mode:?}: {entry}");
                }
                drop(task);
                cleaned();
            }
        }
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(SOURCE)
        .unwrap_or_else(|error| panic!("{error}"));
    for &entry in ENTRIES {
        assert_cancellation(
            &mut vm,
            entry,
            100_000,
            sweep,
            || {},
            || crate::runtime::arena::LIVE.with(|live| live.get()),
            cleaned,
        );
    }
}

#[test]
fn local_suspension_segments_cancel_at_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel within synchronous segments of suspending functions"]
fn exhaustive_local_suspension_segments_cancel_every_instruction() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn local_suspension_segments_reject_live_borrowed_values_at_the_boundary() {
    for body in [
        "let n = 42; let f = || { n }; task.yield_now(); f()",
        "let n = 42; let f = || { n }; let alias = f; task.yield_now(); alias()",
        "var n = 41; var f = || { n += 1 }; task.yield_now(); f(); n",
        "let p = alloc(42); let values = [&*p]; task.yield_now(); *values[0]",
        // Destruction keeps an owner live even after its last explicit read.
        "let p = alloc(42); let values = [&*p]; let n = *values[0]; task.yield_now(); n",
        "let p = alloc(42); let values = [&*p]; let f = move || { *values[0] }; let n = f(); task.yield_now(); n",
        "let n = 42; let f = || { n }; if n == 42 { task.yield_now() }; f()",
        "let n = 42; let f = || { n }; var i = 0; while i < 2 { let _ = f(); task.yield_now(); i += 1 }; n",
        "let n = 42; let f = || { n }; let wait: fn() @suspends -> Unit = task.yield_now; wait(); f()",
        "let n = 42; let f = || { n }; join { task.yield_now(), task.yield_now() }; f()",
        "let n = 42; let f = || { n }; race { task.yield_now(), task.yield_now() }; f()",
        "let n = 42; let f = || { n }; scope { branch { task.yield_now() } }; f()",
        "let values = [42]; let pipeline = iter.shared(&values); task.yield_now(); var n = 0; for value in pipeline { n += *value }; n",
        "var values = [41]; let pipeline = iter.exclusive(&mut values); task.yield_now(); for value in pipeline { *value += 1 }; values[0]",
        "let values = [42]; let limit = 0; for _ in iter.shared(&values).filter(|v| { *v > limit }) { task.yield_now() }; 42",
        "var values = [42]; let limit = 0; for _ in iter.exclusive(&mut values).filter(|v| { *v > limit }) { task.yield_now() }; 42",
        "let p = alloc(42); let wrapped = PatternBox { value: [&*p] }; let PatternBox { value } = &wrapped; task.yield_now(); *value[0]",
        "let p = alloc(42); let wrapped = PatternBox { value: [&*p] }; let PatternBox { value } = &wrapped; join { task.yield_now(), task.yield_now() }; *value[0]",
    ] {
        let source = format!("import joky/task import joky/iter struct PatternBox<T> {{ value: T }} fn bad() @suspends -> Int32 {{ {body} }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert_eq!(error.code(), ErrorCode::InvalidCall, "{body}: {error}");
        assert!(error.message().contains("cannot remain live across suspension"), "{body}: {error}");
        let marked = error.span().unwrap().slice(&source).unwrap();
        assert!(marked.contains("yield_now") || marked == "wait()", "{body}: {marked}");
    }
}

#[test]
fn borrowed_destructuring_preserves_conflict_and_escape_checks() {
    for body in [
        "fn bad() -> &String { let source = Box { value: \"local\" }; let Box { value } = &source; value }",
        "fn bad() @suspends { var source = Box { value: \"old\" }; let Box { value } = &source; source = Box { value: \"new\" }; assert_eq(value, \"old\") }",
        "fn bad() @suspends { let source = Box { value: \"old\" }; let Box { value } = &source; let moved = source; assert_eq(value, \"old\") }",
        "fn bad() @suspends { let escaped = { let source = Box { value: \"local\" }; let Box { value } = &source; value }; assert_eq(escaped, \"local\") }",
    ] {
        let source = format!("struct Box<T> {{ value: T }}\n{body}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert_eq!(error.code(), ErrorCode::InvalidAssignment, "{body}: {error}");
        assert!(error.span().is_some(), "{body}: {error}");
    }
}
