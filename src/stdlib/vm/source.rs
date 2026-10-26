//! Task-local source results; no thread-local last-error transport across suspension.
use super::*;
use crate::runtime::{
    arena::Arena,
    buffer::Buffer,
    cps::{self, CpsFrame},
    executor::current_executor,
};

// 0 success, 1 compilation, 2 incompatible schema, 3 publication conflict, 4 runtime.
fn classify(error: &Error) -> u64 {
    match (error.phase(), error.code()) {
        (
            ErrorPhase::Parse
            | ErrorPhase::Resolve
            | ErrorPhase::TypeCheck
            | ErrorPhase::Compile
            | ErrorPhase::Abi,
            _,
        ) => 1,
        (_, ErrorCode::TypeMismatch) => 2,
        (_, ErrorCode::InvalidState) => 3,
        _ => 4,
    }
}

fn apply(owner: u64, name: &str, source: &str, mode: u64) -> Result<u64, Error> {
    let handle = unsafe { managed::handle(owner)? };
    let state = lookup(handle)?;
    if mode >= 2 {
        let mut engine = state.engine.try_borrow_mut().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        match mode {
            2 => {
                let (input, output) = source.split_once('\n').ok_or_else(|| {
                    runtime_error(ErrorCode::InvalidCall, "Invalid Script contract packet")
                })?;
                engine.require_script_entry(name, input, output)?;
            }
            3 => engine.add_source_module(name, source)?,
            _ => {
                return Err(runtime_error(
                    ErrorCode::InvalidCall,
                    "Unknown Script source operation",
                ))
            }
        }
        return Ok(0);
    }
    let executor = if mode == 1 {
        let executor = current_executor().ok_or_else(|| {
            runtime_error(
                ErrorCode::InvalidState,
                "Script publication requires an active executor",
            )
        })?;
        state.bind(&executor)?;
        Some(executor)
    } else {
        None
    };
    let mut engine = state
        .engine
        .try_borrow_mut()
        .map_err(|_| runtime_error(ErrorCode::InvalidState, "VM is executing another operation"))?;
    state.modules.borrow_mut().try_reserve(1).map_err(|_| {
        runtime_error(
            ErrorCode::ResourceLimit,
            "Script module history allocation failed",
        )
    })?;
    let count = if let Some(executor) = executor {
        state.remember(&engine);
        let owners: Vec<_> = state
            .modules
            .borrow()
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .collect();
        engine.publish_script_source(&executor, &owners, name, source)? as u64
    } else {
        engine.load_named_source(name, source)?;
        0
    };
    state.remember(&engine);
    Ok(count)
}

/// # Safety
/// The five inputs obey core::vm::source_signature and remain alive for this call.
/// The packet has five UInt64 elements; the returned String is owned by arena.
pub(crate) unsafe fn source_poll(arena: &mut Arena, frame: &mut CpsFrame) -> Result<u64, Error> {
    let name = std::str::from_utf8((&*(frame.slots[1] as *const Buffer)).bytes()?)
        .map_err(|_| runtime_error(ErrorCode::InvalidUtf8, "Invalid source filename"))?;
    let source = std::str::from_utf8((&*(frame.slots[2] as *const Buffer)).bytes()?)
        .map_err(|_| runtime_error(ErrorCode::InvalidUtf8, "Invalid source text"))?;
    let packet = &*(frame.slots[4] as *const Buffer);
    if packet.len() != 5 {
        return Err(runtime_error(
            ErrorCode::AbiError,
            "Invalid source result packet",
        ));
    }
    let mut lanes = [std::ptr::null_mut::<u64>(); 5];
    for (index, lane) in lanes.iter_mut().enumerate() {
        *lane = packet.load(index as u64)?.cast();
    }
    // Reserve the success result before any publication. Nothing fallible follows a commit.
    let empty = Buffer::copy_bytes(arena, b"")?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        apply(frame.slots[0], name, source, frame.slots[3])
    }))
    .unwrap_or_else(|_| {
        Err(runtime_error(
            ErrorCode::Panic,
            "Script source operation panicked",
        ))
    });
    let (values, message) = match result {
        Ok(count) => ([0, 0, 0, 0, count], empty),
        Err(error) => {
            let span = error.span();
            let values = [
                classify(&error),
                span.map_or(0, |s| s.start() as u64),
                span.map_or(0, |s| s.end() as u64),
                u64::from(span.is_some()),
                0,
            ];
            let message = Buffer::copy_bytes(arena, error.to_string().as_bytes())?;
            Buffer::release(arena, empty.as_ptr())?;
            (values, message)
        }
    };
    for (lane, value) in lanes.into_iter().zip(values) {
        lane.write(value);
    }
    frame.result = message.as_ptr() as usize as u64;
    frame.state = cps::DONE;
    Ok(frame.result)
}

/// # Safety
/// Five frame slots contain checked arguments; the owned String uses frame.result.
#[export_name = "__joky_vm_source"]
pub(crate) unsafe extern "C" fn source_poll_native(frame: *mut CpsFrame) -> i32 {
    if crate::runtime::parallel::pool::active() {
        return match crate::runtime::parallel::control::poll(
            &mut *cps::arena(frame),
            &mut *frame,
            crate::runtime::parallel::control::Operation::VmSource,
        ) {
            Ok(Some(_)) => 0,
            Ok(None) => 1,
            Err(error) => {
                crate::runtime::error::record_error(error);
                cps::RUNTIME_ERROR
            }
        };
    }
    match source_poll(&mut *cps::arena(frame), &mut *frame) {
        Ok(_) => 0,
        Err(error) => {
            crate::runtime::error::record_error(error);
            cps::RUNTIME_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    use crate::vm::InterpreterLimits;
    fn cancellation(exhaustive: bool) {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        vm.load_source(
            r#"import joky/vm
fn run() @suspends ->Int32 {
    let engine=vm.Vm.interpreter()
    engine.load_source("fn answer()->Int32{{7}}").unwrap()
    engine.publish_source("fn answer()->Int32{{8}}").unwrap()
    match engine.publish_source("fn broken(") {Err(_)=>42,Ok(_)=>0}
}"#,
        )
        .unwrap();
        let execute = |cut: usize| {
            let before = crate::runtime::arena::LIVE.with(|n| n.get());
            let handles = VMS.with(|v| v.borrow().len());
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("run", &[]).unwrap())
                .unwrap();
            let mut steps = 0;
            for _ in 0..cut {
                let report = executor.poll(1).unwrap();
                steps += 1;
                if !report.completed.is_empty() {
                    assert_eq!(*report.completed[0].result.as_ref().unwrap(), 42);
                    break;
                }
            }
            executor.cancel_group(&group);
            drop(executor);
            assert_eq!(VMS.with(|v| v.borrow().len()), handles, "cut {cut}");
            assert_eq!(
                crate::runtime::arena::LIVE.with(|n| n.get()),
                before,
                "cut {cut}"
            );
            steps
        };
        let steps = execute(10000);
        assert!(steps < 10000);
        for cut in (0..=steps).step_by(if exhaustive { 1 } else { (steps / 16).max(1) }) {
            execute(cut);
        }
    }
    #[test]
    fn vm_source_cancellation_releases_messages_and_handles() {
        cancellation(false);
    }
    #[test]
    #[ignore = "exhaustive: cancel source loading and publication at each interpreter instruction"]
    fn exhaustive_vm_source_cancellation() {
        cancellation(true);
    }
}
