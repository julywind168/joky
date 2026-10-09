//! S0 probes for existing ownership mechanisms, not a dynamic String implementation.
use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, ModuleSources, Type};
use std::cell::{Cell, RefCell};

const LIBRARY: &str = include_str!("../../tests/fixtures/string_storage_probe.jk");
const SOURCE: &str = r#"
    import probe
    fn success() -> Int32 { probe.success() }
    fn failure() -> Int32 { probe.failure() }
    fn transfer() -> Int32 { probe.transfer() }
    fn tight_failure() -> Int32 { probe.tight_failure() }
    fn tight_allocation() -> Int32 { probe.tight_allocation() }
    fn task() @suspends -> Int32 { probe.success() }
"#;
thread_local! {
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    static EXHAUST: Cell<bool> = const { Cell::new(false) };
}
extern "C" fn record(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
    if matches!(value, 901 | 902) && EXHAUST.with(Cell::get) {
        arena::LIMIT.with(|limit| limit.set(0));
    }
    if value == 903 && EXHAUST.with(Cell::get) {
        // End the transfer-only fence before the separately allocating view.
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}
fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules.insert("probe", LIBRARY).unwrap();
    modules
}
fn configured(mode: VmMode) -> VmEngine {
    let mut vm = registered(mode);
    vm.load_source_with_modules(SOURCE, &modules())
        .unwrap_or_else(|error| panic!("{mode:?}: {error} at {:?}", error.span()));
    vm
}
fn registered(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    // SAFETY: The callback has the declared synchronous Int32 -> Unit ABI.
    unsafe {
        vm.register_fn(
            "record_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn cleaned() {
    arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=3 {
            let created = events.iter().filter(|&&event| event == -id).count();
            let dropped = events.iter().filter(|&&event| event == id).count();
            assert_eq!(created, dropped, "resource {id}: {events:?}");
            assert!(created <= 1, "duplicate resource {id}: {events:?}");
        }
    });
}
struct RestoreLimit(usize);
impl RestoreLimit {
    fn new() -> Self {
        Self(arena::LIMIT.with(Cell::get))
    }
}
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
        EXHAUST.with(|flag| flag.set(false));
    }
}

#[test]
fn string_storage_probe_moves_returns_borrows_and_replaces() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for entry in ["success", "transfer"] {
            reset();
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?} {entry}");
            cleaned();
        }
        reset();
        let error = vm.call_int32_0("failure").unwrap_err();
        assert_eq!(error.message(), "Storage probe failure");
        cleaned();
    }
}

#[test]
fn string_storage_probe_budget_failures_clean_partial_owners() {
    let _restore = RestoreLimit::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        let mut completed = false;
        let mut failed = false;
        for budget in (0..16384).step_by(32) {
            reset();
            arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("success");
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?}: {error}");
                    failed = true;
                }
            }
            cleaned();
            if completed {
                break;
            }
        }
        assert!(failed && completed, "{mode:?}");
    }
}

#[test]
fn string_storage_probe_first_errors_survive_exhausted_arena_and_drop_failure() {
    let _restore = RestoreLimit::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for (entry, code, message) in [
            (
                "tight_failure",
                ErrorCode::RuntimeError,
                "Primary probe failure",
            ),
            (
                "tight_allocation",
                ErrorCode::ResourceLimit,
                "Task arena allocation",
            ),
        ] {
            reset();
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
            EXHAUST.with(|flag| flag.set(true));
            let error = vm.call_int32_0(entry).unwrap_err();
            assert_eq!(error.code(), code, "{mode:?}: {error}");
            assert!(error.message().contains(message), "{mode:?}: {error}");
            EVENTS.with(|events| assert_eq!(*events.borrow(), [-3, 902, 3]));
            assert!(error.span().is_some());
            cleaned();
        }
        drop(vm);
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}

#[test]
fn string_storage_probe_transfer_obeys_budget_without_leaking() {
    let _restore = RestoreLimit::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        reset();
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
        EXHAUST.with(|flag| flag.set(true));
        let result = vm.call_int32_0("transfer");
        // Native owner moves need no further arena allocations. Interpreter
        // cleanup snapshots may allocate even though no payload is copied.
        match mode {
            VmMode::Jit => assert_eq!(result.unwrap(), 42),
            VmMode::Interpreter => match result {
                Ok(value) => assert_eq!(value, 42),
                Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
            },
        }
        EVENTS.with(|events| {
            let events = events.borrow();
            assert!(*events == [901] || *events == [901, 903], "{events:?}");
        });
        cleaned();
    }
}

#[test]
fn string_storage_probe_pending_sync_helper_survives_reload_and_cancellation() {
    for complete in [false, true] {
        let mut vm = configured(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        reset();
        let mut task = vm.call_async("task", &[]).unwrap();
        let mut reached_owner = false;
        for _ in 0..10000 {
            if EVENTS.with(|events| events.borrow().contains(&-1)) {
                reached_owner = true;
                break;
            }
            assert!(!task.is_finished());
            task.resume().unwrap();
        }
        assert!(reached_owner);
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        if complete {
            for _ in 0..10000 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert!(task.is_finished());
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        cleaned();
    }
}

#[test]
fn string_storage_probe_rejects_moves_escaping_views_and_private_storage() {
    for (body, message) in [
        ("fn bad() { let t = probe.finish(probe.builder()) probe.release(t) probe.sum(&t); }", "moved"),
        ("fn bad() { let b = probe.builder() probe.finish(b); probe.finish(b); }", "moved"),
        ("fn bad() { let t = probe.finish(probe.builder()) let v = probe.view(&t) probe.release(t) slices.len(v); }", "borrow"),
        ("fn bad() -> &Slice<Int32> { let t = probe.finish(probe.builder()) probe.view(&t) }", "borrowed input"),
        ("fn bad(t: &probe.TextProbe) { t.data.len(); }", "private"),
        ("fn bad() { let t = probe.finish(probe.builder()) let copy = t probe.sum(&t); }", "moved"),
    ] {
        let source = format!("import probe import joky/slice as slices.{{type Slice}} {body}");
        rejects(&source, &modules(), message);
    }
}

fn rejects(source: &str, modules: &ModuleSources, message: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = registered(mode)
            .load_source_with_modules(source, modules)
            .unwrap_err();
        assert!(
            error.message().contains(message),
            "{mode:?}: {source}: {error}"
        );
    }
    let result = crate::TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), modules)
        .and_then(|ast| {
            crate::aot::AotCompiler::new("string-probe-negative")?
                .compile_module(&ast)
                .map(|_| ())
        });
    let error = result.unwrap_err();
    assert!(error.message().contains(message), "AOT: {source}: {error}");
}

#[test]
fn string_storage_probe_records_remaining_container_and_suspension_gaps() {
    for (source, message) in [
        (
            "import joky/array.{type Array} fn bad() { Array.new::<&String>(); }",
            "Copy type or owning record",
        ),
        (
            "import probe fn bad(text: &probe.TextProbe) @suspends {}",
            "synchronous",
        ),
        (
            "import probe extern fn host(text: probe.TextProbe)",
            "host ABI",
        ),
    ] {
        rejects(source, &modules(), message);
    }
    assert!(Type::String.is_owner());
    assert!(!Type::String.is_copy_value());
    assert_eq!(
        (
            Type::UInt8.memory_layout().unwrap().size,
            Type::UInt8.memory_layout().unwrap().align
        ),
        (1, 1)
    );
    assert_eq!(
        Type::String.storage_layout().unwrap().size,
        std::mem::size_of::<usize>()
    );
    assert_eq!(Type::String.parameter_slots(), 1);
    for pointer_size in [4, 8] {
        let target = crate::memory_layout::MemoryLayoutTarget {
            int32_align: 4,
            int64_align: pointer_size,
            pointer_size,
            pointer_align: pointer_size,
            max_size: i32::MAX as usize,
        };
        // Existing narrow storage lanes demonstrate why slots cannot define
        // UInt8/Rune layouts. The actual future types remain rejected above.
        let byte = Type::Bool.memory_layout_for(target).unwrap();
        let scalar = Type::UInt32.memory_layout_for(target).unwrap();
        let pointer = Type::Ptr.memory_layout_for(target).unwrap();
        assert_eq!((byte.size, byte.align), (1, 1));
        assert_eq!((scalar.size, scalar.align), (4, 4));
        assert_eq!((pointer.size, pointer.align), (pointer_size, pointer_size));
        assert!(Type::String.memory_layout_for(target).is_err());
    }
}
