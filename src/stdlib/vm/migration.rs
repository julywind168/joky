//! Exclusive migration ownership and request-local diagnostics for all schedulers.
use super::*;
use crate::{
    core::vm::MigrationOperation,
    runtime::{
        arena::Arena,
        buffer::Buffer,
        cps::{self, CpsFrame},
        resource::OwnedResource,
    },
};
use std::{ffi::c_void, ptr::NonNull};

struct Session {
    state: Rc<async_call::HandleState>,
    owner: managed::Owner,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.state.migration.borrow_mut().take();
        self.state.migration_claimed.set(false);
    }
}
struct Owner {
    session: Option<Session>,
    error: Option<(u64, Error)>,
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Owner>()));
}
unsafe fn open(arena: &mut Arena, bits: u64) -> Result<u64, Error> {
    let result = (|| {
        let owner = managed::retain(bits).map_err(|e| (6, e))?;
        let state = lookup(owner.id().map_err(|e| (6, e))?).map_err(|e| (6, e))?;
        if state.migration_claimed.get() || state.migration.borrow().is_some() {
            return Err((
                5,
                runtime_error(
                    ErrorCode::InvalidState,
                    "VM already has a migration session or candidate",
                ),
            ));
        }
        state.migration_claimed.set(true);
        Ok(Session { state, owner })
    })();
    let owner = match result {
        Ok(session) => Owner {
            session: Some(session),
            error: None,
        },
        Err(error) => Owner {
            session: None,
            error: Some(error),
        },
    };
    let token = NonNull::new(Box::into_raw(Box::new(owner)).cast()).unwrap();
    let resource =
        OwnedResource::from_token(token, release).inspect_err(|_| release(token.as_ptr()))?;
    Ok(Buffer::external(arena, resource)?.as_ptr() as u64)
}
unsafe fn owner<'a>(bits: u64) -> Result<&'a mut Owner, Error> {
    let buffer = (bits as *const Buffer)
        .as_ref()
        .ok_or_else(|| runtime_error(ErrorCode::AbiError, "Null migration session"))?;
    let token = buffer.resource_ref()?.control_token(release)?;
    Ok(&mut *token.cast::<Owner>())
}
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
        (_, ErrorCode::InvalidState | ErrorCode::InvalidCall) => 3,
        _ => 4,
    }
}
unsafe fn command(frame: &mut CpsFrame) -> Result<u64, Error> {
    let packet = &*(frame.slots[6] as *const Buffer);
    if packet.len() != 4 {
        return Err(runtime_error(
            ErrorCode::AbiError,
            "Invalid migration result packet",
        ));
    }
    let mut lanes = [std::ptr::null_mut::<u64>(); 4];
    for (i, lane) in lanes.iter_mut().enumerate() {
        *lane = packet.load(i as u64)?.cast();
    }
    let text = |slot: usize| -> Result<&str, Error> {
        std::str::from_utf8((&*(frame.slots[slot] as *const Buffer)).bytes()?)
            .map_err(|_| runtime_error(ErrorCode::InvalidUtf8, "Invalid migration text"))
    };
    let (source, start, advance) = (text(3)?, text(4)?, text(5)?);
    let owner = owner(frame.slots[0])?;
    let mut value = 0;
    if let Some(session) = &owner.session {
        let result = session.owner.id().map_err(|e| (6, e)).and_then(|handle| {
            catch_unwind(AssertUnwindSafe(|| {
                use async_call::*;
                let number = frame.slots[2];
                match frame.slots[1] {
                    0 => Ok(0),
                    1 => domain_schema_result(handle, true),
                    2 => prepare_migration_result(handle, true, number, source, start)
                        .map(|v| v as u64),
                    3 => begin_migration_result(handle, true, number, source, start, advance)
                        .map(|v| v as u64),
                    4 => begin_migration_batch_result(handle, true, source).map(|v| v as u64),
                    5 => stage_migration_result(handle, true, start, advance).map(|v| v as u64),
                    6 => step_migration_result(handle, true, number).map(|v| v as u64),
                    7 => publish_migration_result(handle, true).map(|v| v as u64),
                    8 => cancel_migration_result(handle, true).map(|v| v as u64),
                    _ => Err(runtime_error(
                        ErrorCode::InvalidCall,
                        "Unknown migration command",
                    )),
                }
            }))
            .unwrap_or_else(|_| {
                Err(runtime_error(
                    ErrorCode::Panic,
                    "Migration operation panicked",
                ))
            })
            .map_err(|e| (classify(&e), e))
        });
        owner.error = match result {
            Ok(v) => {
                value = v;
                None
            }
            Err(e) => Some(e),
        };
    }
    let (status, values) = match &owner.error {
        None => (0, [value, 0, 0, 0]),
        Some((status, error)) => {
            let span = error.span();
            (
                *status,
                [
                    0,
                    span.map_or(0, |s| s.start() as u64),
                    span.map_or(0, |s| s.end() as u64),
                    u64::from(span.is_some()),
                ],
            )
        }
    };
    // Packet addresses were validated before executing any command, including publication.
    for (lane, value) in lanes.into_iter().zip(values) {
        lane.write(value);
    }
    Ok(status)
}

/// # Safety
/// Slots use the compiler-owned signature; mutable session and packet borrows are exclusive.
pub(crate) unsafe fn migration_poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    op: MigrationOperation,
) -> Result<Option<u64>, Error> {
    if crate::runtime::parallel::pool::active() {
        return crate::runtime::parallel::control::poll(
            arena,
            frame,
            crate::runtime::parallel::control::Operation::VmMigration(op),
        );
    }
    if matches!(op, MigrationOperation::Command)
        && (1..=7).contains(&frame.slots[1])
        && !crate::vm::state_domains::parallel::gate(frame)
    {
        return Ok(None);
    }
    let value = match op {
        MigrationOperation::Open => open(arena, frame.slots[0])?,
        MigrationOperation::Command => command(frame)?,
        MigrationOperation::Diagnostic => {
            let owner = owner(frame.slots[0])?;
            let message = owner
                .error
                .as_ref()
                .map(|(_, e)| e.to_string())
                .unwrap_or_default();
            Buffer::copy_bytes(arena, message.as_bytes())?.as_ptr() as u64
        }
    };
    frame.result = value;
    frame.state = cps::DONE;
    Ok(Some(value))
}
unsafe fn native(frame: *mut CpsFrame, op: MigrationOperation) -> i32 {
    match migration_poll(&mut *cps::arena(frame), &mut *frame, op) {
        Ok(Some(_)) => 0,
        Ok(None) => 1,
        Err(error) => {
            crate::runtime::error::record_error(error);
            cps::RUNTIME_ERROR
        }
    }
}
#[export_name = "__joky_vm_migration_open"]
pub(crate) unsafe extern "C" fn migration_open_native(frame: *mut CpsFrame) -> i32 {
    native(frame, MigrationOperation::Open)
}
#[export_name = "__joky_vm_migration_command"]
pub(crate) unsafe extern "C" fn migration_command_native(frame: *mut CpsFrame) -> i32 {
    native(frame, MigrationOperation::Command)
}
#[export_name = "__joky_vm_migration_diagnostic"]
pub(crate) unsafe extern "C" fn migration_diagnostic_native(frame: *mut CpsFrame) -> i32 {
    native(frame, MigrationOperation::Diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    fn cancellation(exhaustive: bool) {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        vm.set_interpreter_limits(crate::vm::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        vm.load_source(
            r#"import joky/vm
fn main() @suspends -> Int32 {
    let engine = vm.Vm.interpreter()
    var session = engine.migration()!
    engine.migration().is_err();
    session.schema().is_err();
    session.cancel()!
    let alias = engine.share()
    var closed = engine.migration()!
    alias.close();
    closed.schema().is_err();
    0
}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let execute = |cut: usize| {
            let before = crate::runtime::arena::LIVE.with(|n| n.get());
            let handles = VMS.with(|v| v.borrow().len());
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("main", &[]).unwrap())
                .unwrap();
            let mut steps = 0;
            for _ in 0..cut {
                let report = executor.poll(1).unwrap();
                steps += 1;
                if !report.completed.is_empty() {
                    assert_eq!(
                        *report.completed[0]
                            .result
                            .as_ref()
                            .unwrap_or_else(|e| panic!("cut={cut}: {e}")),
                        0
                    );
                    break;
                }
            }
            executor.cancel_group(&group);
            drop(executor);
            assert_eq!(VMS.with(|v| v.borrow().len()), handles, "cut={cut}");
            assert_eq!(
                crate::runtime::arena::LIVE.with(|n| n.get()),
                before,
                "cut={cut}"
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
    fn migration_sessions_cancellation_releases_open_and_failed_owners() {
        cancellation(false);
    }
    #[test]
    #[ignore = "exhaustive: cancel migration session acquisition and error delivery at every instruction"]
    fn exhaustive_migration_session_cancellation() {
        cancellation(true);
    }
}
