//! Persistent reference-free records with checked, nonescaping synchronous receivers.
use super::{engine::LoadedModule, InstanceValue, InterpreterLimits, VmEngine};
use crate::{
    backends::interp,
    core::{instance, ir::FunctionInfo},
    runtime::{
        arena::{Arena, Budget},
        buffer::{Buffer, ElementCallback, ElementCleanup, ElementHook},
    },
    Error, ErrorCode, ErrorPhase, Type,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ptr::NonNull,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

/// Identity scoped to one VM. Closed IDs are never recycled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId {
    vm: u64,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceStatus {
    Ready,
    Busy,
    Failed,
}

/// Retain this report to inspect destructor failure after the VM is dropped.
#[derive(Clone, Default)]
pub struct InstanceCleanupReport(Rc<RefCell<Option<Error>>>);
impl InstanceCleanupReport {
    pub fn error(&self) -> Option<Error> {
        self.0.borrow().clone()
    }
}

/// Persistent roots have a budget separate from each invocation's task arena.
#[derive(Clone, Copy, Debug)]
pub struct InstanceLimits {
    pub max_instances: usize,
    /// Charged root bytes plus per-instance bookkeeping, not process RSS.
    pub max_bytes: usize,
    /// Live invocation allocation allowance, in addition to imported owners.
    pub max_call_bytes: usize,
    /// Combined charged persistent and invocation arena storage for this VM.
    pub max_peak_bytes: usize,
}
impl Default for InstanceLimits {
    fn default() -> Self {
        Self {
            max_instances: 4096,
            max_bytes: 64 * 1024 * 1024,
            max_call_bytes: 64 * 1024 * 1024,
            max_peak_bytes: 128 * 1024 * 1024,
        }
    }
}

pub(super) struct Instances {
    identity: u64,
    shutting_down: Cell<bool>,
    active_calls: Cell<usize>,
    polling_tasks: Cell<bool>,
    next: Cell<u64>,
    entries: RefCell<HashMap<InstanceId, Rc<Instance>>>,
    usage: Rc<Usage>,
    limits: Cell<InstanceLimits>,
    closing: RefCell<Vec<Rc<Instance>>>,
    tasks: RefCell<HashMap<crate::runtime::executor::TaskId, InstanceId>>,
    peak: std::sync::Arc<Budget>,
}
#[derive(Default)]
struct Usage {
    bytes: Cell<usize>,
    roots: Cell<usize>,
    cleanup_error: Rc<RefCell<Option<Error>>>,
}
impl Default for Instances {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            identity: NEXT
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect("VM instance identities exhausted"),
            next: Cell::new(1),
            shutting_down: Cell::new(false),
            active_calls: Cell::new(0),
            polling_tasks: Cell::new(false),
            entries: Default::default(),
            usage: Rc::new(Usage::default()),
            limits: Cell::new(InstanceLimits::default()),
            closing: Default::default(),
            tasks: Default::default(),
            peak: std::sync::Arc::new(Budget {
                limit: (InstanceLimits::default().max_peak_bytes).into(),
                ..Default::default()
            }),
        }
    }
}

struct Instance {
    id: InstanceId,
    // State is dropped before the pinned module and its dependencies.
    state: RefCell<State>,
    module: LoadedModule,
    ty: Type,
    closed: Cell<bool>,
    cleanup_queued: Cell<bool>,
    group: crate::runtime::executor::TaskGroup,
}
struct State {
    storage: Option<Storage>,
    failure: Option<Error>,
    cleanup_arena: Option<Arena>,
    close_error: Option<Error>,
}
struct Storage {
    arena: Arena,
    pointer: NonNull<u8>,
    charge: usize,
    overhead: usize,
    usage: Rc<Usage>,
    peak: std::sync::Arc<Budget>,
    cleanup: Option<ElementCleanup>,
    // Explicitly pin code until cleanup has completed, including unpublished roots.
    module: LoadedModule,
}
impl Storage {
    fn destroy_in(&mut self, arena: &mut Arena) -> Result<(), Error> {
        let result = self
            .cleanup
            .take()
            .map_or(Ok(()), |descriptor| unsafe {
                descriptor.entry(self.pointer.as_ptr()).run(arena)
            })
            .map_err(|error| match &self.module {
                LoadedModule::Jit(module) => error.with_sources(&module.sources),
                LoadedModule::Interpreter(_) => error,
            });
        if let Err(error) = &result {
            self.usage
                .cleanup_error
                .borrow_mut()
                .get_or_insert(error.clone());
        }
        result
    }
    fn destroy(&mut self) -> Result<(), Error> {
        let mut arena = std::mem::take(&mut self.arena);
        let result = self.destroy_in(&mut arena);
        self.arena = arena;
        result
    }
    fn owners(&self) -> Result<Vec<*mut Buffer>, Error> {
        let Some(descriptor) = &self.cleanup else {
            return Ok(Vec::new());
        };
        let mut owners = Vec::new();
        owners
            .try_reserve(descriptor.owners.len())
            .map_err(|_| error(ErrorCode::ResourceLimit, "Owner graph allocation failed"))?;
        owners.extend(
            descriptor
                .owners
                .iter()
                .filter(|&&offset| offset != usize::MAX)
                .map(|&offset| unsafe {
                    self.pointer
                        .as_ptr()
                        .add(offset)
                        .cast::<*mut Buffer>()
                        .read()
                }),
        );
        Ok(owners)
    }
}
impl Drop for Storage {
    fn drop(&mut self) {
        let _ = self.destroy();
        self.usage.bytes.set(self.usage.bytes.get() - self.charge);
        self.usage.roots.set(self.usage.roots.get() - 1);
        self.peak.refund(self.overhead);
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, code, message)
}
fn invalid(message: impl Into<String>) -> Error {
    error(ErrorCode::InvalidCall, message)
}

impl Instances {
    fn defer_cleanup(&self, instance: &Rc<Instance>) {
        if !instance.cleanup_queued.replace(true) {
            self.closing.borrow_mut().push(instance.clone());
        }
    }
    fn get(&self, id: InstanceId) -> Result<Rc<Instance>, Error> {
        if id.vm != self.identity {
            return Err(invalid("Instance belongs to another VM"));
        }
        self.entries
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error(ErrorCode::NotFound, "Unknown or closed instance"))
    }

    fn allocate(
        &self,
        ty: &Type,
        module: &LoadedModule,
        descriptor: &ElementCleanup,
    ) -> Result<Storage, Error> {
        let layout = ty.storage_layout().map_err(|e| invalid(e.to_string()))?;
        let charge = layout
            .size
            .max(1)
            .checked_add(layout.align - 1)
            .and_then(|n| n.checked_add(descriptor.metadata_bytes()?))
            .and_then(|n| {
                n.checked_add(
                    std::mem::size_of::<Instance>()
                        + std::mem::size_of::<(NonNull<u8>, std::alloc::Layout)>()
                        + std::mem::size_of::<(InstanceId, Rc<Instance>)>()
                        + 2 * std::mem::size_of::<usize>(),
                )
            })
            .ok_or_else(|| error(ErrorCode::ResourceLimit, "Instance size overflow"))?;
        let bytes = self
            .usage
            .bytes
            .get()
            .checked_add(charge)
            .filter(|&n| n <= self.limits.get().max_bytes)
            .ok_or_else(|| {
                error(
                    ErrorCode::ResourceLimit,
                    "Persistent instance byte budget exceeded",
                )
            })?;
        if self.usage.roots.get() >= self.limits.get().max_instances
            || self.entries.borrow().len() >= self.limits.get().max_instances
        {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Persistent instance count budget exceeded",
            ));
        }
        let mut arena = Arena::with_budget(self.peak.clone(), None);
        let pointer = arena.allocate(layout.size, layout.align)?;
        let overhead = charge - arena.bytes();
        self.peak.charge(overhead)?;
        // Padding and inactive owner lanes start zeroed; no Drop runs before initialization.
        unsafe {
            pointer.as_ptr().write_bytes(0, layout.size.max(1));
        }
        self.usage.bytes.set(bytes);
        self.usage.roots.set(self.usage.roots.get() + 1);
        Ok(Storage {
            arena,
            overhead,
            peak: self.peak.clone(),
            cleanup: None,
            module: module.clone(),
            pointer,
            charge,
            usage: self.usage.clone(),
        })
    }
}

impl LoadedModule {
    fn root_cleanup(&self, constructor: &str, ty: &Type) -> Result<ElementCleanup, Error> {
        let owners = ty
            .storage_scalars()
            .map_err(|e| invalid(e.to_string()))?
            .into_iter()
            .filter(|(_, ty)| ty.is_owner_lane())
            .map(|(offset, _)| offset)
            .collect();
        let hooks = match self {
            Self::Jit(module) => module.instance_hooks[constructor]
                .iter()
                .map(|(hook, callback)| ElementHook {
                    before: hook.before,
                    offset: hook.offset,
                    indirect: hook.indirect,
                    guards: hook.guards.clone(),
                    callback: ElementCallback::Native(*callback),
                })
                .collect(),
            Self::Interpreter(module) => interp::instance_hooks(module, constructor)
                .iter()
                .map(|hook| {
                    let name = hook.name.clone();
                    let module = module.clone();
                    ElementHook {
                        before: hook.before,
                        offset: hook.offset,
                        indirect: hook.indirect,
                        guards: hook.guards.clone(),
                        callback: ElementCallback::Interpreted(Rc::new(move |data, arena| {
                            interp::cleanup_call(&module, &name, data, arena)
                        })),
                    }
                })
                .collect(),
        };
        Ok(ElementCleanup { owners, hooks })
    }
    pub(super) fn instance_info(&self, name: &str) -> Result<FunctionInfo, Error> {
        match self {
            Self::Jit(module) => module.instance_entries.get(name).map(|f| &f.info),
            Self::Interpreter(module) => interp::instance_info(module, name),
        }
        .cloned()
        .ok_or_else(|| error(ErrorCode::NotFound, format!("Function `{name}` not found")))
    }

    pub(super) fn invoke_instance(
        &self,
        name: &str,
        args: &[u64],
        limits: InterpreterLimits,
        arena: &mut Arena,
    ) -> Result<Vec<u64>, Error> {
        match self {
            Self::Interpreter(module) => interp::call_instance(module, name, args, limits, arena),
            Self::Jit(module) => {
                let info = self.instance_info(name)?;
                let entry = module
                    .instance_entries
                    .get(name)
                    .ok_or_else(|| invalid("Function has no checked instance entry"))?;
                let mut output = vec![0; info.signature.ret.scalar_types().len().max(1)];
                // This entry was emitted for the checked signature by the shared
                // native compiler. Buffers and module code outlive the whole call.
                let call: unsafe extern "C" fn(*const u64, *mut u64, *mut Arena) =
                    unsafe { std::mem::transmute(entry.entry) };
                crate::runtime::error::checked(|| unsafe {
                    call(args.as_ptr(), output.as_mut_ptr(), arena)
                })
                .map_err(|e| e.with_sources(&module.sources))?;
                if info.signature.ret.is_aggregate() {
                    output.truncate(info.signature.ret.scalar_types().len());
                }
                Ok(output)
            }
        }
    }
}

fn arguments(types: &[Type], values: &[InstanceValue]) -> Result<Vec<u64>, Error> {
    if types.len() != values.len() || types.iter().zip(values).any(|(t, v)| *t != v.value_type()) {
        return Err(invalid("Instance argument signature mismatch"));
    }
    Ok(values.iter().map(|v| v.bits()).collect())
}

pub(super) fn initialize(pointer: NonNull<u8>, ty: &Type, words: &[u64]) -> Result<(), Error> {
    let locations = ty.storage_scalars().map_err(|e| invalid(e.to_string()))?;
    if locations.len() != words.len() {
        return Err(invalid("Instance result layout mismatch"));
    }
    for ((offset, scalar), &bits) in locations.iter().zip(words) {
        let size = scalar
            .storage_layout()
            .map_err(|e| invalid(e.to_string()))?
            .size;
        // Compiler-authored offsets and widths are within the validated reference-free
        // root allocation. Native-endian writes do not depend on host byte order.
        unsafe {
            let p = pointer.as_ptr().add(*offset);
            match size {
                1 => p.write(bits as u8),
                2 => p.cast::<u16>().write_unaligned(bits as u16),
                4 => p.cast::<u32>().write_unaligned(bits as u32),
                8 => p.cast::<u64>().write_unaligned(bits),
                _ => return Err(invalid("Unsupported persistent scalar layout")),
            }
        }
    }
    Ok(())
}

impl VmEngine {
    #[cfg(test)]
    pub(crate) fn with_instance_test_module(module: crate::backends::jit::CompiledModule) -> Self {
        let mut vm = Self::new();
        vm.current = Some(LoadedModule::Jit(Rc::new(module)));
        vm
    }

    pub fn instance_cleanup_report(&self) -> InstanceCleanupReport {
        InstanceCleanupReport(self.instances.usage.cleanup_error.clone())
    }

    /// First destructor error observed during explicit or deferred cleanup.
    pub fn instance_cleanup_error(&self) -> Option<Error> {
        self.instances.usage.cleanup_error.borrow().clone()
    }
    /// Current live charge shared by instance roots and active invocations.
    pub fn instance_peak_bytes(&self) -> usize {
        self.instances.peak.bytes.get()
    }

    /// Inspect a compiler-checked instance entry in the current module without
    /// constructing or executing it. This exposes types, never a receiver pointer.
    pub fn instance_function_signature(&self, name: &str) -> Option<crate::FunctionSignature> {
        self.current
            .as_ref()?
            .instance_info(name)
            .ok()
            .map(|info| info.signature)
    }

    /// Set limits for subsequent roots; existing live roots must still fit.
    pub fn set_instance_limits(&mut self, limits: InstanceLimits) -> Result<(), Error> {
        if limits.max_instances == 0
            || limits.max_bytes == 0
            || limits.max_call_bytes == 0
            || limits.max_peak_bytes == 0
            || self.instances.peak.bytes.get() > limits.max_peak_bytes
            || self.instances.usage.bytes.get() > limits.max_bytes
            || self
                .instances
                .entries
                .borrow()
                .len()
                .max(self.instances.usage.roots.get())
                > limits.max_instances
        {
            return Err(invalid("Instance limits cannot exclude existing roots"));
        }
        self.instances.peak.limit.set(limits.max_peak_bytes);
        self.instances.limits.set(limits);
        Ok(())
    }

    /// Construct a checked, reference-free record using a synchronous Joky function.
    /// Parameters are typed scalars; reference-free owning container state is retained.
    pub fn create_instance(
        &self,
        constructor: &str,
        args: &[InstanceValue],
    ) -> Result<InstanceId, Error> {
        if self.instances.shutting_down.get() {
            return Err(invalid("VM is shutting down"));
        }
        let module = self
            .current
            .clone()
            .ok_or_else(|| invalid("No module loaded"))?;
        let info = module.instance_info(constructor)?;
        if !instance::constructor(&info) {
            return Err(invalid(
                "Instance constructor must return a supported reference-free record",
            ));
        }
        let words = arguments(&info.signature.params, args)?;
        let sequence = self.instances.next.get();
        let next = sequence
            .checked_add(1)
            .ok_or_else(|| error(ErrorCode::ResourceLimit, "Instance IDs exhausted"))?;
        self.instances.next.set(next);
        let descriptor = module.root_cleanup(constructor, &info.signature.ret)?;
        let mut storage = self
            .instances
            .allocate(&info.signature.ret, &module, &descriptor)?;
        let mut arena = Arena::with_budget(
            self.instances.peak.clone(),
            Some(self.instances.limits.get().max_call_bytes),
        );
        arena.credit_imported();
        let result =
            module.invoke_instance(constructor, &words, self.interpreter_limits(), &mut arena)?;
        initialize(storage.pointer, &info.signature.ret, &result)?;
        storage.cleanup = Some(descriptor);
        if let Err(error) = self.instances.retain_instance(&mut storage, &mut arena) {
            let _ = storage.destroy_in(&mut arena);
            return Err(error);
        }
        let id = InstanceId {
            vm: self.instances.identity,
            sequence,
        };
        let instance = Rc::new(Instance {
            id,
            module,
            ty: info.signature.ret,
            closed: Cell::new(false),
            cleanup_queued: Cell::new(false),
            group: Default::default(),
            state: RefCell::new(State {
                storage: Some(storage),
                failure: None,
                cleanup_arena: None,
                close_error: None,
            }),
        });
        let mut entries = self.instances.entries.borrow_mut();
        // A constructor may synchronously create or fail other instances.
        if entries.len() >= self.instances.limits.get().max_instances {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Persistent instance count budget exceeded",
            ));
        }
        entries.try_reserve(1).map_err(|_| {
            error(
                ErrorCode::ResourceLimit,
                "Instance registry allocation failed",
            )
        })?;
        entries.insert(id, instance);
        Ok(id)
    }

    /// Invoke an exported synchronous function whose first parameter is a
    /// checked @noescape reference to this instance's exact record type.
    /// Invalid arguments leave the instance ready. Execution failure releases
    /// the state and permanently fails this instance; side effects are not undone.
    pub fn call_instance(
        &self,
        id: InstanceId,
        method: &str,
        args: &[InstanceValue],
    ) -> Result<InstanceValue, Error> {
        let access = self.instance_access(id)?;
        let result = access.call(method, args);
        if access.instance.closed.get()
            || access
                .instance
                .state
                .try_borrow()
                .is_ok_and(|s| s.failure.is_some())
        {
            self.executor.cancel_group(&access.instance.group);
            self.finish_instance_cleanup();
        }
        result
    }

    /// Invalidate the ID immediately. Reentrant close defers freeing storage
    /// until the active call exits; no pointer in that call becomes dangling.
    pub fn close_instance(&self, id: InstanceId) -> Result<(), Error> {
        let instance = self.instances.get(id)?;
        let had_storage = instance
            .state
            .try_borrow()
            .is_ok_and(|s| s.storage.is_some());
        instance.closed.set(true);
        self.instances.entries.borrow_mut().remove(&id);
        self.instances.defer_cleanup(&instance);
        self.executor.cancel_group(&instance.group);
        self.finish_instance_cleanup();
        // Cleanup failures also remain available through instance_cleanup_report.
        let state = instance.state.try_borrow();
        match state {
            Ok(s) if had_storage && s.cleanup_arena.is_none() && s.storage.is_none() => {
                if let Some(e) = &s.close_error {
                    return Err(e.clone());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn instance_status(&self, id: InstanceId) -> Result<InstanceStatus, Error> {
        let instance = self.instances.get(id)?;
        let result = match instance.state.try_borrow() {
            Err(_) => InstanceStatus::Busy,
            Ok(state) if state.failure.is_some() => InstanceStatus::Failed,
            Ok(_) => InstanceStatus::Ready,
        };
        Ok(result)
    }
    pub fn instance_error(&self, id: InstanceId) -> Result<Option<Error>, Error> {
        let instance = self.instances.get(id)?;
        let state = instance
            .state
            .try_borrow()
            .map_err(|_| invalid("Instance is busy"))?;
        Ok(state.failure.clone())
    }
    pub fn instance_type(&self, id: InstanceId) -> Result<Type, Error> {
        Ok(self.instances.get(id)?.ty.clone())
    }
    /// Includes failed roots until they are explicitly closed.
    pub fn instance_count(&self) -> usize {
        self.instances.entries.borrow().len()
    }
    pub fn instance_bytes(&self) -> usize {
        self.instances.usage.bytes.get()
    }
}

impl Instances {
    fn retain_instance(&self, storage: &mut Storage, arena: &mut Arena) -> Result<(), Error> {
        let graph = unsafe { Buffer::owning_allocations(arena, storage.owners()?) }?;
        let owners = arena.selected_bytes(&graph.allocations)?;
        let charge = storage
            .overhead
            .checked_add(storage.arena.bytes())
            .and_then(|n| n.checked_add(owners))
            .and_then(|n| n.checked_add(graph.metadata))
            .ok_or_else(|| {
                error(
                    ErrorCode::ResourceLimit,
                    "Persistent instance size overflow",
                )
            })?;
        let bytes = self
            .usage
            .bytes
            .get()
            .checked_sub(storage.charge)
            .and_then(|n| n.checked_add(charge))
            .filter(|&n| n <= self.limits.get().max_bytes)
            .ok_or_else(|| {
                error(
                    ErrorCode::ResourceLimit,
                    "Persistent instance byte budget exceeded",
                )
            })?;
        arena.transfer(&mut storage.arena, &graph.allocations, usize::MAX)?;
        unsafe {
            Buffer::migrate_metadata(&graph.buffers, &storage.arena);
        }
        storage.charge = charge;
        self.usage.bytes.set(bytes);
        Ok(())
    }
}

#[derive(Clone)]
struct InstanceAccess {
    instances: Rc<Instances>,
    instance: Rc<Instance>,
    interpreter_limits: InterpreterLimits,
}
impl InstanceAccess {
    fn call(&self, method: &str, args: &[InstanceValue]) -> Result<InstanceValue, Error> {
        let instance = &self.instance;
        if self.instances.shutting_down.get() {
            return Err(invalid("VM is shutting down"));
        }
        if instance.closed.get() {
            return Err(invalid("Instance is closed"));
        }
        let info = instance.module.instance_info(method)?;
        if instance::receiver(&info) != Some(&instance.ty) {
            return Err(invalid(
                "Instance method requires a matching @noescape record receiver",
            ));
        }
        let mut words = arguments(&info.signature.params[1..], args)?;
        let mut state = instance
            .state
            .try_borrow_mut()
            .map_err(|_| invalid("Instance is busy (reentrant call)"))?;
        if state.failure.is_some() {
            return Err(invalid("Instance has failed"));
        }
        let storage = state
            .storage
            .as_mut()
            .ok_or_else(|| invalid("Instance has failed"))?;
        struct CallGuard<'a>(&'a Cell<usize>);
        impl Drop for CallGuard<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() - 1);
            }
        }
        self.instances
            .active_calls
            .set(self.instances.active_calls.get() + 1);
        let _call = CallGuard(&self.instances.active_calls);
        let mut arena = Arena::with_budget(
            self.instances.peak.clone(),
            Some(self.instances.limits.get().max_call_bytes),
        );
        words.try_reserve(1).map_err(|_| {
            error(
                ErrorCode::ResourceLimit,
                "Instance argument allocation failed",
            )
        })?;
        let graph = unsafe { Buffer::owning_allocations(&storage.arena, storage.owners()?) }?;
        let imported = storage.arena.pointers_except(storage.pointer)?;
        // Entry failures leave every allocation and cleanup responsibility intact.
        storage.arena.transfer(&mut arena, &imported, usize::MAX)?;
        unsafe {
            Buffer::migrate_metadata(&graph.buffers, &arena);
        }
        arena.credit_imported();
        let receiver = if instance.ty.is_owner() && !instance.ty.is_inline_owner() {
            unsafe { storage.pointer.as_ptr().cast::<usize>().read() }
        } else {
            storage.pointer.as_ptr() as usize
        };
        words.insert(0, receiver as u64);
        let result = instance
            .module
            .invoke_instance(method, &words, self.interpreter_limits, &mut arena)
            .and_then(|words| {
                InstanceValue::from_bits(&info.signature.ret, words.first().copied().unwrap_or(0))
                    .ok_or_else(|| invalid("Invalid instance scalar result"))
            })
            .and_then(|value| {
                if instance.closed.get() {
                    return Err(invalid("Instance was closed during its call"));
                }
                self.instances.retain_instance(storage, &mut arena)?;
                Ok(value)
            });
        if let Err(e) = &result {
            state.failure = Some(e.clone());
            state.cleanup_arena = Some(arena);
            self.instances.defer_cleanup(instance);
        }
        result
    }
}
thread_local! { static CURRENT_INSTANCE: RefCell<Option<InstanceAccess>> = const { RefCell::new(None) }; }
struct AccessGuard(Option<InstanceAccess>);
impl Drop for AccessGuard {
    fn drop(&mut self) {
        CURRENT_INSTANCE.with(|c| {
            c.replace(self.0.take());
        });
    }
}
impl crate::runtime::executor::TaskEnvironment for InstanceAccess {
    fn resume(&self, task: &mut crate::runtime::suspension::Continuation) -> Result<(), Error> {
        if self.instance.closed.get()
            || self
                .instance
                .state
                .try_borrow()
                .map_err(|_| invalid("Instance is busy"))?
                .failure
                .is_some()
        {
            return Err(invalid("Instance task owner is closed or failed"));
        }
        let old = CURRENT_INSTANCE.with(|c| c.replace(Some(self.clone())));
        let _guard = AccessGuard(old);
        let result = task.resume().map_err(Error::from);
        if let Err(error) = &result {
            // A later root in this same batch may call the owner from a host
            // callback. Publish failure now; active poll/stop counts still
            // defer State destruction until all external access has stopped.
            self.instance
                .state
                .borrow_mut()
                .failure
                .get_or_insert(error.clone());
            self.instances.defer_cleanup(&self.instance);
        }
        result
    }
    fn dispose(&self, task: crate::runtime::suspension::Continuation) {
        let old = CURRENT_INSTANCE.with(|c| c.replace(Some(self.clone())));
        let _guard = AccessGuard(old);
        drop(task);
    }
}
/// Owning identity during an instance task poll, task disposal or State cleanup.
/// This does not borrow State and is unavailable outside that dynamic context.
pub fn current_instance_id() -> Option<InstanceId> {
    CURRENT_INSTANCE.with(|c| c.borrow().as_ref().map(|a| a.instance.id))
}

/// Call the owning instance from a synchronous host adapter during an instance
/// task poll. The method must be compiler-checked @noescape; no reference escapes.
/// The owner and module snapshot are implicit, so scripts cannot forge an identity.
pub fn call_current_instance(method: &str, args: &[InstanceValue]) -> Result<InstanceValue, Error> {
    let access = CURRENT_INSTANCE
        .with(|c| c.borrow().clone())
        .ok_or_else(|| invalid("No active instance task"))?;
    access.call(method, args)
}

impl VmEngine {
    fn instance_access(&self, id: InstanceId) -> Result<InstanceAccess, Error> {
        Ok(InstanceAccess {
            instances: self.instances.clone(),
            instance: self.instances.get(id)?,
            interpreter_limits: self.interpreter_limits(),
        })
    }
    /// Configure the owner-thread executor while it has no live roots.
    pub fn configure_tasks(
        &mut self,
        clock: Rc<dyn crate::runtime::executor::Clock>,
        max_tasks: usize,
    ) -> Result<(), Error> {
        if self.executor.task_count() != 0 || !self.domains.borrow().is_empty() || max_tasks == 0 {
            return Err(invalid(
                "Task configuration requires an empty executor and a positive limit",
            ));
        }
        let domain_limits = self.domains.borrow().limits();
        self.executor = Rc::new(crate::runtime::executor::HostExecutor::new(
            clock, max_tasks,
        ));
        self.domains = self.executor.domains();
        self.set_domain_limits(domain_limits)?;
        Ok(())
    }
    /// Register a scalar async export from the owner's pinned module. No code
    /// runs until poll_tasks, and no state receiver is held while waiting.
    pub fn spawn_instance_task(
        &self,
        id: InstanceId,
        name: &str,
        args: &[InstanceValue],
    ) -> Result<crate::runtime::executor::TaskId, Error> {
        self.spawn_instance_task_inner(id, name, args, self.interpreter_limits())
    }
    /// Override the interpreter task's lifetime/poll limits while State access
    /// keeps the VM's synchronous call limits. Existing tasks are unaffected.
    /// JIT has no interpreter budgets and rejects this API.
    pub fn spawn_instance_task_with_limits(
        &self,
        id: InstanceId,
        name: &str,
        args: &[InstanceValue],
        limits: InterpreterLimits,
    ) -> Result<crate::runtime::executor::TaskId, Error> {
        if self.mode() != super::VmMode::Interpreter
            || limits.max_frames == 0
            || limits.poll_budget == Some(0)
        {
            return Err(invalid(
                "Task limits require interpreter mode and positive frame/poll limits",
            ));
        }
        self.spawn_instance_task_inner(id, name, args, limits)
    }
    fn spawn_instance_task_inner(
        &self,
        id: InstanceId,
        name: &str,
        args: &[InstanceValue],
        limits: InterpreterLimits,
    ) -> Result<crate::runtime::executor::TaskId, Error> {
        if self.instances.shutting_down.get() {
            return Err(invalid("VM is shutting down"));
        }
        let access = self.instance_access(id)?;
        if self.instance_status(id)? != InstanceStatus::Ready {
            return Err(invalid("Instance is not ready"));
        }
        let task = access.instance.module.prepare_async(name, args, limits)?;
        let group = access.instance.group.clone();
        let task = self
            .executor
            .spawn_in(&group, task, Some(Rc::new(access)))?;
        self.instances.tasks.borrow_mut().insert(task, id);
        Ok(task)
    }
    pub fn instance_task_count(&self) -> usize {
        self.executor.task_count()
    }
    pub fn cancel_instance_task(&self, id: crate::runtime::executor::TaskId) -> bool {
        let cancelled = self.executor.cancel(id);
        self.finish_instance_cleanup();
        cancelled
    }
    /// Collect a bounded batch of I/O events without waiting, drive only ready
    /// roots and state turns, then collect user terminal outcomes. Instance roots
    /// fail their instance; domain requests have independent failure groups.
    pub fn poll_tasks(
        &self,
        max_polls: usize,
    ) -> Result<crate::runtime::executor::PollReport, Error> {
        if self.instances.active_calls.get() != 0 {
            return Err(invalid("Cannot poll tasks during an instance call"));
        }
        if self.instances.polling_tasks.replace(true) {
            return Err(invalid("Instance tasks are already polling"));
        }
        struct PollGuard<'a>(&'a Cell<bool>);
        impl Drop for PollGuard<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        // Keep every terminal root's owner until the entire report is consumed.
        // A later poll or a stop callback can reenter instance cleanup.
        let polling = PollGuard(&self.instances.polling_tasks);
        let report = self.executor.poll(max_polls)?;
        for completion in &report.completed {
            let id = self.instances.tasks.borrow_mut().remove(&completion.id);
            if let (Some(id), Err(error)) = (id, &completion.result) {
                if let Ok(instance) = self.instances.get(id) {
                    instance
                        .state
                        .borrow_mut()
                        .failure
                        .get_or_insert(error.clone());
                    self.instances.defer_cleanup(&instance);
                    self.executor.cancel_group(&instance.group);
                }
            }
        }
        drop(polling);
        self.finish_instance_cleanup();
        self.finish_domain_cleanup();
        Ok(report)
    }
    /// Wait for an instance root's I/O, external wake or nearest timer without
    /// running scripts. Use poll_tasks afterwards. Frame loops should only poll.
    pub fn wait_tasks(&self) -> Result<(), Error> {
        if self.instances.active_calls.get() != 0 || self.instances.polling_tasks.get() {
            return Err(invalid(
                "Cannot wait for tasks during an instance call or poll",
            ));
        }
        self.executor.wait()
    }
    fn finish_instance_cleanup(&self) {
        let candidates = std::mem::take(&mut *self.instances.closing.borrow_mut());
        for instance in candidates {
            if instance.group.running() != 0 {
                self.instances.closing.borrow_mut().push(instance);
                continue;
            }
            let Ok(state) = instance.state.try_borrow_mut() else {
                self.instances.closing.borrow_mut().push(instance.clone());
                continue;
            };
            // Do not keep a State borrow across stop callbacks.
            drop(state);
            self.executor.cancel_group(&instance.group);
            let access = InstanceAccess {
                instances: self.instances.clone(),
                instance: instance.clone(),
                interpreter_limits: self.interpreter_limits(),
            };
            let old = CURRENT_INSTANCE.with(|c| c.replace(Some(access)));
            let _guard = AccessGuard(old);
            let mut state = instance.state.borrow_mut();
            if let Some(mut storage) = state.storage.take() {
                let result = match state.cleanup_arena.as_mut() {
                    Some(arena) => storage.destroy_in(arena),
                    None => storage.destroy(),
                };
                if let Err(e) = result {
                    state.close_error.get_or_insert(e);
                }
            }
            state.cleanup_arena.take();
            instance.cleanup_queued.set(false);
        }
        if !self.instances.polling_tasks.get() {
            self.instances
                .tasks
                .borrow_mut()
                .retain(|id, _| self.executor.contains(*id));
        }
    }
}
impl Drop for VmEngine {
    fn drop(&mut self) {
        self.shutdown_domains();
        self.instances.shutting_down.set(true);
        let ids: Vec<_> = self.instances.entries.borrow().keys().copied().collect();
        for id in ids {
            let _ = self.close_instance(id);
        }
        self.finish_instance_cleanup();
    }
}
