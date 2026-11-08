use super::{support, VmEngine, VmMode};
use crate::{
    runtime::{arena::LIVE, driver::block_on},
    FunctionSignature, Type,
};

const SOURCE: &str = include_str!("../../../../tests/fixtures/borrowed_owner_enums.jk");

thread_local! { static DROPS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }

extern "C" fn record_drop(id: i32) {
    DROPS.with(|drops| drops.borrow_mut().push(id));
}

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    // SAFETY: the callback uses the registered scalar ABI and lives for the test.
    unsafe {
        vm.register_fn(
            "record_drop",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record_drop as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
    vm
}

fn clean() {
    LIVE.with(|live| assert_eq!(live.get(), 0));
    DROPS.with(|drops| {
        let drops = drops.borrow();
        assert!(drops.iter().all(|id| [1, 2].contains(id)), "{drops:?}");
        for id in [1, 2] {
            assert!(
                drops.iter().filter(|&&value| value == id).count() <= 1,
                "{drops:?}"
            );
        }
    });
}

#[test]
fn borrowed_owner_enums_execute_and_release_only_owned_payloads() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        DROPS.with(|drops| drops.borrow_mut().clear());
        assert_eq!(
            block_on(vm.prepare_async("exercise", &[]).unwrap()).unwrap(),
            42
        );
        DROPS.with(|drops| assert_eq!(*drops.borrow(), [1, 2]));
        clean();
    }
}

fn cancel(sweep: support::CancellationSweep) {
    let mut vm = vm(VmMode::Interpreter);
    support::assert_cancellation(
        &mut vm,
        "exercise",
        10000,
        sweep,
        || DROPS.with(|drops| drops.borrow_mut().clear()),
        || {
            (
                LIVE.with(|live| live.get()),
                DROPS.with(|drops| drops.borrow().len()),
            )
        },
        clean,
    );
}

#[test]
fn borrowed_owner_enums_cancel_at_resource_boundaries() {
    cancel(support::CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel mixed owning and borrowed enum payloads at every instruction"]
fn exhaustive_borrowed_owner_enums_cancel_every_instruction() {
    cancel(support::CancellationSweep::EveryInstruction);
}

#[test]
fn borrowed_owner_enums_reject_escape_mutation_and_storage() {
    let prefix = "fn pick(text: &String) -> Result<&String, String> { Ok(text) }\n";
    for body in [
        "fn bad() -> Result<&String, String> { let text = \"local\"; pick(&text) }",
        "fn bad(text: String) -> Result<&String, String> { pick(&text) }",
        "fn bad() { var text = \"before\"; let result = pick(&text); text = \"after\"; assert(result! == text, \"use\") }",
        "fn bad() -> &String { let text = \"local\"; let result = pick(&text); result! }",
        "fn bad(text: &String) @noescape -> Result<&String, String> { pick(text) }",
        "fn bad(value: &mut Int32) -> Result<&mut Int32, String> { Ok(value) }",
        "fn bad(text: &String) { let values: Array<Result<&String, String>> = [pick(text)] }",
        "struct Boxed { value: Result<&String, String> } fn bad(text: &String) { let values = [Boxed { value: pick(text) }] }",
        "fn bad(text: &String) { let value = pick(text); let again = value; value!; }",
        "fn bad() { var values: Array<Int32> = [42]; let view: Result<&Array<Int32>, String> = Ok(&values); values.push(7); view!.len(); }",
        "fn bad() @suspends { let text = \"local\"; let value = pick(&text); isolate { value!; } }",
        "fn bad(value: &mut Result<&String, String>) {}",
        "fn unpack(value: Result<&String, String>) -> &String { value! } fn bad() { var text = \"before\"; let value = unpack(pick(&text)); text = \"after\"; value.byte_length(); }",
    ] {
        let source = format!("{prefix}{body}");
        crate::parser::parse(&source).unwrap_or_else(|error| panic!("{body}: {error}"));
        assert!(crate::core::prepared::check_source(&source, &Default::default()).is_err(), "accepted: {body}");
    }
}
