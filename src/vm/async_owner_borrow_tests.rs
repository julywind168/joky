use super::{support, VmEngine, VmMode};
use crate::runtime::{arena::LIVE, driver::block_on};
const SOURCE: &str = include_str!("../../tests/fixtures/async_owner_borrows.jk");
fn clean() {
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn async_owner_borrows_short_segments_in_both_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            block_on(vm.prepare_async("exercise", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
}
fn cancel(sweep: support::CancellationSweep) {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
    support::assert_cancellation(
        &mut vm,
        "exercise",
        5000,
        sweep,
        || {},
        || LIVE.with(|n| n.get()),
        clean,
    );
}
#[test]
fn async_owner_borrows_cancel_at_resource_boundaries() {
    cancel(support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel short owner borrows and container growth at every instruction"]
fn exhaustive_async_owner_borrows_cancel_every_instruction() {
    cancel(support::CancellationSweep::EveryInstruction);
}
#[test]
fn async_owner_borrows_reject_live_views_and_conflicts() {
    for body in [
        "let item = values.borrow_ref(0) task.yield_now() let n = *item",
        "let region = values.slice(0, 1) task.yield_now() slices.len(region);",
        "let item = values.borrow_ref(0) values.push(9) let n = *item",
        "let view = &values task.yield_now() view.len();",
    ] {
        let source = format!("import joky/array.{{type Array}} import joky/task import joky/slice as slices fn bad() @suspends {{ var values = Array.new::<Int32>() values.push(42) {body} }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(
                VmEngine::with_mode(mode).load_source(&source).is_err(),
                "{source}"
            );
        }
        let ast = crate::parser::parse(&source).unwrap();
        assert!(
            crate::aot::AotCompiler::new("invalid-short-borrow")
                .unwrap()
                .compile_module(&ast)
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn async_owner_borrows_keep_places_after_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let task = vm.call_async("exercise", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        assert_eq!(block_on(task).unwrap(), 42);
        clean();
    }
}
