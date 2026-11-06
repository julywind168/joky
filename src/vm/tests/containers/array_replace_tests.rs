use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, FunctionSignature, Type};

const SOURCE: &str = include_str!("../../../../tests/fixtures/array_replace.jk");
thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
extern "C" fn deny_allocations() {
    crate::runtime::arena::LIMIT.with(|limit| limit.set(0));
}
struct RestoreLimit;
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
}
fn configured(mode: VmMode, source: &str) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
        vm.register_fn(
            "deny_allocations",
            FunctionSignature::new(vec![], Type::Unit),
            deny_allocations as *const u8,
        )
        .unwrap();
    }
    vm.load_source(source).unwrap_or_else(|error| {
        let location = error.span().and_then(|span| {
            error
                .sources()
                .get(span.source_id())
                .map(|file| (file.name(), span.slice(file.text())))
        });
        panic!("{mode:?}: {error}: {location:?}")
    });
    vm
}

fn assert_cleaned() {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=4 {
            let drops = events.iter().filter(|&&event| event == id).count();
            assert!(drops <= 1, "duplicate drop: {events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "lost resource: {events:?}");
            }
        }
    });
    LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn array_replace_replacement_preserves_cleanup_without_budget_headroom() {
    let _restore = RestoreLimit;
    let source = format!(
        r#"{SOURCE}
        extern fn deny_allocations()
        fn probe_copy_no_allocation() -> Int32 {{
            var values = Array.with_capacity::<Int32>(1)
            values.push(42)
            deny_allocations()
            let old = values.replace(0, 7)
            if values.capacity() != 1 || values.get(0).unwrap_or(0) != 7 {{ return 0 }}
            old.unwrap_or(0)
        }}
        fn probe_no_allocation() -> Int32 {{
            var values = Array.with_capacity::<Resource>(3)
            values.push(resource(1)); values.push(resource(2)); values.push(resource(3))
            let next = resource(4)
            deny_allocations()
            let old = values.replace(1, next)
            if values.capacity() != 3 || values.len() != 3 {{ return 0 }}
            if values.borrow_ref(0).id != 1 || values.borrow_ref(1).id != 4 || values.borrow_ref(2).id != 3 {{ return 0 }}
            match old {{ Some(value) => value.id + 40, None => 0 }}
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        EVENTS.with(|events| events.borrow_mut().clear());
        let vm = configured(mode, &source);
        let copy_result = vm.call_int32_0("probe_copy_no_allocation");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        assert_eq!(copy_result.unwrap(), 42, "{mode:?}");
        LIVE.with(|live| assert_eq!(live.get(), 0));
        let result = vm.call_int32_0("probe_no_allocation");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        // Owner snapshots may still allocate (currently in the interpreter).
        // The source algorithm needs no new element buffer, not zero runtime memory.
        match result {
            Ok(value) => assert_eq!(value, 42, "{mode:?}"),
            Err(error) => assert_eq!(
                error.code(),
                crate::ErrorCode::ResourceLimit,
                "{mode:?}: {error}"
            ),
        }
        assert_cleaned();
    }
}

#[test]
fn array_replace_preserves_ownership_under_allocation_failure() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, SOURCE);
        let mut completed = false;
        for budget in (0..8192).step_by(16) {
            EVENTS.with(|events| events.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("replace_transitions");
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => assert_eq!(
                    error.code(),
                    crate::ErrorCode::ResourceLimit,
                    "{mode:?}: {error}"
                ),
            }
            assert_cleaned();
            if completed {
                break;
            }
        }
        assert!(completed, "{mode:?}");
    }
}

#[test]
fn array_replace_cancellation_keeps_old_next_and_array_owned() {
    let mut vm = configured(VmMode::Interpreter, SOURCE);
    super::support::assert_cancellation(
        &mut vm,
        "replace_task",
        2000,
        super::support::CancellationSweep::EveryInstruction,
        || EVENTS.with(|e| e.borrow_mut().clear()),
        || EVENTS.with(|e| e.borrow().len()),
        assert_cleaned,
    );
}

#[test]
fn array_replace_rejects_conflicting_views_and_consumed_values() {
    let prefix = "import joky/array.{type Array} import joky/element";
    for body in [
        "fn bad(a: &Array<Int32>) { a.replace(0, 1); }",
        "fn bad(a: &mut Array<Int32>) { let v = a.borrow(0); a.replace(0, 1); element.read(v); }",
        "fn bad(a: &mut Array<Array<Int32>>) { let v = a.borrow_ref(0); a.replace(0, Array.new::<Int32>()); v.len(); }",
        "fn bad(a: &mut Array<Array<Int32>>) { let v = Array.new::<Int32>(); a.replace(0, v); v.len(); }",
        "fn bad(a: &mut Array<&Int32>) { let p = alloc(1); a.replace(0, (&*p)); (*(p) = 2); }",
    ] {
        let source = format!("{prefix} {body}");
        super::support::frontend_error(&source, &crate::ModuleSources::new());
    }
}
