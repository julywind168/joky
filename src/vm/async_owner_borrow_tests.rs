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

const VIEWS: &str = include_str!("../../tests/fixtures/async_owner_views.jk");
#[test]
fn async_owner_views_execute_in_both_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(VIEWS).unwrap_or_else(|e| {
            panic!("{mode:?}: {e} {:?}", e.span().and_then(|s| s.slice(VIEWS)))
        });
        assert_eq!(
            block_on(vm.prepare_async("exercise_all", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
}

fn cancel_views(sweep: support::CancellationSweep) {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(VIEWS).unwrap_or_else(|e| panic!("{e}"));
    support::assert_cancellation(
        &mut vm,
        "exercise_all",
        10000,
        sweep,
        || {},
        || LIVE.with(|n| n.get()),
        clean,
    );
}
#[test]
fn async_owner_views_cancel_at_resource_boundaries() {
    cancel_views(support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel nested and scoped owner loans at every instruction"]
fn exhaustive_async_owner_views_cancel_every_instruction() {
    cancel_views(support::CancellationSweep::EveryInstruction);
}
#[test]
fn async_owner_views_keep_places_after_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(VIEWS).unwrap();
        let task = vm.call_async("exercise_all", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        assert_eq!(block_on(task).unwrap(), 42);
        clean();
    }
}

#[test]
fn async_owner_views_reject_interior_aliases_and_escapes() {
    let prefix = r#"
        import joky/array.{type Array}
        import joky/task
        import joky/slice as slices
        struct Packet { bytes: Array<Int32>, tag: Int32 }
        type Packets = Array<Packet>
        fn make() -> Packet { Packet { bytes: Array.new::<Int32>(), tag: 1 } }
        fn view<T>(values: &Array<T>) -> &T { values.borrow_ref(0) }
        fn identity<T>(value: &T) -> &T { value }
        fn inner(value: &Packet) -> &Array<Int32> { &value.bytes }
        fn read(value: &Packet) @suspends { task.yield_now() let n = value.tag }
        fn write(value: &mut Packet) @suspends { task.yield_now() value.tag = 2 }
    "#;
    for body in [
        "fn bad(values: &Packets) @suspends { let r = values.borrow_ref(0) task.yield_now() let n = r.tag }",
        "fn bad(values: &Packets) @suspends { read(view(values)) }",
        "fn bad(values: &Packets) @suspends { let r = identity(view(values)) task.yield_now() let n = r.tag }",
        "fn bad(values: &Packets) @suspends { let r = inner(view(values)) task.yield_now() r.len(); }",
        "fn choose(values: &Packets, p: &Packet, flag: Bool) -> &Packet { if flag { p } else { view(values) } } fn bad(values: &Packets, p: &Packet) @suspends { read(choose(values, p, false)) }",
        "fn left(values: &Packets, n: Int32) -> &Packet { if n == 0 { view(values) } else { right(values, n - 1) } } fn right(values: &Packets, n: Int32) -> &Packet { left(values, n) } fn bad(values: &Packets) @suspends { read(right(values, 2)) }",
        "fn bad(values: &Packets) @suspends { parallel { | read(view(values)) } }",
        "fn bad(values: &Packets) @suspends { let r = view(values) scope { branch { read(r) } } }",
        "fn bad(values: &Array<Int32>) @suspends { let r = values.slice(0, 1) task.yield_now() slices.len(r); }",
        "fn bad() @suspends { var value = make() parallel { | write(&mut value) | read(&value) } }",
        "fn bad() @suspends { var value = make() parallel { | write(&mut value) | write(&mut value) } }",
        "fn bad() @suspends { var value = make() scope { let r = &mut value branch { write(r) } value.tag = 3 } }",
        "fn bad() @suspends { var value = make() scope { let r = &value branch { read(r) } let moved = value } }",
        "fn bad() @suspends { let r = { let value = make() &value } read(r) }",
        "fn bad(value: &Packet) @suspends -> &Packet { task.yield_now() value }",
        "fn bad(value: &Packet) @suspends { alloc(value); }",
        "extern fn host(value: &Packet) @suspends fn bad() @suspends { let value = make() host(&value) }",
    ] {
        let source = format!("{prefix}{body}");
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.span().is_some(), "{mode:?}: {body}: {error}");
        }
        crate::aot::AotCompiler::new("invalid-owner-view").unwrap()
            .compile_module(&ast).expect_err(body);
    }
}

#[test]
fn async_owner_tcp_buffers_survive_real_io() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let source = include_str!("../../tests/fixtures/async_owner_tcp.jk");
        vm.load_source(source).unwrap_or_else(|e| {
            panic!("{mode:?}: {e} {:?}", e.span().and_then(|s| s.slice(source)))
        });
        assert_eq!(block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(), 0);
        crate::runtime::tcp::LIVE.with(|n| assert_eq!(n.get(), 0));
        clean();
    }
}

thread_local! {
    static DROPS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record_drop(id: i32) {
    DROPS.with(|events| events.borrow_mut().push(id));
}
#[test]
fn async_owner_views_drop_children_before_borrowed_parents() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record_drop",
                crate::FunctionSignature::new(vec![crate::Type::Int32], crate::Type::Unit),
                record_drop as *const u8,
            )
            .unwrap();
        }
        vm.load_source(include_str!("../../tests/fixtures/async_owner_drop.jk"))
            .unwrap();
        for (entry, expected) in [
            ("pending", vec![2, 42]),
            ("failed", vec![2, 42]),
            ("cancelled", vec![2, 42]),
            ("replaced", vec![1, 2, 3]),
            ("unstarted", vec![1]),
        ] {
            DROPS.with(|events| events.borrow_mut().clear());
            let task = vm.call_async(entry, &[]).unwrap();
            if entry == "pending" {
                assert!(!task.is_finished());
                drop(task);
            } else if entry == "failed" {
                assert_eq!(
                    block_on(task).unwrap_err().code(),
                    crate::ErrorCode::RuntimeError
                );
            } else {
                block_on(task).unwrap();
            }
            DROPS.with(|events| assert_eq!(*events.borrow(), expected, "{mode:?}: {entry}"));
            clean();
        }
    }
}
