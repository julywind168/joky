use super::{support, VmEngine, VmMode};
use crate::runtime::{arena::LIVE, driver::block_on};
const SOURCE: &str = include_str!("../../../../tests/fixtures/async_owner_borrows.jk");
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
fn async_owner_borrows_reject_conflicts() {
    for body in ["let item = values.borrow_ref(0); values.push(9); let n = *item"] {
        let source = format!("import joky/array.{{type Array}} import joky/task import joky/slice fn bad() @suspends {{ var values = Array.new::<Int32>(); values.push(42); {body} }}");
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

const VIEWS: &str = include_str!("../../../../tests/fixtures/async_owner_views.jk");
const DETACHED: &str = include_str!("../../../../tests/fixtures/detached_owners.jk");

#[test]
fn detached_owners_borrow_independently() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(DETACHED).unwrap_or_else(|e| {
            panic!(
                "{mode:?}: {e} {:?}",
                e.span().and_then(|s| s.slice(DETACHED))
            )
        });
        assert_eq!(
            block_on(vm.prepare_async("exercise", &[]).unwrap()).unwrap(),
            42
        );
        clean();
    }
}

#[test]
fn detached_tcp_owners_borrow_independently() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../../../tests/fixtures/detached_tcp_owners.jk"
        ))
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(), 0);
        crate::runtime::tcp::LIVE.with(|n| assert_eq!(n.get(), 0));
        clean();
    }
}

fn cancel_detached(sweep: support::CancellationSweep) {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(DETACHED).unwrap();
    support::assert_cancellation(
        &mut vm,
        "exercise",
        10000,
        sweep,
        || {},
        || LIVE.with(|n| n.get()),
        clean,
    );
}

#[test]
fn detached_owners_cancel_at_resource_boundaries() {
    cancel_detached(support::CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel detached owner groups and independent loans at every instruction"]
fn exhaustive_detached_owners_cancel_every_instruction() {
    cancel_detached(support::CancellationSweep::EveryInstruction);
}

#[test]
fn detached_owners_survive_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(DETACHED).unwrap();
        let task = vm.call_async("exercise", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        assert_eq!(block_on(task).unwrap(), 42);
        clean();
    }
}

#[test]
fn detached_owners_keep_alias_and_escape_checks() {
    let prefix = r#"
        import joky/array.{type Array}
        import joky/option.{type Option}
        import joky/task
        struct Buckets { left: Array<Int32>, right: Array<Int32> }
        fn pair() -> (Array<Int32>, Array<Int32>) { (Array.new::<Int32>(), Array.new::<Int32>()) }
        fn relay(values: Array<Int32>) -> (Array<Int32>, Array<Int32>) { (values, Array.new::<Int32>()) }
        fn append(values: &mut Array<Int32>) @suspends { task.yield_now(); values.push(42) }
    "#;
    for (body, message) in [
        ("fn bad() @suspends { var (a, b) = pair(); join { | append(&mut a) | append(&mut a) } }", "conflicts"),
        ("fn bad() { var (a, b) = pair(); let view = &a; let (c, d) = relay(a); view.len(); }", "conflicts"),
        ("fn bad(input: &Array<Int32>) -> &Array<Int32> { let (a, b) = pair(); return &a }", "conflicts"),
        ("fn bad() { let (a, b) = pair(); let moved = a; a.len(); }", "moved"),
        ("fn first(values: Array<&Int32>) -> Option<&Int32> { let (values, _) = (values, Array.new::<Int32>()); values.get(0) } fn bad() { var values = Array.new::<&Int32>(); values.push(&*(alloc(0))); let local = 42; let _ = first({ values.push(&local); values }) }", "local borrow"),
        ("fn first(value: &Int32) -> Option<&Int32> { var values = Array.new::<&Int32>(); values.push(value); let (values, _) = (values, Array.new::<Int32>()); values.get(0) } fn bad() -> Option<&Int32> { let local = 42; first(&local) }", "local"),
        ("fn bad() @suspends { let (left, right) = pair(); var buckets = Buckets { left: left, right: right }; join { | append(&mut buckets.left) | append(&mut buckets.right) } }", "conflicts"),
    ] {
        let source = format!("{prefix}{body}");
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.message().contains(message), "{mode:?}: {body}: {error}");
            assert!(error.span().is_some());
        }
        let error = crate::aot::AotCompiler::new("invalid-detached-owner").unwrap()
            .compile_module(&ast).expect_err(body);
        assert!(error.message().contains(message), "{body}: {error}");
    }
}

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
fn async_owner_views_reject_aliases_and_escapes() {
    let prefix = r#"
        import joky/array.{type Array}
        import joky/task
        import joky/slice
        struct Packet { bytes: Array<Int32>, tag: Int32 }
        type Packets = Array<Packet>
        fn make() -> Packet { Packet { bytes: Array.new::<Int32>(), tag: 1 } }
        fn view<T>(values: &Array<T>) -> &T { values.borrow_ref(0) }
        fn identity<T>(value: &T) -> &T { value }
        fn inner(value: &Packet) -> &Array<Int32> { &value.bytes }
        fn read(value: &Packet) @suspends { task.yield_now(); let n = value.tag }
        fn write(value: &mut Packet) @suspends { task.yield_now(); value.tag = 2 }
    "#;
    for body in [
        "fn bad() @suspends { var value = make(); join { | write(&mut value) | read(&value) } }",
        "fn bad() @suspends { var value = make(); join { | write(&mut value) | write(&mut value) } }",
        "fn bad() @suspends { var value = make(); scope { let r = &mut value; branch { write(r) }; value.tag = 3 } }",
        "fn bad() @suspends { var value = make(); scope { let r = &value; branch { read(r) }; let moved = value } }",
        "fn bad() @suspends { let r = { let value = make(); &value }; read(r) }",
        "fn bad(value: &Packet) @suspends -> &Packet { task.yield_now(); value }",
        "fn bad(value: &Packet) @suspends { alloc(value); }",
        "extern fn host(value: &Packet) @suspends fn bad() @suspends { let value = make(); host(&value) }",
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
        let source = include_str!("../../../../tests/fixtures/async_owner_tcp.jk");
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
        vm.load_source(include_str!(
            "../../../../tests/fixtures/async_owner_drop.jk"
        ))
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
