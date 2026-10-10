use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::ErrorCode;

fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(include_str!("../../tests/fixtures/json1b_values.jk"))
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}
fn clean() {
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}
struct RestoreBudget;
impl Drop for RestoreBudget {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
}
#[test]
fn json1b_partial_construction_failure_releases_owners() {
    let _restore = RestoreBudget;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let mut failed = false;
        let mut completed = false;
        for budget in (0..65536).step_by(64) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("json_probe");
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit);
                    failed = true;
                }
            }
            clean();
            if completed {
                break;
            }
        }
        assert!(failed && completed);
    }
}
fn cancellation(sweep: CancellationSweep) {
    let mut vm = vm(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "json_work",
        30000,
        sweep,
        || {},
        || crate::runtime::arena::LIVE.with(|live| live.get()),
        clean,
    );
}
#[test]
fn json1b_cancellation_cleans_values_and_number_borrows() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel JSON values at every interpreter instruction"]
fn exhaustive_json1b_cancellation_cleans_values_and_number_borrows() {
    cancellation(CancellationSweep::EveryInstruction);
}
#[test]
fn json1b_pending_values_survive_reload_and_vm_destruction() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = vm(mode);
            let mut task = vm.call_async("json_work", &[]).unwrap();
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

fn decoder_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(include_str!("../../tests/fixtures/json2a_decode.jk"))
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}
#[test]
fn json2a_partial_construction_and_error_paths_release_owners() {
    let _restore = RestoreBudget;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = decoder_vm(mode);
        for entry in ["json2a_probe", "json2a_failure"] {
            let mut failed = false;
            let mut completed = false;
            for budget in (0..65536).step_by(64) {
                crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
                let result = vm.call_int32_0(entry);
                crate::runtime::arena::LIMIT
                    .with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
                match result {
                    Ok(value) => {
                        assert_eq!(value, 42);
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(error.code(), ErrorCode::ResourceLimit);
                        failed = true;
                    }
                }
                clean();
                if completed {
                    break;
                }
            }
            assert!(failed && completed);
        }
    }
}
fn decoder_cancellation(sweep: CancellationSweep) {
    let mut vm = decoder_vm(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "json2a_cancel_work",
        100000,
        sweep,
        || {},
        || crate::runtime::arena::LIVE.with(|live| live.get()),
        clean,
    );
}
#[test]
fn json2a_cancellation_releases_parser_frames_and_completed_trees() {
    decoder_cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel JSON decoding at every interpreter instruction"]
fn exhaustive_json2a_cancellation_releases_parser_frames_and_completed_trees() {
    decoder_cancellation(CancellationSweep::EveryInstruction);
}
#[test]
fn json2a_pending_trees_survive_reload_and_vm_destruction() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = decoder_vm(mode);
            let mut task = vm.call_async("json2a_work", &[]).unwrap();
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

fn encoder_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(include_str!("../../tests/fixtures/json2b_encode.jk"))
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}
#[test]
fn json2b_partial_output_and_limit_failures_release_owners() {
    let _restore = RestoreBudget;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = encoder_vm(mode);
        for entry in ["json2b_probe", "json2b_failure"] {
            let mut failed = false;
            let mut completed = false;
            for budget in (0..65536).step_by(64) {
                crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
                let result = vm.call_int32_0(entry);
                crate::runtime::arena::LIMIT
                    .with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
                match result {
                    Ok(value) => {
                        assert_eq!(value, 42);
                        completed = true;
                    }
                    Err(error) => {
                        assert_eq!(error.code(), ErrorCode::ResourceLimit);
                        failed = true;
                    }
                }
                clean();
                if completed {
                    break;
                }
            }
            assert!(failed && completed);
        }
    }
}
fn encoder_cancellation(sweep: CancellationSweep) {
    let mut vm = encoder_vm(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "json2b_cancel_work",
        100000,
        sweep,
        || {},
        || crate::runtime::arena::LIVE.with(|live| live.get()),
        clean,
    );
}
#[test]
fn json2b_cancellation_releases_output_and_borrowed_root_frames() {
    encoder_cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel JSON encoding at every interpreter instruction"]
fn exhaustive_json2b_cancellation_releases_output_and_borrowed_root_frames() {
    encoder_cancellation(CancellationSweep::EveryInstruction);
}
#[test]
fn json2b_pending_trees_and_output_survive_reload_and_vm_destruction() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = encoder_vm(mode);
            let mut task = vm.call_async("json2b_work", &[]).unwrap();
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
