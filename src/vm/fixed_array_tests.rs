use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, Type};
const SOURCE: &str = include_str!("../../tests/fixtures/fixed_array_f2a_cleanup.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn note(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
fn configured(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "note",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            note as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
    vm
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn cleaned() {
    arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    EVENTS.with(|events| {
        let events = events.borrow();
        assert!(
            events.iter().filter(|&&n| n == -1).count() <= 1,
            "duplicate cleanup: {events:?}"
        );
        let calls: Vec<_> = events.iter().copied().filter(|&n| n > 0).collect();
        assert!(
            [1, 2, 3, 4].starts_with(&calls),
            "callback replay: {events:?}"
        );
    });
}
struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn fixed_array_f2a_failures_and_budget_reuse() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for (name, events) in [
            ("construct", vec![1, 2, 3, 4, -1]),
            ("zero_env", vec![-1]),
            ("early", vec![-1]),
        ] {
            reset();
            assert_eq!(vm.call_int32_0(name).unwrap(), 42);
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events, "{mode:?} {name}"));
            arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        reset();
        assert!(vm.call_int32_0("fail_init").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3, -1]));
        cleaned();
        reset();
        assert!(vm.call_int32_0("assignment_fail").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3]));
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        reset();
        assert!(vm.call_int32_0("fail_cleanup").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, -2]));
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        let mut successful = false;
        let mut failed_after_callback = false;
        for budget in (0..8192).step_by(64) {
            arena::LIMIT.with(|limit| limit.set(budget));
            reset();
            match vm.call_int32_0("construct") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    successful = true;
                }
                Err(error) => {
                    assert_eq!(
                        error.code(),
                        ErrorCode::ResourceLimit,
                        "{mode:?} {budget}: {error}"
                    );
                    failed_after_callback |= EVENTS.with(|events| events.borrow().contains(&1));
                }
            }
            cleaned();
        }
        assert!(successful && failed_after_callback);
        arena::LIMIT.with(|limit| limit.set(8192));
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/fixed_array_f2a.jk"))
            .unwrap();
        assert_eq!(vm.call_int32_0("reuse").unwrap(), 42);
        arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}
fn cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = configured(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "task",
        10000,
        sweep,
        reset,
        || EVENTS.with(|events| events.borrow().clone()),
        cleaned,
    );
}
#[test]
fn fixed_array_f2a_cancellation_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel fixed-array initialization at every interpreter instruction"]
fn exhaustive_fixed_array_f2a_cancellation() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn fixed_array_f2a_rejects_unopened_contexts() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let types = crate::TypeRegistry::default();
    for body in [
        "fn bad(x: FixedArray<FixedArray<Int32,2>,2>) {}",
        "struct Outer { x: FixedArray<Int32,2> } fn bad() { let a = Outer { x: fixed.repeat::<Int32,2>(1) } }",
        "fn bad() { let a = Some(fixed.repeat::<Int32,2>(1)) }",
        "fn bad() { let a = (fixed.repeat::<Int32,2>(1), 2) }",
        "import joky/array.{type Array} fn bad() { let a = Array.new::<FixedArray<Int32,2>>() }",
        "fn bad() { let a = alloc(fixed.repeat::<Int32,2>(1)) }",
        "import joky/arena.{type Arena} fn bad() { let arena = Arena.new() let a = arena.alloc(fixed.repeat::<Int32,2>(1)) }",
        "fn bad() { let a = fixed.repeat::<Int32,2>(1) let f = || { a.len() } f(); }",
        "fn bad() @suspends { let a = fixed.repeat::<Int32,2>(1) }",
        "fn take(a:FixedArray<Int32,2>) -> FixedArray<Int32,2> { a } fn bad() { let f = take f(fixed.repeat::<Int32,2>(1)); }",
        "fn bad() -> &FixedArray<Int32,2> { let a = fixed.repeat::<Int32,2>(1) &a }",
        "fn bad() { var a = fixed.repeat::<Int32,2>(1) let r = &a a.set(0,2) r.len(); }",
        "fn bad() { let a = fixed.repeat::<String,0>(\"no\") }",
        "fn bad(x:&Int32) { let a = fixed.repeat::<&Int32,0>(x) }",
        "fn bad() { let a = fixed.repeat::<Int32,2>(1) a.slice(0,1); }",
        "fn bad() { let text = \"once\" let f = move |i:UInt64| { let owned = text int32(i) } fixed.from_fn::<Int32,2>(f); }",
        "fn wait(i:UInt64) @suspends -> Int32 { int32(i) } fn bad() { fixed.from_fn::<Int32,2>(wait); }",
        "extern fn foreign(a: FixedArray<Int32,2>)",
    ] {
        let source = format!("import joky/fixed_array as fixed.{{type FixedArray}}\n{body}");
        let result = crate::parser::parse(&source).map_err(crate::Error::from)
            .and_then(|module| PreparedModule::new(&module, &types, &BindingRegistry::default()))
            .and_then(|prepared| prepared.plan(&Default::default()));
        assert!(result.is_err(), "accepted: {source}");
    }
}
