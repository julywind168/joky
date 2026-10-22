//! Typed Script calls are frame-owned children on the caller's executor.
use super::*;
use crate::runtime::{
    arena::ExternalCharge,
    cps::{self, CpsFrame, DONE, RUNTIME_ERROR},
    executor::{current_executor, current_waker, HostExecutor, TaskWaker},
    suspension::Continuation,
};
use std::{any::Any, cell::Cell, rc::Weak};

mod diagnostics;
pub(crate) use diagnostics::{module_count, print_modules};

pub(super) struct HandleState {
    pub(super) engine: RefCell<VmEngine>,
    session: RefCell<Option<Session>>,
    pub(super) modules: RefCell<Vec<Weak<dyn Any>>>,
    migration: RefCell<Option<Migration>>,
}
enum Migration {
    Single(Box<crate::vm::state_domains::migration::PreparedMigration>),
    Batch(Box<crate::vm::state_domains::migration::batch::Batch>),
}
struct Session {
    domains: Weak<RefCell<crate::vm::state_domains::Domains>>,
    calls: Vec<Weak<Control>>,
}
struct Control {
    closed: Cell<bool>,
    module: Weak<dyn Any>,
    waker: TaskWaker,
}
struct Call {
    task: Continuation,
    layout: Option<(usize, usize)>,
    result_type: crate::Type,
    control: Rc<Control>,
    _charge: ExternalCharge,
}
impl HandleState {
    pub(super) fn new(mode: VmMode) -> Self {
        Self {
            engine: RefCell::new(VmEngine::with_mode(mode)),
            session: RefCell::new(None),
            modules: RefCell::new(Vec::new()),
            migration: RefCell::new(None),
        }
    }
    pub(super) fn remember(&self, engine: &VmEngine) {
        let mut modules = self.modules.borrow_mut();
        modules.retain(|m| m.strong_count() != 0);
        if let Some(owner) = engine.script_code_owner() {
            if !modules.iter().any(|m| m.ptr_eq(&Rc::downgrade(&owner))) {
                modules.push(Rc::downgrade(&owner));
            }
        }
    }
    pub(super) fn bind(&self, executor: &HostExecutor) -> Result<(), Error> {
        let mut context = self.session.borrow_mut();
        if let Some(session) = context.as_ref() {
            if !session
                .domains
                .upgrade()
                .is_some_and(|d| Rc::ptr_eq(&d, &executor.domains()))
            {
                return Err(runtime_error(
                    ErrorCode::InvalidState,
                    "Script handle belongs to another or closed executor",
                ));
            }
        } else {
            *context = Some(Session {
                domains: Rc::downgrade(&executor.domains()),
                calls: Vec::new(),
            });
        }
        Ok(())
    }
    pub(super) fn close(&self) {
        let migration = self.migration.borrow_mut().take();
        drop(migration);
        if let Some(session) = self.session.borrow().as_ref() {
            for call in session.calls.iter().filter_map(Weak::upgrade) {
                // A call may close its own VM while being polled. Invalidate now;
                // its frame disposes the child when control next returns to it.
                call.closed.set(true);
                call.waker.wake();
            }
        }
    }
}

#[export_name = "joky_vm_publish_domain_code"]
pub(crate) extern "C" fn publish_domain_code(handle: u64) -> i32 {
    operation(-1, || {
        let executor = current_executor().ok_or_else(|| {
            runtime_error(
                ErrorCode::InvalidState,
                "Script publication requires an active executor",
            )
        })?;
        let state = lookup(handle)?;
        state.bind(&executor)?;
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        state.remember(&engine);
        let owners: Vec<_> = state
            .modules
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let count = engine.publish_script_code(&executor, &owners)?;
        // Domain limits bound the count well below Int32::MAX.
        Ok(count as i32)
    })
}

struct MigrationContext {
    state: Rc<HandleState>,
    executor: HostExecutor,
    owners: Vec<Rc<dyn Any>>,
}
fn migration_context(handle: u64) -> Result<MigrationContext, Error> {
    let executor = current_executor().ok_or_else(|| {
        runtime_error(
            ErrorCode::InvalidState,
            "Script migration requires an active executor",
        )
    })?;
    let state = lookup(handle)?;
    state.bind(&executor)?;
    {
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        state.remember(&engine);
    }
    let owners = state
        .modules
        .borrow()
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    Ok(MigrationContext {
        state,
        executor,
        owners,
    })
}
#[export_name = "joky_vm_domain_schema"]
pub(crate) extern "C" fn domain_schema(handle: u64) -> u64 {
    operation(0, || {
        let MigrationContext {
            executor, owners, ..
        } = migration_context(handle)?;
        crate::vm::state_domains::migration::schema(&executor.domains(), &owners)
    })
}
#[export_name = "joky_vm_prepare_migration_v1"]
/// # Safety
/// Both text descriptors must be aligned and borrow live immutable UTF-8 for this call.
pub(crate) unsafe extern "C" fn prepare_migration(
    handle: u64,
    schema: u64,
    source: *const u8,
    method: *const u8,
) -> i32 {
    operation(1, || {
        let source = borrow(source)?;
        let method = borrow(method)?;
        let MigrationContext {
            state,
            executor,
            owners,
        } = migration_context(handle)?;
        if state.migration.borrow().is_some() {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "A migration is already prepared",
            ));
        }
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        let plan = crate::vm::state_domains::migration::prepare(
            &engine,
            &executor.domains(),
            &owners,
            schema,
            source,
            method,
        )?;
        *state.migration.borrow_mut() = Some(Migration::Single(Box::new(plan)));
        Ok(0)
    })
}
#[export_name = "joky_vm_begin_migration_v1"]
/// # Safety
/// All text descriptors must borrow live immutable UTF-8 for this call.
pub(crate) unsafe extern "C" fn begin_migration(
    handle: u64,
    schema: u64,
    source: *const u8,
    start: *const u8,
    advance: *const u8,
) -> i32 {
    operation(1, || {
        let source = borrow(source)?;
        let start = borrow(start)?;
        let advance = borrow(advance)?;
        let MigrationContext {
            state,
            executor,
            owners,
        } = migration_context(handle)?;
        if state.migration.borrow().is_some() {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "A migration is already prepared",
            ));
        }
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        let plan = crate::vm::state_domains::migration::begin(
            &engine,
            &executor.domains(),
            &owners,
            schema,
            source,
            start,
            advance,
        )?;
        *state.migration.borrow_mut() = Some(Migration::Single(Box::new(plan)));
        Ok(0)
    })
}
#[export_name = "joky_vm_begin_migration_batch_v1"]
/// # Safety
/// The aligned text descriptor must borrow live immutable UTF-8 during this call.
pub(crate) unsafe extern "C" fn begin_migration_batch(handle: u64, source: *const u8) -> i32 {
    operation(1, || {
        let source = borrow(source)?;
        let MigrationContext {
            state,
            executor,
            owners,
        } = migration_context(handle)?;
        if state.migration.borrow().is_some() {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "A migration is already prepared",
            ));
        }
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        let batch = crate::vm::state_domains::migration::batch::begin(
            &engine,
            &executor.domains(),
            &owners,
            source,
        )?;
        *state.migration.borrow_mut() = Some(Migration::Batch(Box::new(batch)));
        Ok(0)
    })
}
#[export_name = "joky_vm_stage_migration_v1"]
/// # Safety
/// Both aligned text descriptors must borrow live immutable UTF-8 during this call.
pub(crate) unsafe extern "C" fn stage_migration(
    handle: u64,
    start: *const u8,
    advance: *const u8,
) -> i32 {
    operation(1, || {
        let start = borrow(start)?;
        let advance = borrow(advance)?;
        let MigrationContext {
            state,
            executor,
            owners,
        } = migration_context(handle)?;
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        let plan = state
            .migration
            .borrow_mut()
            .take()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No migration prepared"))?;
        let Migration::Batch(mut batch) = plan else {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "Migration is not a batch",
            ));
        };
        crate::vm::state_domains::migration::batch::stage(
            &engine,
            &executor.domains(),
            &owners,
            &mut batch,
            start,
            advance,
        )?;
        *state.migration.borrow_mut() = Some(Migration::Batch(batch));
        Ok(0)
    })
}
#[export_name = "joky_vm_step_migration"]
pub(crate) extern "C" fn step_migration(handle: u64, budget: u64) -> i32 {
    operation(-1, || {
        let MigrationContext {
            state,
            executor,
            owners,
        } = migration_context(handle)?;
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        let mut plan = state
            .migration
            .borrow_mut()
            .take()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No migration prepared"))?;
        let done = match &mut plan {
            Migration::Single(plan) => crate::vm::state_domains::migration::step(
                &engine,
                &executor.domains(),
                &owners,
                plan,
                budget,
            )?,
            Migration::Batch(plan) => crate::vm::state_domains::migration::batch::step(
                &engine,
                &executor.domains(),
                &owners,
                plan,
                budget,
            )?,
        };
        *state.migration.borrow_mut() = Some(plan);
        Ok(if done { 0 } else { 1 })
    })
}
#[export_name = "joky_vm_publish_migration"]
pub(crate) extern "C" fn publish_migration(handle: u64) -> i32 {
    operation(1, || {
        let MigrationContext {
            state,
            executor,
            owners,
        } = migration_context(handle)?;
        let mut engine = state.engine.try_borrow_mut().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        // Reserve bookkeeping before publication so remembering the new module
        // cannot allocate after the state and entry have switched.
        state.modules.borrow_mut().try_reserve(1).map_err(|_| {
            runtime_error(
                ErrorCode::ResourceLimit,
                "Migration module allocation failed",
            )
        })?;
        let plan = state
            .migration
            .borrow_mut()
            .take()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No migration prepared"))?;
        match plan {
            Migration::Single(plan) => crate::vm::state_domains::migration::publish(
                &mut engine,
                &executor.domains(),
                &owners,
                *plan,
            )?,
            Migration::Batch(plan) => crate::vm::state_domains::migration::batch::publish(
                &mut engine,
                &executor.domains(),
                &owners,
                *plan,
            )?,
        }
        state.remember(&engine);
        Ok(0)
    })
}
#[export_name = "joky_vm_cancel_migration"]
pub(crate) extern "C" fn cancel_migration(handle: u64) -> i32 {
    operation(1, || {
        let state = lookup(handle)?;
        let plan = state.migration.borrow_mut().take();
        drop(plan);
        Ok(0)
    })
}

unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Call>()));
    }
    (*frame).host_stop = None;
    (*frame).wake = None;
}

unsafe fn poll(frame: &mut CpsFrame, typed: bool) -> Result<i32, Error> {
    if frame.state == 0 {
        let executor = current_executor().ok_or_else(|| {
            runtime_error(
                ErrorCode::InvalidState,
                "Script call requires an active executor",
            )
        })?;
        let handle = if typed {
            super::managed::handle(frame.slots[0])?
        } else {
            frame.slots[0]
        };
        let state = lookup(handle)?;
        state.bind(&executor)?;
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        let (task, layout, result_type) = if typed {
            let name = crate::runtime::owned_text::borrow(
                frame.slots[1] as *const crate::runtime::buffer::Buffer,
            )?;
            let input = crate::runtime::owned_text::borrow(
                frame.slots[2] as *const crate::runtime::buffer::Buffer,
            )?;
            let output = crate::runtime::owned_text::borrow(
                frame.slots[3] as *const crate::runtime::buffer::Buffer,
            )?;
            let packet = &*(frame.slots[4] as *const crate::runtime::buffer::Buffer);
            let count = usize::try_from(packet.len()).map_err(|_| {
                runtime_error(ErrorCode::ResourceLimit, "Script packet size overflow")
            })?;
            let bytes = count
                .checked_mul(std::mem::size_of::<u64>())
                .ok_or_else(|| {
                    runtime_error(ErrorCode::ResourceLimit, "Script packet size overflow")
                })?;
            let _packet_charge = (&mut *cps::arena(frame)).reserve_external(bytes)?;
            let mut words = Vec::new();
            words.try_reserve_exact(count).map_err(|_| {
                runtime_error(ErrorCode::ResourceLimit, "Script packet allocation failed")
            })?;
            for i in 0..packet.len() {
                words.push(packet.load(i)?.cast::<u64>().read());
            }
            let (task, layout) = engine.prepare_script_call(name, input, output, &words)?;
            (
                task,
                layout,
                engine
                    .script_signature(name)
                    .ok_or_else(|| {
                        runtime_error(ErrorCode::NotFound, "Script result signature missing")
                    })?
                    .ret,
            )
        } else {
            let name = borrow(frame.slots[1] as *const u8)?;
            let schema = crate::core::script::schema(&crate::Type::Int32).unwrap();
            let (task, layout) = engine.prepare_script_call(
                name,
                &schema,
                &schema,
                &[frame.slots[2] as u32 as u64],
            )?;
            (task, layout, crate::Type::Int32)
        };
        state.remember(&engine);
        let control = Rc::new(Control {
            closed: Cell::new(false),
            module: Rc::downgrade(&engine.script_code_owner().unwrap()),
            waker: current_waker().ok_or_else(|| {
                runtime_error(ErrorCode::InvalidState, "Script call requires a task waker")
            })?,
        });
        let charge = (&mut *cps::arena(frame)).reserve_external(std::mem::size_of::<Call>())?;
        let mut session = state.session.borrow_mut();
        let session = session.as_mut().unwrap();
        session.calls.retain(|c| c.strong_count() != 0);
        session.calls.try_reserve(1).map_err(|_| {
            runtime_error(
                ErrorCode::ResourceLimit,
                "Script call registration allocation failed",
            )
        })?;
        session.calls.push(Rc::downgrade(&control));
        frame.host_context = Box::into_raw(Box::new(Call {
            task,
            layout,
            result_type,
            control,
            _charge: charge,
        }))
        .cast();
        frame.host_stop = Some(stop);
        frame.state = 1;
    }
    if frame.state != 1 || frame.host_context.is_null() {
        return Err(runtime_error(
            ErrorCode::InvalidState,
            "Invalid Script call frame",
        ));
    }
    let call = &mut *frame.host_context.cast::<Call>();
    if call.control.closed.get() {
        return Err(runtime_error(
            ErrorCode::InvalidState,
            "Script VM closed during call",
        ));
    }
    call.task.resume().map_err(Error::from)?;
    if call.control.closed.get() {
        return Err(runtime_error(
            ErrorCode::InvalidState,
            "Script VM closed during call",
        ));
    }
    if let Some(values) = call.task.domain_result(call.layout) {
        if crate::runtime::domain_packet::validate(std::slice::from_ref(&call.result_type), &values)
            .is_err()
        {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                "Invalid Script result value",
            ));
        }
        let value = values.first().copied().unwrap_or(0);
        if typed {
            crate::runtime::domain_packet::write(
                &mut *cps::arena(frame),
                frame.slots[4] as *mut crate::runtime::buffer::Buffer,
                &values,
            )?;
        }
        // Dispose child locals and callbacks before publishing its result.
        crate::runtime::error::checked(|| {
            stop(frame, crate::runtime::borrowed_host::STOP_COMPLETED)
        })?;
        frame.result = if typed { 0 } else { value };
        frame.state = DONE;
        Ok(0)
    } else {
        frame.wake = call.task.wait_plan().wake();
        Ok(1)
    }
}

/// # Safety
/// Three live slots contain a same-thread VM handle, a JokyText name descriptor,
/// and Int32 input bits. The frame remains exclusive through completion/stop.
/// The name is read only on the first poll; no borrowed input pointer is retained.
#[export_name = "joky_vm_call_async_int32_v2"]
pub(crate) unsafe extern "C" fn call_async_int32(frame: *mut CpsFrame) -> i32 {
    poll_native(frame, false)
}

unsafe fn poll_native(frame: *mut CpsFrame, typed: bool) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| poll(&mut *frame, typed))).unwrap_or_else(|_| {
        Err(runtime_error(
            ErrorCode::Panic,
            "Script async call panicked",
        ))
    });
    match result {
        Ok(status) => status,
        Err(error) => {
            crate::runtime::error::record_error(error);
            stop(frame, crate::runtime::borrowed_host::STOP_FAILED);
            RUNTIME_ERROR
        }
    }
}

/// # Safety
/// Six live slots match core::vm::call_signature, with an attached arena.
/// The caller owns the VM borrow, three Strings and packet through completion/stop.
#[export_name = "__joky_vm_call"]
pub(crate) unsafe extern "C" fn call_poll_native(frame: *mut CpsFrame) -> i32 {
    match call_poll(&mut *frame) {
        Ok(Some(status)) => {
            (*frame).result = status;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            crate::runtime::error::record_error(error);
            RUNTIME_ERROR
        }
    }
}

/// Private compiler adapter: three owning Strings and a borrowed UInt64 packet
/// remain alive in the caller's frame until this operation completes or stops.
pub(crate) unsafe fn call_poll(frame: &mut CpsFrame) -> Result<Option<u64>, Error> {
    let result = catch_unwind(AssertUnwindSafe(|| poll(frame, true)))
        .unwrap_or_else(|_| Err(runtime_error(ErrorCode::Panic, "Script call panicked")));
    match result {
        Ok(0) => Ok(Some(0)),
        Ok(_) => Ok(None),
        Err(error) => {
            // Cleanup may report an error too. Consume it inside this boundary so
            // a handled child failure cannot poison the next caller operation.
            let _ = crate::runtime::error::checked(|| {
                stop(frame, crate::runtime::borrowed_host::STOP_FAILED)
            });
            if frame.slots.get(5).copied().unwrap_or(0) == 0 {
                return Err(error);
            }
            let status = match error.code() {
                ErrorCode::InvalidState => 1,
                ErrorCode::NotFound | ErrorCode::InvalidCall | ErrorCode::TypeMismatch => 2,
                _ => 3,
            };
            frame.result = status;
            frame.state = DONE;
            Ok(Some(status))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{executor::TaskGroup, string::JokyText};

    #[test]
    fn c_scalar_adapter_preserves_signed_results_and_checks_contracts() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let handle = create_with_mode(mode);
            let source = JokyText::new(
                r#"
                import joky/task
                fn run(n: Int32) @suspends -> Int32 { task.yield_now(); n + 1 }
                fn wrong(n: UInt32) @suspends -> Int32 { 1 / 0 }
                fn result(n: Int32) @suspends -> Bool { let bad = 1 / 0; true }
                fn sync(n: Int32) -> Int32 { 1 / 0 }
            "#,
            );
            assert_eq!(unsafe { load_source(handle, source.as_ptr()) }, 0);
            let executor = HostExecutor::default();
            for (entry, input, expected) in [
                ("run", 41i32, Some(42i32)),
                ("run", -43, Some(-42)),
                ("wrong", 0, None),
                ("result", 0, None),
                ("sync", 0, None),
            ] {
                let group = TaskGroup::new();
                let name = Rc::new(JokyText::new(entry));
                let mut frame = CpsFrame::new(3);
                frame.slots.copy_from_slice(&[
                    handle,
                    name.as_ptr() as usize as u64,
                    input as u32 as u64,
                ]);
                let task = unsafe { Continuation::from_native(call_async_int32, frame, name) };
                executor.spawn(&group, task).unwrap();
                let mut result = None;
                for _ in 0..50 {
                    let report = executor.poll(1).unwrap();
                    if let Some(done) = report.completed.into_iter().next() {
                        result = Some(done.result);
                        break;
                    }
                }
                let result = result.expect("C scalar adapter must complete");
                match expected {
                    Some(value) => assert_eq!(result.unwrap() as i32, value),
                    None => assert_eq!(result.unwrap_err().code(), ErrorCode::InvalidCall),
                }
                assert_eq!(executor.task_count(), 0);
            }
            assert_eq!(close(handle), 0);
            drop(executor);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }

    #[test]
    fn script_scalar_bridge_rejects_cross_executor_reuse() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let handle = create_with_mode(mode);
            unsafe {
                assert_eq!(
                    load_source(
                        handle,
                        JokyText::new("fn run(n:Int32) @suspends ->Int32 { n+1 }").as_ptr()
                    ),
                    0
                );
            }
            for attempt in 0..2 {
                let executor = HostExecutor::default();
                let group = TaskGroup::new();
                let name = Rc::new(JokyText::new("run"));
                let mut frame = CpsFrame::new(3);
                frame
                    .slots
                    .copy_from_slice(&[handle, name.as_ptr() as usize as u64, 41]);
                // The descriptor owner is retained through cancellation; bytes are static.
                let task = unsafe { Continuation::from_native(call_async_int32, frame, name) };
                executor.spawn(&group, task).unwrap();
                let report = executor.poll(1).unwrap();
                assert_eq!(report.completed.len(), 1);
                if attempt == 0 {
                    assert_eq!(*report.completed[0].result.as_ref().unwrap(), 42);
                } else {
                    assert_eq!(
                        report.completed[0].result.as_ref().unwrap_err().code(),
                        ErrorCode::InvalidState
                    );
                }
            }
            assert_eq!(close(handle), 0);
        }
    }
}
