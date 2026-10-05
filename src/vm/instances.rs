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
    next: Cell<u64>,
    entries: RefCell<HashMap<InstanceId, Rc<Instance>>>,
    usage: Rc<Usage>,
    limits: InstanceLimits,
    peak: Rc<Budget>,
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
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect("VM instance identities exhausted"),
            next: Cell::new(1),
            entries: Default::default(),
            usage: Rc::new(Usage::default()),
            limits: Default::default(),
            peak: Rc::new(Budget {
                limit: Cell::new(InstanceLimits::default().max_peak_bytes),
                ..Default::default()
            }),
        }
    }
}

struct Instance {
    // State is dropped before the pinned module and its dependencies.
    state: RefCell<State>,
    module: LoadedModule,
    ty: Type,
    closed: Cell<bool>,
}
struct State {
    storage: Option<Storage>,
    failure: Option<Error>,
}
struct Storage {
    arena: Arena,
    pointer: NonNull<u8>,
    charge: usize,
    overhead: usize,
    usage: Rc<Usage>,
    peak: Rc<Budget>,
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
            .filter(|&n| n <= self.limits.max_bytes)
            .ok_or_else(|| {
                error(
                    ErrorCode::ResourceLimit,
                    "Persistent instance byte budget exceeded",
                )
            })?;
        if self.usage.roots.get() >= self.limits.max_instances
            || self.entries.borrow().len() >= self.limits.max_instances
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
            .filter(|(_, ty)| ty.is_buffer())
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
    fn instance_info(&self, name: &str) -> Result<FunctionInfo, Error> {
        match self {
            Self::Jit(module) => module.instance_entries.get(name).map(|f| &f.info),
            Self::Interpreter(module) => interp::instance_info(module, name),
        }
        .cloned()
        .ok_or_else(|| error(ErrorCode::NotFound, format!("Function `{name}` not found")))
    }

    fn invoke_instance(
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

fn initialize(storage: &Storage, ty: &Type, words: &[u64]) -> Result<(), Error> {
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
            let p = storage.pointer.as_ptr().add(*offset);
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
            .instances
            .usage
            .bytes
            .get()
            .checked_sub(storage.charge)
            .and_then(|n| n.checked_add(charge))
            .filter(|&n| n <= self.instances.limits.max_bytes)
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
        self.instances.usage.bytes.set(bytes);
        Ok(())
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
        self.instances.limits = limits;
        Ok(())
    }

    /// Construct a checked, reference-free record using a synchronous Joky function.
    /// Parameters are typed scalars; reference-free owning container state is retained.
    pub fn create_instance(
        &self,
        constructor: &str,
        args: &[InstanceValue],
    ) -> Result<InstanceId, Error> {
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
            Some(self.instances.limits.max_call_bytes),
        );
        arena.credit_imported();
        let result =
            module.invoke_instance(constructor, &words, self.interpreter_limits(), &mut arena)?;
        initialize(&storage, &info.signature.ret, &result)?;
        storage.cleanup = Some(descriptor);
        if let Err(error) = self.retain_instance(&mut storage, &mut arena) {
            let _ = storage.destroy_in(&mut arena);
            return Err(error);
        }
        let id = InstanceId {
            vm: self.instances.identity,
            sequence,
        };
        let instance = Rc::new(Instance {
            module,
            ty: info.signature.ret,
            closed: Cell::new(false),
            state: RefCell::new(State {
                storage: Some(storage),
                failure: None,
            }),
        });
        let mut entries = self.instances.entries.borrow_mut();
        // A constructor may synchronously create or fail other instances.
        if entries.len() >= self.instances.limits.max_instances {
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
        let instance = self.instances.get(id)?;
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
        let storage = state
            .storage
            .as_mut()
            .ok_or_else(|| invalid("Instance has failed"))?;
        let mut arena = Arena::with_budget(
            self.instances.peak.clone(),
            Some(self.instances.limits.max_call_bytes),
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
            .invoke_instance(method, &words, self.interpreter_limits(), &mut arena)
            .and_then(|words| {
                InstanceValue::from_bits(&info.signature.ret, words.first().copied().unwrap_or(0))
                    .ok_or_else(|| invalid("Invalid instance scalar result"))
            })
            .and_then(|value| {
                if instance.closed.get() {
                    return Err(invalid("Instance was closed during its call"));
                }
                self.retain_instance(storage, &mut arena)?;
                Ok(value)
            });
        if let Err(e) = &result {
            let _ = storage.destroy_in(&mut arena);
            state.storage.take();
            state.failure = Some(e.clone());
        }
        result
    }

    /// Invalidate the ID immediately. Reentrant close defers freeing storage
    /// until the active call exits; no pointer in that call becomes dangling.
    pub fn close_instance(&self, id: InstanceId) -> Result<(), Error> {
        let instance = self.instances.get(id)?;
        instance.closed.set(true);
        self.instances.entries.borrow_mut().remove(&id);
        if let Ok(mut state) = instance.state.try_borrow_mut() {
            if let Some(mut storage) = state.storage.take() {
                return storage.destroy();
            }
        }
        Ok(())
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
