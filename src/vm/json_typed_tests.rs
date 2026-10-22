use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::ErrorCode;

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(include_str!("../../tests/fixtures/json_typed.jk"))
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}

fn clean() {
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}

#[test]
fn json_typed_context_and_key_contracts_are_checked_in_all_backends() {
    for source in [
        "import joky/json/typed.{type Context} fn alter(context: &mut Context) { context.nodes = 0 }",
        "import joky/json/typed as typed import joky/map.{type Map} fn main() { let value = Map.new::<Int32, String>(); typed.encode(&value); }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(source).is_err());
        }
        let module = crate::parser::parse(source).unwrap();
        assert!(crate::aot::AotCompiler::new("invalid-typed-json")
            .unwrap()
            .compile_module(&module)
            .is_err());
    }
}

struct RestoreBudget(usize);
impl Drop for RestoreBudget {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn json_typed_partial_mapping_and_error_paths_release_owners() {
    let restore = RestoreBudget(crate::runtime::arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in ["json_typed_probe", "json_typed_failure"] {
            let mut failed = false;
            let mut completed = false;
            for budget in (0..65536).step_by(128) {
                crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
                let result = vm.call_int32_0(entry);
                crate::runtime::arena::LIMIT.with(|limit| limit.set(restore.0));
                match result {
                    Ok(value) => {
                        assert_eq!(value, 42);
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?} {entry}");
                        failed = true;
                    }
                }
                clean();
                if completed {
                    break;
                }
            }
            assert!(failed && completed, "{mode:?} {entry}");
        }
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(include_str!("../../tests/fixtures/json_typed_cancel.jk"))
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    super::support::assert_cancellation(
        &mut vm,
        "json_typed_cancel_work",
        100000,
        sweep,
        || {},
        || crate::runtime::arena::LIVE.with(|live| live.get()),
        clean,
    );
}

#[test]
fn json_typed_cancellation_releases_partial_structs_and_paths() {
    cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel typed JSON mapping at every interpreter instruction"]
fn exhaustive_json_typed_cancellation_releases_partial_structs_and_paths() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn json_typed_pending_values_survive_reload_and_vm_destruction() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = vm(mode);
            let mut task = vm.call_async("json_typed_work", &[]).unwrap();
            assert!(!task.is_finished());
            crate::runtime::arena::LIVE.with(|live| assert!(live.get() > 0));
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if finish {
                while !task.is_finished() {
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            clean();
        }
    }
}
