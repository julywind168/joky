//! Parameter mutability belongs to the callee binding, not the call signature.
use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/mutable_parameters.jk");

#[test]
fn mutable_parameters_preserve_values_ownership_and_suspension() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE)
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        // Replacement must release both the old and new owner on failure.
        let error = vm.call_int32_0("fail");
        assert_eq!(error.unwrap_err().message(), "Division by zero");
        LIVE.with(|n| assert_eq!(n.get(), 0));
        let mut task = vm.call_async("main", &[]).unwrap();
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(0));
        drop(task);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        for resumes in 0..2 {
            let mut task = vm.call_async("main", &[]).unwrap();
            for _ in 0..resumes {
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            drop(task);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn mutable_parameters_keep_immutable_borrow_and_move_restrictions() {
    for (source, message) in [
        ("fn f(value: Int32) { value = 2 }", "immutable"),
        (
            "fn f(value: Int32) { let p = &mut value; *p = 2 }",
            "var binding",
        ),
        ("fn f(var value: &Int32) { *value = 2 }", "shared"),
        ("fn f(var value: Int32) -> &Int32 { &value }", "borrow"),
        (
            "fn f(var value: Int32) { let p = &value; value = 2; *p; }",
            "borrow",
        ),
        (
            "fn f(var value: String) { let moved = value; value.byte_length(); }",
            "moved",
        ),
        (
            "fn take(var value: String) {} fn f() { let value = \"x\"; take(value); take(value) }",
            "moved",
        ),
        (
            "fn f() { let operation = |value: Int32| { value = 2 }; operation(1) }",
            "immutable",
        ),
        (
            "struct S {} impl S { fn f(var &mut self) {} }",
            "borrowed receiver",
        ),
    ] {
        let error = super::support::frontend_error(source, &crate::ModuleSources::new());
        assert!(error.message().contains(message), "{source}\n{error}");
    }
}
