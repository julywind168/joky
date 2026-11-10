use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};
use std::cell::RefCell;

thread_local! {
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}
extern "C" fn record(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
fn configured(mode: VmMode, source: &str, invariants: bool) -> VmEngine {
    let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
    if invariants {
        registry
            .types()
            .standard
            .set(crate::core::imports::list_test_library())
            .unwrap();
    }
    let mut vm = VmEngine::with_mode_and_registry(mode, registry);
    vm.set_prelude_enabled(false);
    // SAFETY: The callback uses the declared synchronous Int32 -> Unit ABI.
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
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn assert_live_zero() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}

pub(crate) fn model_source(seed: u32, steps: i32) -> String {
    format!(
        "{} fn probe() -> Int32 {{ model({seed}, {steps}) }}",
        include_str!("../../../../tests/fixtures/list_model.jk")
    )
}
pub(crate) fn model_events(seed: u32, steps: i32) -> Vec<i32> {
    let mut list = Vec::new();
    let mut rng = seed;
    let mut events = Vec::new();
    for step in 0..steps {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        let mut op = (rng >> 16) & 15;
        let phase = step & 127;
        if phase < 16 {
            op = (phase & 1) as u32;
        }
        if (16..28).contains(&phase) {
            op = 3;
        }
        if phase == 127 {
            op = 8;
        }
        let index = ((rng >> 8) & 31) as usize;
        let value = step + 1;
        let result = match op {
            0 => {
                list.insert(0, value);
                0
            }
            1 => {
                list.push(value);
                0
            }
            2 => {
                if list.is_empty() {
                    0
                } else {
                    list.remove(0)
                }
            }
            3 => list.pop().unwrap_or(0),
            4 => {
                if index <= list.len() {
                    list.insert(index, value);
                    1
                } else {
                    0
                }
            }
            5 => {
                if index < list.len() {
                    list.remove(index)
                } else {
                    0
                }
            }
            6 => {
                if index < list.len() {
                    std::mem::replace(&mut list[index], value)
                } else {
                    0
                }
            }
            7 => {
                list.reverse();
                0
            }
            8 => {
                list.clear();
                0
            }
            9 => 0,
            10 => list.get(index).copied().unwrap_or(0),
            11 => list.first().copied().unwrap_or(0),
            12 => list.last().copied().unwrap_or(0),
            _ => {
                list.push(value);
                1
            }
        };
        events.extend([i32::MIN, result, list.len() as i32]);
        events.extend(&list);
    }
    events
}
#[test]
fn list_model_checks_order_and_all_private_indices() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for seed in [123456789, 0xfedcba98] {
            let vm = configured(mode, &model_source(seed, 256), true);
            reset();
            assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
            EVENTS.with(|events| {
                assert_eq!(
                    *events.borrow(),
                    model_events(seed, 256),
                    "{mode:?} seed={seed}"
                )
            });
            assert_live_zero();
        }
    }
}

pub(crate) const CASES: &[(&str, Option<&str>)] = &[
    ("boundaries", None),
    ("capacity_reuse", None),
    ("copy_views", None),
    ("strings", None),
    ("transitions", None),
    ("bulk", None),
    ("nested", None),
    ("references", None),
    ("shared_aggregate", None),
    ("unit_and_enum", None),
    ("failed_after_move", Some("List moved failure")),
    ("missing", Some("List index out of bounds")),
    ("overflow", Some("List capacity overflow")),
    (
        "layout_limit",
        Some("Buffer capacity exceeds the target address range"),
    ),
    ("missing_copy", Some("List index out of bounds")),
    ("missing_mut", Some("List index out of bounds")),
    ("missing_owner_mut", Some("List index out of bounds")),
];
pub(crate) fn fixture_source(entry: &str) -> String {
    format!(
        "{} fn probe() -> Int32 {{ {entry}::<Unit>() }} fn task() @suspends -> Int32 {{ probe() }}",
        include_str!("../../../../tests/fixtures/list.jk")
    )
}
/// Share generic specializations across independent calls in one compiled module.
pub(crate) fn fixture_entries(entries: &[&str]) -> String {
    let entries: Vec<_> = entries
        .iter()
        .map(|&entry| (format!("probe_{entry}"), entry))
        .collect();
    super::support::generic_fixture_entries(
        include_str!("../../../../tests/fixtures/list.jk"),
        &entries,
    )
}
fn assert_owners() {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=7 {
            assert!(
                events.iter().filter(|&&n| n == -id).count() <= 1,
                "duplicate construction: {events:?}"
            );
        }
    });
    assert_live_zero();
}
#[test]
fn list_executes_copy_owned_borrowed_and_bulk_apis() {
    let entries: Vec<_> = CASES
        .iter()
        .map(|case| case.0)
        .filter(|&entry| entry != "capacity_reuse")
        .collect();
    let source = fixture_entries(&entries);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source, false);
        for &(entry, failure) in CASES {
            // This case alone uses private storage probes; keep that registry isolated.
            let invariant_vm;
            let (vm, name) = if entry == "capacity_reuse" {
                invariant_vm = configured(mode, &fixture_source(entry), true);
                (&invariant_vm, "probe".to_owned())
            } else {
                (&vm, format!("probe_{entry}"))
            };
            reset();
            match failure {
                None => assert_eq!(vm.call_int32_0(&name).unwrap(), 42, "{mode:?} {entry}"),
                Some(message) => {
                    let error = vm.call_int32_0(&name).unwrap_err();
                    assert_eq!(error.message(), message, "{mode:?} {entry}");
                    assert_eq!(
                        error.code(),
                        if entry == "layout_limit" {
                            crate::ErrorCode::ResourceLimit
                        } else {
                            crate::ErrorCode::RuntimeError
                        }
                    );
                    EVENTS.with(|events| assert!(!events.borrow().contains(&99)));
                    if entry == "missing" {
                        assert!(
                            error
                                .span()
                                .into_iter()
                                .chain(error.runtime_trace().iter().map(|f| f.span))
                                .chain(error.native_call_sites().iter().copied())
                                .any(|span| error
                                    .sources()
                                    .get(span.source_id())
                                    .and_then(|f| span.slice(f.text()))
                                    == Some("list.borrow_ref(18446744073709551615)")),
                            "missing user call location: {error}"
                        );
                    }
                }
            }
            assert_owners();
            if entry == "transitions" || entry == "bulk" {
                EVENTS.with(|events| {
                    let events = events.borrow();
                    for id in 1..=7 {
                        assert_eq!(events.iter().filter(|&&n| n == -id).count(), 1);
                    }
                });
            }
            if entry == "transitions" {
                EVENTS.with(|events| {
                    let events = events.borrow();
                    let a = events.iter().position(|&n| n == 100).unwrap();
                    let b = events.iter().position(|&n| n == 101).unwrap();
                    assert_eq!(b, a + 1, "reserve/reverse dropped owner: {events:?}");
                });
            }
        }
    }
}

#[test]
fn list_allocation_failures_clean_partial_construction_and_transfers() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
        }
    }
    let _restore = Restore;
    let source = fixture_entries(&["transitions", "bulk"]);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source, false);
        let mut sites = std::collections::BTreeSet::new();
        let mut failed_bulk_transfer = false;
        for entry in ["transitions", "bulk"] {
            let mut completed = false;
            let mut partial = false;
            for budget in (0..65536).step_by(16) {
                reset();
                crate::runtime::arena::LIMIT.with(|n| n.set(budget));
                let result = vm.call_int32_0(&format!("probe_{entry}"));
                crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
                match result {
                    Ok(result) => {
                        assert_eq!(result, 42);
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(
                            error.code(),
                            crate::ErrorCode::ResourceLimit,
                            "{mode:?} {entry}: {error}"
                        );
                        partial |= EVENTS
                            .with(|events| events.borrow().iter().any(|&n| (-7..0).contains(&n)));
                        failed_bulk_transfer |=
                            entry == "bulk" && EVENTS.with(|events| events.borrow().contains(&-6));
                        for span in error
                            .span()
                            .into_iter()
                            .chain(error.runtime_trace().iter().map(|f| f.span))
                            .chain(error.native_call_sites().iter().copied())
                        {
                            if let Some(text) = error
                                .sources()
                                .get(span.source_id())
                                .and_then(|f| span.slice(f.text()))
                            {
                                for array in ["values", "owners", "slots"] {
                                    if text == format!("list.{array}.reserve(additional)")
                                        || text == format!("list.{array}.reserve(required - slots)")
                                    {
                                        sites.insert(array);
                                    }
                                }
                            }
                        }
                    }
                }
                assert_owners();
                if completed {
                    break;
                }
            }
            assert!(completed && partial, "{mode:?} {entry}");
        }
        assert_eq!(sites.len(), 3, "{mode:?} allocation sites: {sites:?}");
        assert!(
            failed_bulk_transfer,
            "{mode:?}: no partial bulk transfer failure"
        );
    }
}
fn cancellation(entry: &str, sweep: super::support::CancellationSweep) {
    let mut vm = configured(VmMode::Interpreter, &fixture_source(entry), false);
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
fn list_cancellation_covers_owner_and_bulk_transfer_boundaries() {
    for entry in ["transitions", "bulk"] {
        cancellation(entry, super::support::CancellationSweep::ResourceBoundaries);
    }
}
#[test]
#[ignore = "exhaustive: cancel List at every interpreter instruction"]
fn exhaustive_list_cancels_at_each_instruction() {
    for entry in ["transitions", "bulk"] {
        cancellation(entry, super::support::CancellationSweep::EveryInstruction);
    }
}
#[test]
fn list_instruction_exhaustion_and_reload_retain_owners_and_old_code() {
    for entry in ["transitions", "bulk"] {
        let mut vm = configured(VmMode::Interpreter, &fixture_source(entry), false);
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
            let mut vm = configured(VmMode::Interpreter, &fixture_source(entry), false);
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

fn view_modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "views",
            include_str!("../../../../tests/fixtures/list_views.jk"),
        )
        .unwrap();
    modules
}
fn rejected(body: &str, modules: &crate::ModuleSources) {
    let source = format!("import joky/list.{{type List}} import joky/array.{{type Array}} import joky/option.{{type Option, Some, None}} import joky/element import joky/string {body}");
    let error = super::support::frontend_error(&source, modules);
    assert!(error.span().is_some(), "{body}: {error}");
}
#[test]
fn list_rejects_copy_conflicts_escapes_private_storage_and_aliases() {
    let modules = crate::ModuleSources::new();
    for body in [
        "fn bad() { var m = List.new::<String>(); for x in &m { m.reverse(); x.byte_length(); } }",
        "fn bad() { let m = List.new::<String>(); let it = m.iter_shared(); m.release(); for _ in it {} }",
        "fn bad() -> list.SharedIter<String> { let m = List.new::<String>(); m.iter_shared() }",
        "import joky/iter fn bad() { var m = List.new::<String>(); let (_, item) = iter.step_shared(m.iter_shared()); m.clear(); item.is_some(); }",
        "import joky/task fn bad(m: &List<String>) @suspends { for value in m { task.yield_now(); value.byte_length(); } }",
        "fn bad(m: &mut List<String>) { for _ in m {} }",
        "fn bad(m: &List<String>) { m.get(0); }",
        "fn bad(m: &List<String>) { m.front(); }",
        "fn bad(m: &List<String>) { m.back(); }",
        "fn bad(m: &List<String>) { m.borrow(0); }",
        "fn bad(m: &mut List<String>) { m.borrow_mut(0); }",
        "fn bad(m: &List<Int32>) { m.borrow_mut_ref(0); }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow_mut(0); let b=m.borrow_mut(1); element.write(a,1); element.write(b,2) }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow_mut(0); m.get(1); element.write(a,1) }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow(0); m.push_back(3); element.read(a); }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow(0); m.pop_back(); element.read(a); }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow(0); m.remove(1); element.read(a); }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow(0); m.replace(1,3); element.read(a); }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow(0); m.reserve(0); element.read(a); }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow(0); m.reverse(); element.read(a); }",
        "fn bad(m: &mut List<Int32>) { let a=m.borrow(0); m.clear(); element.read(a); }",
        "fn bad(m: List<Int32>) { let a=m.borrow(0); m.release(); element.read(a); }",
        "fn bad(m: List<Int32>) { let a=m.borrow(0); let moved=m; element.read(a); }",
        "fn bad(m: &mut List<String>) { let a=m.borrow_ref(0); m.clear(); a.byte_length(); }",
        "fn bad() -> &String { var m=List.new::<String>(); m.push_back(\"a\"); m.borrow_ref(0) }",
        "fn bad() -> &element.Element<Int32> { var m=List.new::<Int32>(); m.push_back(1); m.borrow(0) }",
        "fn bad(m: &List<Int32>) { Some(m.borrow(0)); }",
        "fn bad(m: &List<Int32>) { (m.borrow(0),0); }",
        "fn bad(m: &List<Int32>) { m.values.len(); }",
        "fn bad(m: &List<Int32>) { m.slots.len(); }",
        "fn bad(m: &mut List<&Int32>) { let n=42; m.push_back(&n); }",
        "fn bad(m: &mut List<&Int32>) { let n=42; element.write(m.borrow_mut(0),&n); }",
        "fn bad() { List.new::<&mut Int32>(); }",
        "struct Owned { data: Array<Int32>, value: &Int32 } fn bad() { List.new::<Owned>(); }",
        "fn bad() { var m=List.new::<Int32>(); m.append(&mut m); }",
        "fn bad(m: &mut List<Int32>, other: &mut List<Int32>) { let a=other.borrow(0); m.append(other); element.read(a); }",
        "fn bad(m: List<String>) { let value=m.borrow_ref(0); m.into_array(); value.byte_length(); }",
        "fn bad() { var m=List.new::<&Int32>(); let p=alloc(42); m.push_back((&*p)); let out=m.into_array(); out.release() ; (*(p) = 0) }",
    ] { rejected(body, &modules); }
    for body in [
        "import views fn bad(m: &mut views.Sequence<Int32>) { let v=views.view(m,0); m.reverse(); element.read(v); }",
        "import views fn bad(m: &mut views.Sequence<String>) { let v=views.owned(m,0); views.removed(m,0); v.byte_length(); }",
        "import views fn bad(m: &mut views.Sequence<Int32>) { views.append(m,m) }",
        "import views fn bad(m: &mut views.Sequence<&Int32>) { let n=42; views.put(m,&n); }",
        "import views fn bad() { var m=List.new::<&Int32>(); let p=alloc(42); views.put(&mut m,(&*p)); let out=views.export(m); out.release() ; (*(p) = 0) }",
    ] { rejected(body, &view_modules()); }
}
#[test]
fn list_cross_module_aliases_retain_element_and_content_origins() {
    let modules = view_modules();
    for body in [
        "var m=views.new::<Int32>(); views.put(&mut m,20); views.put(&mut m,22); element.write(views.edit(&mut m,0),20); let a=element.read(views.view(&m,0)); let b=views.copied(&m,1).unwrap_or(0); m.reverse(); a+b",
        "var input=Array.new::<String>(); input.push(\"twenty\"); var m=views.import_array(input); views.put(&mut m,\"two\"); let edit=views.owned_mut(&mut m,0); mem.drop(mem.replace(edit,\"a\")); let n=int32(views.owned(&m,0).byte_length()); let out=views.export(m); n+int32(out.borrow_ref(1).byte_length())+38",
        "let p=(&*(alloc(42))); var m=views.new::<&Int32>(); views.put(&mut m,p); let old=views.removed(&mut m,0); m.release(); match old { Some(value) => (*value), None => 0 }",
    ] {
        let source = format!("import views import joky/array.{{type Array}} import joky/element import joky/option.{{Some, None}} import joky/string import joky/mem fn probe() -> Int32 {{ {body} }}");
        let mut types = crate::TypeRegistry::default(); types.prelude = false;
        let ast = types.resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_module(&ast).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(vm.call_int32_0("probe").unwrap(),42);
            assert_live_zero();
        }
        #[cfg(unix)]
        crate::backends::aot::tests::assert_sync_int32(&ast,"probe",42);
    }
}

#[test]
#[ignore = "exhaustive: long List sequence model"]
fn exhaustive_list_model_checks_repeated_growth_reuse_and_reversal() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for seed in [123456789, 0xfedcba98] {
            let vm = configured(mode, &model_source(seed, 4096), true);
            reset();
            assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
            EVENTS.with(|events| assert_eq!(*events.borrow(), model_events(seed, 4096)));
            assert_live_zero();
        }
    }
}
