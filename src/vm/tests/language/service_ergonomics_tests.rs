use super::{support, VmEngine, VmMode};
use crate::{FunctionSignature, Type};

thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(id: i32) {
    EVENTS.with(|e| e.borrow_mut().push(id));
}
const OWNERS: &str = r#"
    import joky/clone.{type Clone}
    import joky/array.{type Array}
    import joky/check
    extern fn record(id: Int32)
    struct Resource { id: Int32, text: String }
    fn make(id: Int32) -> Resource {
        let resource = Resource { id: id, text: "owned" }
        record(-id)
        resource
    }

    impl Clone for Resource {
        fn clone(&self) -> Self {
            if self.id == 2 { check.panic("clone failed") }
            make(self.id + 10)
        }
    }
    #derive(Clone)
    struct Pair { first: Resource, second: Resource }
    fn clone_failure() -> Int32 {
        let source = Pair { first: make(1), second: make(2) }
        let copy = source.clone()
        copy.first.id
    }
    fn array_clone_failure() -> Int32 {
        var source = Array.new::<Resource>()
        source.push(make(1))
        source.push(make(2))
        let copy = source.clone()
        0
    }
    fn clone_work() @suspends -> Int32 {
        let source = Pair { first: make(1), second: make(3) }
        let Pair { first, second } = source.clone()
        42
    }
"#;
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source(OWNERS).unwrap_or_else(|e| panic!("{e}"));
    vm
}
fn cleaned() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn service_ergonomics_clone_failure_cleans_partial_copies() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in ["clone_failure", "array_clone_failure"] {
            EVENTS.with(|e| e.borrow_mut().clear());
            assert!(vm
                .call_int32_0(entry)
                .unwrap_err()
                .message()
                .contains("clone failed"));
            cleaned();
        }
    }
}
fn cancellation(sweep: support::CancellationSweep) {
    support::assert_cancellation(
        &mut vm(VmMode::Interpreter),
        "clone_work",
        20_000,
        sweep,
        || EVENTS.with(|e| e.borrow_mut().clear()),
        || EVENTS.with(|e| e.borrow().clone()),
        cleaned,
    );
}
#[test]
fn service_ergonomics_clone_and_destructure_cancellation() {
    cancellation(support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: derived clone and CPS field ownership transfers"]
fn exhaustive_service_ergonomics_clone_and_destructure_cancellation() {
    cancellation(support::CancellationSweep::EveryInstruction);
}
