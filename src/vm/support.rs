//! Shared harness for the VM test files: the effect-recording host bindings,
//! the parity engine builders, and the `@suspends` fixture rewrite.
use super::{VmEngine, VmMode};
pub(super) use crate::parser::test_support::{with_suspends, Suspends};
use crate::runtime::cps::CpsFrame;
use crate::{FunctionSignature, Type};
use std::cell::{Cell, RefCell};

/// Export a fixture's generic cases together so each backend compiles shared code once.
pub(super) fn generic_fixture_entries<N: AsRef<str>>(
    source: &str,
    entries: &[(N, &str)],
) -> String {
    let mut source = source.split("fn main()").next().unwrap().to_owned();
    for (name, target) in entries {
        source.push_str(&format!(
            "\nfn {}() -> Int32 {{ {target}::<Unit>() }}",
            name.as_ref()
        ));
    }
    source
}

#[derive(Clone, Copy)]
pub(super) enum CancellationSweep {
    ResourceBoundaries,
    EveryInstruction,
}

/// Replay deterministic fixtures with synchronous hosts at selected instruction cuts.
/// Resource-boundary sweeps include owner events, adjacent boundaries and periodic cuts;
/// exhaustive sweeps enumerate every cut. Prefixes need one poll, not repeated resumes.
pub(super) fn assert_cancellation<S: PartialEq>(
    vm: &mut VmEngine,
    entry: &str,
    max_instructions: u64,
    sweep: CancellationSweep,
    reset: impl Fn(),
    snapshot: impl Fn() -> S,
    assert_cleaned: impl Fn(),
) {
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    reset();
    let mut previous = snapshot();
    let mut task = vm.call_async(entry, &[]).unwrap();
    let mut cuts = std::collections::BTreeSet::new();
    let mut instructions: u64 = 1;
    loop {
        let current = snapshot();
        if current != previous {
            for cut in instructions.saturating_sub(2)..=instructions + 2 {
                if cut > 0 {
                    cuts.insert(cut);
                }
            }
            previous = current;
        }
        if instructions <= 16 || instructions.is_multiple_of(128) {
            cuts.insert(instructions);
        }
        if task.is_finished() {
            break;
        }
        assert!(
            instructions < max_instructions,
            "{entry} exceeded {max_instructions} instructions"
        );
        task.resume().unwrap();
        instructions += 1;
    }
    assert_eq!(task.cps_result(), Some(42));
    drop(task);
    assert_cleaned();
    cuts.insert(instructions);
    let cuts: Vec<_> = match sweep {
        CancellationSweep::EveryInstruction => (1..=instructions).collect(),
        CancellationSweep::ResourceBoundaries => cuts
            .into_iter()
            .filter(|&cut| cut <= instructions)
            .collect(),
    };
    for budget in cuts {
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        reset();
        let task = vm.call_async(entry, &[]).unwrap();
        if task.is_finished() {
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        assert_cleaned();
    }
}

thread_local! {
    pub(super) static READY: Cell<bool> = const { Cell::new(false) };
    pub(super) static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

pub(super) unsafe extern "C" fn delayed_value(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        EVENTS.with(|events| events.borrow_mut().push(frame.slots[0] as i32));
        frame.host_data = (frame.slots[0] as i32 + 1).to_ne_bytes().to_vec();
        frame.state = 1;
        return 1;
    }
    if !READY.with(Cell::get) {
        return 1;
    }
    frame.result = i32::from_ne_bytes(frame.host_data[..].try_into().unwrap()) as u32 as u64;
    0
}

pub(super) unsafe extern "C" fn fail_now(_frame: *mut CpsFrame) -> i32 {
    7
}

pub(super) extern "C" fn record(value: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}

pub(super) fn parity_vm(source: &str, mode: VmMode) -> VmEngine {
    parity_vm_with_prelude(source, mode, true)
}

pub(super) fn parity_vm_with_prelude(source: &str, mode: VmMode, prelude: bool) -> VmEngine {
    READY.with(|ready| ready.set(false));
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(prelude);
    unsafe {
        vm.register_suspending_fn(
            "delayed_value",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            delayed_value,
        )
        .unwrap();
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            record as *const u8,
        )
        .unwrap();
        vm.register_suspending_fn(
            "fail_now",
            FunctionSignature::new(vec![], Type::Unit),
            fail_now,
        )
        .unwrap();
    }
    vm.load_source(source).unwrap();
    vm
}

/// Call a synchronous fixture through direct or CPS lowering without polling it.
#[track_caller]
pub(super) fn call_int32(vm: &VmEngine, suspends: bool, name: &str, args: &[i32]) -> i32 {
    if suspends {
        let task = vm.call_async(name, args).unwrap();
        assert!(task.is_finished(), "{name}{args:?} unexpectedly suspended");
        task.cps_result().unwrap() as i32
    } else {
        match args {
            [] => vm.call_int32_0(name).unwrap(),
            [a] => vm.call_int32_1(name, *a).unwrap(),
            [a, b] => vm.call_int32_2(name, *a, *b).unwrap(),
            _ => panic!("unsupported fixture arity: {name}{args:?}"),
        }
    }
}

#[track_caller]
pub(super) fn call_unit(vm: &VmEngine, suspends: bool, name: &str) {
    if suspends {
        let task = vm.call_async(name, &[]).unwrap();
        assert!(task.is_finished(), "{name} unexpectedly suspended");
        assert_eq!(task.cps_result(), Some(0), "{name}");
    } else {
        vm.call_unit_0(name).unwrap();
    }
}

pub(super) fn vm(source: &str) -> VmEngine {
    parity_vm(source, VmMode::Jit)
}

/// Static rejection matrices belong to the shared front end. Backend entry-point
/// parity is checked separately below; no code generation is needed for these errors.
#[track_caller]
pub(super) fn frontend_error(source: &str, modules: &crate::ModuleSources) -> crate::Error {
    crate::core::prepared::check_source(source, modules).expect_err(source)
}

#[test]
fn shared_frontend_rejections_reach_all_backend_entry_points() {
    for source in [
        "fn broken( {",
        "fn broken() -> Int32 { unknown }",
        "fn broken() -> Int32 { true }",
        "fn broken() { let n = 1; n = 2; }",
        "struct Loop { next: Loop } fn broken() {}",
        "trait Read { fn value(self) @noescape -> Int32; } fn unused(value: &dyn Read) {}",
        "import joky/list.{type List} import joky/element as element fn broken(xs: &mut List<Int32>) { let view = xs.borrow(0); xs.clear(); element.read(view); }",
        "import joky/set.{type Set} fn broken(xs: Set<Int32>) { xs.union(xs); }",
    ] {
        let expected = frontend_error(source, &crate::ModuleSources::new());
        let check = |error: crate::Error| {
            assert_eq!(error.code(), expected.code(), "{source}: {error}");
            assert_eq!(error.message(), expected.message(), "{source}");
            assert_eq!(error.span(), expected.span(), "{source}");
        };
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source("fn previous() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            check(vm.load_source(source).unwrap_err());
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("previous").unwrap(), 42);
        }
        check(match crate::parser::parse(source) {
            Ok(ast) => crate::aot::AotCompiler::new("frontend-rejection")
                .unwrap().compile_module(&ast).unwrap_err(),
            Err(error) => error.into(),
        });
    }
}
