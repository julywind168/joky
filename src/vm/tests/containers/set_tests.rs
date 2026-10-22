//! Public Set semantics and ownership; Map remains the sole storage implementation.
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};
use std::cell::RefCell;
thread_local! { static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) }; }
extern "C" fn record(value: i32) {
    EVENTS.with(|e| e.borrow_mut().push(value));
}
fn configured(mode: VmMode, source: &str) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(false);
    // SAFETY: The callback has the declared synchronous Int32 -> Unit ABI.
    unsafe {
        vm.register_fn(
            "record_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(source, &crate::ModuleSources::new())
        .unwrap_or_else(|e| {
            panic!(
                "{mode:?}: {e} {:?}",
                e.span().and_then(|s| e
                    .sources()
                    .get(s.source_id())
                    .map(|f| (f.name(), s.slice(f.text()))))
            )
        });
    vm
}
fn reset() {
    EVENTS.with(|e| e.borrow_mut().clear());
}
fn assert_live_zero() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
pub(crate) const CASES: &[(&str, Option<&str>, &[i32])] = &[
    ("boundaries", None, &[]),
    ("capacity_reuse", None, &[]),
    ("strings", None, &[]),
    ("duplicates", None, &[]),
    ("transitions", None, &[]),
    ("bulk", None, &[]),
    ("union_case", None, &[]),
    ("intersection_case", None, &[]),
    ("difference_case", None, &[]),
    ("symmetric_difference_case", None, &[]),
    ("unit_keys", None, &[]),
    ("no_protocols", None, &[]),
    ("bulk_eq_failure", Some("Set Eq failure"), &[1, 2, 3]),
    ("union_eq_failure", Some("Set Eq failure"), &[1, 2]),
    ("intersection_eq_failure", Some("Set Eq failure"), &[1, 2]),
    ("difference_eq_failure", Some("Set Eq failure"), &[1, 2]),
    (
        "symmetric_difference_eq_failure",
        Some("Set Eq failure"),
        &[1, 2],
    ),
    (
        "intersection_drop_failure",
        Some("Set Drop failure"),
        &[1, 2],
    ),
    ("hash_failure", Some("Set Hash failure"), &[1, 2]),
    ("eq_failure", Some("Set Eq failure"), &[1, 2]),
    ("clear_failure", Some("Set Drop failure"), &[1, 2, 3]),
    ("duplicate_drop_failure", Some("Set Drop failure"), &[1, 1]),
    ("query_failure", Some("Set Hash failure"), &[1, 2]),
    (
        "capacity_failure",
        Some("Map capacity exceeds the supported limit"),
        &[],
    ),
];
pub(crate) fn fixture_source(entry: &str) -> String {
    format!(
        "{} fn probe() -> Int32 {{ {entry}::<Unit>() }} fn task() @suspends -> Int32 {{ probe() }}",
        include_str!("../../../../tests/fixtures/set.jk")
    )
}
/// Share generic specializations across independent calls in one compiled module.
pub(crate) fn fixture_entries(entries: &[&str]) -> String {
    let entries: Vec<_> = entries
        .iter()
        .map(|&entry| (format!("probe_{entry}"), entry))
        .collect();
    super::support::generic_fixture_entries(
        include_str!("../../../../tests/fixtures/set.jk"),
        &entries,
    )
}
fn assert_owners() {
    EVENTS.with(|e| {
        let events = e.borrow();
        assert!(
            events
                .iter()
                .all(|&n| (-5..=-1).contains(&n) || (1..=5).contains(&n) || n == 100 || n == 101),
            "unexpected event: {events:?}"
        );
        let mut unrecorded = 0;
        for id in 1..=5 {
            let created = events.iter().filter(|&&n| n == -id).count();
            let dropped = events.iter().filter(|&&n| n == id).count();
            assert!(created <= dropped && dropped <= 1, "owner {id}: {events:?}");
            unrecorded += dropped - created;
        }
        // The interrupted constructor can be owned before its host callback.
        assert!(unrecorded <= 1, "unrecorded owners: {events:?}");
    });
    assert_live_zero();
}
#[test]
fn set_executes_core_views_batches_algebra_and_failures() {
    let entries: Vec<_> = CASES.iter().map(|case| case.0).collect();
    let source = fixture_entries(&entries);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source);
        for &(entry, error, drops) in CASES {
            let name = format!("probe_{entry}");
            reset();
            match error {
                Some(message) => {
                    assert_eq!(
                        vm.call_int32_0(&name).unwrap_err().message(),
                        message,
                        "{mode:?} {entry}"
                    );
                    EVENTS.with(|e| assert!(!e.borrow().contains(&99)));
                }
                None => assert_eq!(vm.call_int32_0(&name).unwrap(), 42, "{mode:?} {entry}"),
            }
            if drops.is_empty() {
                assert_owners();
            } else {
                EVENTS.with(|e| {
                    let mut actual = e.borrow().clone();
                    actual.sort();
                    let mut expected = drops.to_vec();
                    expected.sort();
                    assert_eq!(actual, expected, "{mode:?} {entry}");
                });
                assert_live_zero();
            }
            EVENTS.with(|e| {
                let events = e.borrow();
                if entry == "duplicates" {
                    assert_eq!(*events, vec![-1, -2, 2, -3, 100, 1, 101, 3]);
                }
                if entry == "transitions" {
                    let a = events.iter().position(|&n| n == 100).unwrap();
                    let b = events.iter().position(|&n| n == 101).unwrap();
                    assert_eq!(b, a + 1);
                }
                let early: &[i32] = match entry {
                    "union_case" => &[3],
                    "intersection_case" => &[1],
                    "difference_case" => &[2],
                    "symmetric_difference_case" => &[2, 3],
                    "bulk" => &[2, 4],
                    _ => &[],
                };
                if !early.is_empty() {
                    let end = events
                        .iter()
                        .position(|&n| n == if entry == "bulk" { 100 } else { 101 })
                        .unwrap();
                    for id in early {
                        assert!(events[..end].contains(id), "{entry}: {events:?}");
                    }
                }
            });
        }
    }
}

pub(crate) fn model_source(seed: u32, steps: i32) -> String {
    format!("{} fn probe() -> Int32 {{ model({seed},{steps}) }} fn matrix() -> Int32 {{ algebra_matrix(8) }} fn row(a: Int32) -> Int32 {{ algebra_row(uint32(a),256) }}", include_str!("../../../../tests/fixtures/set_model.jk"))
}
pub(crate) fn model_events(seed: u32, steps: i32) -> Vec<i32> {
    let mut set = std::collections::BTreeSet::new();
    let mut rng = seed;
    let mut events = Vec::new();
    for _ in 0..steps {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        let op = (rng >> 16) & 7;
        let value = ((rng >> 8) & 7) as i32;
        let mask = (rng >> 24) & 255;
        let other: std::collections::BTreeSet<_> =
            (0..8).filter(|i| mask & (1 << i) != 0).collect();
        let result = match op {
            0 => i32::from(set.insert(value)),
            1 => i32::from(set.remove(&value)),
            2 => i32::from(set.contains(&value)),
            3 => 0,
            4 => {
                set.clear();
                0
            }
            5 => {
                set = set.union(&other).copied().collect();
                0
            }
            6 => {
                set = set.intersection(&other).copied().collect();
                0
            }
            _ => {
                set = set.symmetric_difference(&other).copied().collect();
                0
            }
        };
        let membership = set.iter().fold(0, |bits, value| bits | (1 << value));
        events.extend([result, set.len() as i32, membership]);
    }
    events
}
#[test]
fn set_model_matches_membership_and_algebra_for_each_state() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for seed in [123456789, 0xfedcba98] {
            let vm = configured(mode, &model_source(seed, 256));
            reset();
            assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
            EVENTS
                .with(|e| assert_eq!(*e.borrow(), model_events(seed, 256), "{mode:?} seed={seed}"));
            assert_eq!(vm.call_int32_0("matrix").unwrap(), 42);
            assert_live_zero();
        }
    }
}
#[test]
#[ignore = "exhaustive: long Set model and full eight-element algebra matrix"]
fn exhaustive_set_model_checks_membership_and_algebra() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for seed in [123456789, 0xfedcba98] {
            let source = model_source(seed, 4096);
            let vm = configured(mode, &source);
            reset();
            assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
            EVENTS.with(|e| assert_eq!(*e.borrow(), model_events(seed, 4096)));
            // The full Cartesian matrix has 65,536 pairs; run it once per backend.
            if seed == 123456789 {
                for row in 0..256 {
                    assert_eq!(
                        vm.call_int32_1("row", row).unwrap(),
                        42,
                        "{mode:?} row={row}"
                    );
                    assert_live_zero();
                }
            }
            assert_live_zero();
        }
    }
}

fn view_modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "views",
            include_str!("../../../../tests/fixtures/set_views.jk"),
        )
        .unwrap();
    modules
}
fn rejected(body: &str, message: &str) {
    let source=format!("import joky/set.{{type Set}} import joky/array.{{type Array}} import joky/slice as slices import joky/option.{{Some,None}} import joky/string import views {body}");
    let error = super::support::frontend_error(&source, &view_modules());
    assert!(error.message().contains(message), "{body}: {error}");
}
#[test]
fn set_rejects_invalid_keys_mutable_elements_escapes_and_consuming_aliases() {
    for (body,message) in [
        ("fn bad() { Set.new::<&Int32>(); }","reference"),
        ("struct Shared { value: &Int32 } fn bad() { Set.new::<Shared>(); }","reference"),
        ("enum Shared { Empty, Value(&Int32) } fn bad() { Set.new::<Shared>(); }","reference"),
        ("fn bad() { Set.new::<&mut Int32>(); }","Copy"),
        ("struct Owned { data: String, value: &Int32 } fn bad() { Set.new::<Owned>(); }","reference"),
        ("struct Key { id: Int32 } fn bad(s: &Set<Key>) { s.contains(Key { id: 1 }); }","Borrow"),
        ("fn bad(s: &Set<String>) { slices.get(s.elements(),0); }","Copy"),
        ("fn bad(s: &Set<String>) { slices.borrow_mut_ref(s.elements(),0); }","type mismatch"),
        ("fn bad(s: &Set<Int32>) { slices.set(s.elements(),0,42); }","type mismatch"),
        ("fn bad(s: &Set<Int32>) { s.items.len(); }","private"),
        ("fn bad(s: &mut Set<Int32>) { let v=s.elements(); s.insert(2); slices.len(v); }","borrow"),
        ("fn bad(s: &mut Set<Int32>) { let v=s.elements(); s.remove(2); slices.len(v); }","borrow"),
        ("fn bad(s: &mut Set<Int32>) { let v=s.elements(); s.pop(); slices.len(v); }","borrow"),
        ("fn bad(s: &mut Set<Int32>) { let v=s.elements(); s.reserve(0); slices.len(v); }","borrow"),
        ("fn bad(s: &mut Set<Int32>) { let v=s.elements(); s.clear(); slices.len(v); }","borrow"),
        ("fn bad(s: Set<Int32>) { let v=s.elements(); s.release(); slices.len(v); }","borrow"),
        ("fn bad(s: Set<Int32>) { let v=s.elements(); let moved=s; slices.len(v); }","borrow"),
        ("fn bad(s: Set<Int32>) { let v=s.elements(); s.into_array(); slices.len(v); }","borrow"),
        ("fn bad() -> &slices.Slice<Int32> { var s=Set.new::<Int32>(); s.insert(1); s.elements() }","borrow"),
        ("fn bad() -> &String { var s=Set.new::<String>(); s.insert(\"x\"); slices.borrow_ref(s.elements(),0) }","borrow"),
        ("fn bad(s: Set<Int32>) { s.union(s); }","moved"),
        ("fn bad(s: Set<Int32>) { s.symmetric_difference(s); }","moved"),
        ("fn bad(s: Set<Int32>) { s.intersection(&s); }","moved"),
        ("fn bad(s: Set<Int32>) { s.difference(&s); }","moved"),
        ("fn bad(s: Set<String>) { s.release(); s.len(); }","moved"),
        ("fn bad(s: &mut Set<String>) { let q=\"x\"; s.insert(q); q.byte_length(); }","moved"),
        ("fn bad(s: &mut views.Collection<Int32>) { let v=views.elements(s); views.remove(s,1); slices.len(v); }","borrow"),
        ("fn bad(s: &mut views.Collection<String>) { slices.borrow_mut_ref(views.elements(s),0); }","type mismatch"),
        ("fn bad(s: views.Collection<Int32>) { views.intersection(s,&s); }","moved"),
        ("fn bad() { views.new::<&Int32>(); }","reference"),
    ] { rejected(body,message); }
}
#[test]
fn set_cross_module_noescape_queries_views_and_algebra_preserve_ownership() {
    for body in [
        "var s=views.new::<String>(); views.insert(&mut s,\"alpha\"); var query=\"alpha\"; let view=views.elements(&s); let n=int32(slices.borrow_ref(view,0).byte_length()); if !views.contains(&s,query) { return 0 }; views.remove(&mut s,query); query=\"replacement\"; n+int32(query.byte_length())+26",
        "var input=Array.new::<Int32>(); input.push(20); input.push(22); let s=views.import_array(input); let out=views.export(s); out.get(0).unwrap_or(0)+out.get(1).unwrap_or(0)",
        "var left=views.new::<String>(); views.insert(&mut left,\"alpha\"); let output={ var right=views.new::<String>(); views.insert(&mut right,\"alpha\"); views.intersection(left,&right) }; int32(slices.borrow_ref(views.elements(&output),0).byte_length())+37",
    ] {
        let source=format!("import views import joky/array.{{type Array}} import joky/slice as slices import joky/string fn probe() -> Int32 {{ {body} }}");
        let mut types=crate::TypeRegistry::default();types.prelude=false;
        let ast=types.resolve_with_modules(&crate::parser::parse(&source).unwrap(),&view_modules()).unwrap();
        for mode in [VmMode::Jit,VmMode::Interpreter] {
            let mut vm=VmEngine::with_mode(mode);vm.load_module(&ast).unwrap_or_else(|e|panic!("{e}"));
            assert_eq!(vm.call_int32_0("probe").unwrap(),42);assert_live_zero();
        }
        #[cfg(unix)]
        crate::backends::aot::tests::assert_sync_int32(&ast,"probe",42);
    }
}

const OWNING_CASES: [&str; 6] = [
    "transitions",
    "bulk",
    "union_case",
    "intersection_case",
    "difference_case",
    "symmetric_difference_case",
];

#[test]
fn set_allocation_failures_clean_partial_construction_and_algebra() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
        }
    }
    let _restore = Restore;
    let source = fixture_entries(&OWNING_CASES);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source);
        for entry in OWNING_CASES {
            let mut completed = false;
            let mut partial = false;
            let mut transferred = false;
            for budget in (0..65536).step_by(16) {
                reset();
                crate::runtime::arena::LIMIT.with(|n| n.set(budget));
                let result = vm.call_int32_0(&format!("probe_{entry}"));
                crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
                match result {
                    Ok(value) => {
                        assert_eq!(value, 42);
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(
                            error.code(),
                            crate::ErrorCode::ResourceLimit,
                            "{mode:?} {entry}: {error}"
                        );
                        partial |= EVENTS.with(|e| e.borrow().contains(&-1));
                        transferred |= EVENTS
                            .with(|e| e.borrow().contains(&if entry == "bulk" { -5 } else { 100 }));
                    }
                }
                assert_owners();
                if completed {
                    break;
                }
            }
            assert!(completed && partial, "{mode:?} {entry}");
            if entry != "transitions" {
                assert!(
                    transferred,
                    "{mode:?} {entry}: no failure after both inputs were constructed"
                );
            }
        }
    }
}
fn cancellation(entry: &str, sweep: super::support::CancellationSweep) {
    let mut vm = configured(VmMode::Interpreter, &fixture_source(entry));
    super::support::assert_cancellation(
        &mut vm,
        "task",
        30000,
        sweep,
        reset,
        || EVENTS.with(|events| events.borrow().len()),
        assert_owners,
    );
}
#[test]
fn set_cancellation_covers_owner_and_bulk_transfer_boundaries() {
    for entry in OWNING_CASES {
        cancellation(entry, super::support::CancellationSweep::ResourceBoundaries);
    }
}
#[test]
#[ignore = "exhaustive: cancel Set at every interpreter instruction"]
fn exhaustive_set_cancels_at_each_instruction() {
    for entry in OWNING_CASES {
        cancellation(entry, super::support::CancellationSweep::EveryInstruction);
    }
}
#[test]
fn set_instruction_exhaustion_and_reload_retain_owners_and_old_code() {
    for entry in OWNING_CASES {
        let mut vm = configured(VmMode::Interpreter, &fixture_source(entry));
        let mut complete = false;
        let mut partial = false;
        for budget in (1..30000).step_by(64) {
            vm.set_interpreter_limits(super::InterpreterLimits {
                instruction_budget: Some(budget),
                ..Default::default()
            })
            .unwrap();
            reset();
            match vm.call_int32_0("probe") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    complete = true;
                }
                Err(error) => {
                    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                    partial |= EVENTS.with(|e| e.borrow().contains(&-1));
                }
            }
            assert_owners();
            if complete {
                break;
            }
        }
        assert!(complete && partial);
        for finish in [false, true] {
            let mut vm = configured(VmMode::Interpreter, &fixture_source(entry));
            vm.set_interpreter_limits(super::InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
            reset();
            let mut task = vm.call_async("task", &[]).unwrap();
            while !EVENTS.with(|e| e.borrow().contains(&-3)) {
                assert!(!task.is_finished());
                task.resume().unwrap();
            }
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if finish {
                while !task.is_finished() {
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            assert_owners();
        }
    }
}
