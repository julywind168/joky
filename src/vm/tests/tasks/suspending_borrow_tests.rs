use super::{support, VmEngine, VmMode};
use crate::runtime::{arena, driver::block_on};

const SOURCE: &str = include_str!("../../../../tests/fixtures/suspending_borrows.jk");
const CLEANUP: &str = include_str!("../../../../tests/fixtures/suspending_borrow_cleanup.jk");

fn clean() {
    arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn suspending_borrows_resume_after_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let task = vm.call_async("exercise", &[]).unwrap();
        assert!(!task.is_finished());
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        assert_eq!(block_on(task).unwrap(), 42);
        clean();
    }
}

fn cancel(sweep: support::CancellationSweep) {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(SOURCE).unwrap();
    support::assert_cancellation(
        &mut vm,
        "exercise",
        20000,
        sweep,
        || {},
        || arena::LIVE.with(|n| n.get()),
        clean,
    );
}

#[test]
fn suspending_borrows_cancel_at_resource_boundaries() {
    cancel(support::CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel local, element and Slice loans at every instruction"]
fn exhaustive_suspending_borrows_cancel_every_instruction() {
    cancel(support::CancellationSweep::EveryInstruction);
}

thread_local! {
    static DROPS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record_drop(id: i32) {
    DROPS.with(|events| events.borrow_mut().push(id));
}

#[test]
fn suspending_borrows_clean_children_before_local_and_element_owners() {
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
        vm.load_source(CLEANUP).unwrap();
        for entry in ["pending", "failed", "cancelled", "unstarted", "dynamic"] {
            DROPS.with(|events| events.borrow_mut().clear());
            let mut task = vm.call_async(entry, &[]).unwrap();
            if entry == "pending" || entry == "dynamic" {
                assert!(!task.is_finished());
                if entry == "dynamic" {
                    task.resume().unwrap();
                }
                drop(task);
            } else if entry == "failed" {
                assert_eq!(
                    block_on(task).unwrap_err().code(),
                    crate::ErrorCode::RuntimeError
                );
            } else {
                block_on(task).unwrap();
            }
            let expected = if entry == "unstarted" {
                vec![1]
            } else {
                vec![2, 42]
            };
            DROPS.with(|events| assert_eq!(*events.borrow(), expected, "{mode:?}: {entry}"));
            clean();
        }
        for exit in 0..4 {
            DROPS.with(|events| events.borrow_mut().clear());
            block_on(vm.call_async("joined", &[exit]).unwrap()).unwrap();
            DROPS.with(|events| assert_eq!(*events.borrow(), [2, 42], "{mode:?}: {exit}"));
            clean();
        }
    }
}

#[test]
fn suspending_borrows_reject_conflicts_retention_and_short_lifetimes() {
    let prefix = r#"
        import joky/task
        import joky/array.{type Array}
        import joky/slice.{type Slice}
        import joky/mem
        struct Holder { value: &Int32 }
        fn read(value: &Int32) @suspends -> Int32 { task.yield_now(); *value }
        fn write(value: &mut Int32) @suspends { task.yield_now(); *value = 42 }
        fn retain(value: &Int32) @suspends { alloc(Holder { value: value }); task.yield_now() }
        fn mixed(prefix: (Int32, Int32), local: &Int32, stored: &Int32) @suspends -> Int32 {
            alloc(Holder { value: stored }); task.yield_now(); *local + prefix.0 + prefix.1
        }
        fn slice_read(values: &Slice<Int32>) @suspends { task.yield_now(); slice.get(values, 0); }
        fn view(values: &Array<Int32>) -> &Int32 { values.borrow_ref(0) }
    "#;
    for body in [
        "var value = 1; join { write(&mut value), read(&value) };",
        "var value = 1; join { write(&mut value), write(&mut value) };",
        "var value = 1; scope { let r = &mut value; branch { write(r) }; value = 2 }",
        "scope { { let value = 1; let r = &value; branch { read(r); } } }",
        "scope { var i = 0; while i < 2 { let value = i; let r = &value; branch { read(r); }; i += 1 } }",
        "let value = 1; retain(&value)",
        "let value = 1; join { retain(&value) }",
        "let value = 1; scope { let r = &value; branch { retain(r) } }",
        "let value = 1; join { mixed((0, 0), &value, &value) };",
        "let value = 1; scope { let r = &value; branch { mixed((0, 0), r, r); } }",
        "let value = 1; let r = { let inner = 2; &inner }; read(r);",
        "var values = [1]; let r = values.borrow_ref(0); task.yield_now(); values.push(2); read(r);",
        "var values = [1]; let r = values.borrow_ref(0); task.yield_now(); values.clear(); read(r);",
        "var values = [1]; let r = values.borrow_ref(0); task.yield_now(); mem.drop(values); read(r);",
        "var values = [1]; let r = view(&values); task.yield_now(); values = [2]; read(r);",
        "var values = [1, 2]; join { write(values.borrow_mut_ref(0)), write(values.borrow_mut_ref(1)) };",
        "var values = [1]; let r = values.borrow_mut_ref(0); scope { branch { write(r) }; values.clear() }",
        "var values = [1]; let r = values.slice(0, 1); scope { branch { slice_read(r) }; values.push(2) }",
        "scope { { var values = [1]; let r = values.slice(0, 1); branch { slice_read(r) } } }",
        "let r = { let values = [1]; values.borrow_ref(0) }; read(r);",
        "let values = [1]; retain(values.borrow_ref(0))",
        "let values = [1]; join { retain(values.borrow_ref(0)) }",
        "let values = [1]; let r = values.borrow_ref(0); scope { branch { retain(r) } }",
        "var values = [1]; let r = values.slice_mut(0, 1); let item = slice.borrow_mut_ref(r, 0); write(item); values.clear(); slice.get(r, 0);",
    ] {
        let source = format!("{prefix} fn bad() @suspends {{ {body} }}");
        let ast = crate::parser::parse(&source).unwrap();
        let error = VmEngine::new_interpreter().load_module(&ast).expect_err(body);
        assert!(error.span().is_some(), "{body}: {error}");
        assert!(
            error.message().to_lowercase().contains("borrow") || error.message().contains("retained"),
            "{body}: {error}"
        );
    }
}
