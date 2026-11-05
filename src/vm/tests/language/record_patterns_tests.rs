//! Record pattern destructuring across execution modes, cleanup and cancellation.

use super::support::{assert_cancellation, parity_vm, CancellationSweep, READY};
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
const SOURCE: &str = include_str!("../../../../tests/fixtures/record_patterns.jk");

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(true);
    unsafe {
        vm.register_fn(
            "record_drop",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record_drop as *const u8,
        )
        .unwrap();
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            super::support::record as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}

#[test]
fn record_patterns_match_in_both_execution_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for (name, expected) in [
            ("owning_updates", 42),
            ("copy_full", 34),
            ("copy_rename_and_rest", 34),
            ("copy_reordered_and_var", 44),
            ("copy_annotated", 34),
            ("empty_record", 42),
            ("nested_copy", 34),
            ("nested_owned_rename", 303),
            ("single_evaluation", 3),
            ("owned_transfer", 342),
            ("owned_mixed_and_ignored", 7),
            ("borrowed_snapshots_and_views", 3430),
            ("borrowed_nested_projection", 217),
            ("borrowed_original_survives", 35),
            ("match_owned_payloads", 91),
            ("match_borrowed_entry", 64),
            ("generic_patterns", 38),
            ("alias_pattern", 34),
            ("tuple_combination", 37),
            ("loops_with_destructuring", 18),
        ] {
            assert_eq!(vm.call_int32_0(name), Ok(expected), "{mode:?} {name}");
        }
    }
}

#[test]
fn record_patterns_release_owners_exactly_once_in_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (entry, expected, events) in [
            ("update_drop_order", 42, vec![1, -1, 3, 2]),
            ("update_drop_parent", 7, vec![7, -1, 7]),
            ("update_failed_base", i32::MIN, vec![3]),
            ("update_failed_drop", i32::MIN, vec![1, 2, 3]),
            ("drop_copy_only_runs_immediately", 7, vec![7, -1]),
            // Ignored fields follow the record's own destruction order,
            // matching a value that simply leaves scope.
            ("ignored_fields_release_with_the_record", 5, vec![1, 2, -1]),
            // Bound leaves release in reverse publication order at scope exit.
            ("bound_fields_release_at_scope_exit", 42, vec![-1, 3, 2, 1]),
            ("dropped_field_transfers_whole", 1, vec![2, -1, 1]),
            ("failing_construction", i32::MIN, vec![9, 1]),
            ("failing_rhs_keeps_bindings_out", i32::MIN, vec![8]),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let result = vm(mode).call_int32_0(entry);
            if expected == i32::MIN {
                assert!(result.is_err(), "{mode:?} {entry}: {result:?}");
            } else {
                assert_eq!(result, Ok(expected), "{mode:?} {entry}");
            }
            assert_eq!(
                EVENTS.with(|events| events.borrow().clone()),
                events,
                "{mode:?} {entry}"
            );
            crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn record_patterns_keep_cleanup_through_interpreter_cancellation() {
    let mut vm = vm(VmMode::Interpreter);
    assert_cancellation(
        &mut vm,
        "cancellation_flow",
        20_000,
        CancellationSweep::ResourceBoundaries,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().clone()),
        || {
            // A cut before construction releases nothing; after the End every
            // ignored field has released exactly once, in record order.
            let events = EVENTS.with(|events| events.borrow().clone());
            assert!(
                events.iter().eq([1, 2].iter().take(events.len())),
                "ignored fields must release at most once each, in order: {events:?}"
            );
        },
    );
}

#[test]
fn record_patterns_survive_vm_drop_with_pending_cleanup() {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut task = vm.call_async("cancellation_flow", &[]).unwrap();
    // Advance into the destructuring sequence before destroying the VM.
    while EVENTS.with(|events| events.borrow().is_empty()) && !task.is_finished() {
        task.resume().unwrap();
    }
    drop(vm);
    drop(task);
    let events = EVENTS.with(|events| events.borrow().clone());
    for id in [1, 2] {
        assert_eq!(
            events.iter().filter(|&&event| event == id).count(),
            1,
            "resource {id} must release exactly once across VM drop: {events:?}"
        );
    }
}

fn module_sources() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "record_library",
            include_str!("../../../../tests/fixtures/record_patterns_library.jk"),
        )
        .unwrap();
    modules
}

#[test]
fn record_patterns_destructure_public_fields_across_modules() {
    let source = r#"
        import record_library as parcels.{type Parcel, make, read_tag, consume_tag, sealed}
        fn use_public() -> Int32 {
            var parcel = parcels.make(21)
            let parcels.Parcel { tag, .. } = &parcel
            let copied = read_tag(&parcel)
            let consumed = consume_tag(parcels.make(3))
            let parcels.Sealed { code, .. } = parcels.sealed(5)
            tag * 100 + copied + consumed * 10 + code
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record_drop",
                FunctionSignature::new(vec![Type::Int32], Type::Unit),
                record_drop as *const u8,
            )
            .unwrap();
        }
        vm.load_source_with_modules(source, &module_sources())
            .unwrap();
        assert_eq!(vm.call_int32_0("use_public"), Ok(2156), "{mode:?}");
    }
}

#[test]
fn record_patterns_enforce_field_visibility_across_modules() {
    for (body, message) in [
        (
            "fn bad() -> Int32 { var value = make(1); let p.Parcel { tag, payload } = value; tag }",
            "private",
        ),
        (
            "fn bad() -> Int32 { var value = make(1); let p.Parcel { tag, payload: _ } = value; tag }",
            "private",
        ),
        (
            "fn bad() -> Int32 { var value = make(1); let p.Parcel { tag } = value; tag }",
            "not visible here",
        ),
        (
            "fn bad() -> Int32 { let value = sealed(1); let p.Sealed { code } = value; code }",
            "not visible here",
        ),
    ] {
        let source = format!("import record_library as p.{{type Parcel, make, sealed}}\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error =
                VmEngine::with_mode(mode).load_source_with_modules(&source, &module_sources())
                    .expect_err(body);
            assert!(error.message().contains(message), "{body}: {error}");
        }
        let ast = crate::TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &module_sources())
            .unwrap();
        let error = crate::aot::AotCompiler::new("invalid-record-pattern-module")
            .unwrap()
            .compile_module(&ast)
            .expect_err(body);
        assert!(error.message().contains(message), "{body}: {error}");
    }
}

#[test]
fn record_patterns_reject_drop_ancestors_mut_and_borrowed_suspension() {
    let prefix = r#"
        import joky/array.{type Array}
        import joky/drop.{type Drop}
        extern fn record_drop(value: Int32)
        extern fn slow(value: Int32) @suspends -> Int32
        struct Resource { id: Int32, values: Array<Int32> }
        impl Drop for Resource { fn drop(&mut self) { record_drop(self.id) } }
        struct Holder { left: Resource, right: Resource, tag: Int32 }
        struct Point { x: Int32, y: Int32 }
    "#;
    for (body, message) in [
        // Moving an owning field whose own Drop transfers with it stays legal;
        // opening that field and moving its interior is rejected.
        (
            "fn bad() -> Int32 { let r = Holder { left: Resource { id: 1, values: Array.new::<Int32>() }, right: Resource { id: 2, values: Array.new::<Int32>() }, tag: 5 }; let Holder { left: Resource { id, .. }, .. } = r; id }",
            "implementing Drop",
        ),
        (
            "fn bad() -> Int32 { let r = Holder { left: Resource { id: 1, values: Array.new::<Int32>() }, right: Resource { id: 2, values: Array.new::<Int32>() }, tag: 5 }; let Holder { left, .. } = r; let Resource { values, .. } = left; int32(values.len()) }",
            "implementing Drop",
        ),
        // Copying Copy fields and ignoring owners keeps the record's own Drop.
        (
            "fn good() -> Int32 { let r = Holder { left: Resource { id: 1, values: Array.new::<Int32>() }, right: Resource { id: 2, values: Array.new::<Int32>() }, tag: 5 }; let Holder { tag, .. } = r; tag }",
            "unused",
        ),
        (
            "fn good2() -> Int32 { let r = Holder { left: Resource { id: 1, values: Array.new::<Int32>() }, right: Resource { id: 2, values: Array.new::<Int32>() }, tag: 5 }; let Holder { left, .. } = r; left.id }",
            "unused",
        ),
        (
            "fn bad() @suspends -> Int32 { let p = Point { x: 1, y: 2 }; let Point { x, .. } = &p; x }",
            "synchronous function",
        ),
        (
            "fn bad() -> Int32 { var p = Point { x: 1, y: 2 }; let view = &mut p; let Point { x, .. } = view; x }",
            "Exclusive references cannot be destructured",
        ),
        (
            "fn bad() -> Int32 { var p = Point { x: 1, y: 2 }; let view = &mut p; let Point { x, y } = view; x }",
            "Exclusive references cannot be destructured",
        ),
    ] {
        let source = format!("{prefix}\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_fn(
                    "record_drop",
                    FunctionSignature::new(vec![Type::Int32], Type::Unit),
                    record_drop as *const u8,
                )
                .unwrap();
                vm.register_suspending_fn(
                    "slow",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    super::support::delayed_value,
                )
                .unwrap();
            }
            let error = vm.load_source(&source);
            if message == "unused" {
                error.unwrap_or_else(|e| panic!("{body}: {e}"));
            } else {
                let error = error.expect_err(body);
                assert!(error.message().contains(message), "{body}: {error}");
            }
        }
    }
}

#[test]
fn record_patterns_survive_copy_cps_suspension() {
    let source = r#"
        struct Point { x: Int32, y: Int32 }
        extern fn delayed_value(value: Int32) @suspends -> Int32
        fn intermediate() @suspends -> Point {
            let Point { x, y } = Point { x: delayed_value(3), y: 4 }
            Point { x: x, y: y }
        }
        fn flow() @suspends -> Int32 {
            let Point { x, y } = intermediate()
            delayed_value(0);
            x * 10 + y
        }
    "#;
    let vm = parity_vm(source, VmMode::Interpreter);
    READY.with(|ready| ready.set(true));
    let mut task = vm.call_async("flow", &[]).unwrap();
    for _ in 0..10 {
        if task.is_finished() {
            break;
        }
        task.resume().unwrap();
    }
    assert!(task.is_finished(), "suspended destructuring must complete");
    assert_eq!(task.cps_result(), Some(44));
}

#[test]
#[ignore = "exhaustive: every owned destructuring instruction and cleanup boundary"]
fn exhaustive_record_patterns_owned_cps_cancellation() {
    let mut vm = vm(VmMode::Interpreter);
    assert_cancellation(
        &mut vm,
        "cancellation_flow",
        20_000,
        CancellationSweep::EveryInstruction,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().clone()),
        || {
            let events = EVENTS.with(|events| events.borrow().clone());
            // Before Holder is assembled, initialized field temporaries unwind
            // in reverse order. After assembly its ignored fields drop in order.
            assert!(
                matches!(events.as_slice(), [] | [1] | [1, 2] | [2, 1]),
                "{events:?}"
            );
            crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        },
    );
}

fn update_cancellation(sweep: CancellationSweep) {
    let mut vm = vm(VmMode::Interpreter);
    // Explicit yields are covered above; a budget cut must reach every transfer.
    vm.load_source(&SOURCE.replace("task.yield_now()", "{}"))
        .unwrap();
    assert_cancellation(
        &mut vm,
        "update_cancellation_flow",
        20_000,
        sweep,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().clone()),
        || {
            let events = EVENTS.with(|events| events.borrow().clone());
            assert!(events.iter().all(|id| [1, 2, 3].contains(id)), "{events:?}");
            for id in [1, 2, 3] {
                assert!(
                    events.iter().filter(|&&event| event == id).count() <= 1,
                    "{events:?}"
                );
            }
            crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        },
    );
}
#[test]
fn owning_record_updates_survive_suspension_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        EVENTS.with(|events| events.borrow_mut().clear());
        let vm = vm(mode);
        let mut task = vm.call_async("update_cancellation_flow", &[]).unwrap();
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 3, 2]);
    }
    update_cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: every owning record update instruction and cleanup boundary"]
fn exhaustive_owning_record_update_cancellation() {
    update_cancellation(CancellationSweep::EveryInstruction);
}
