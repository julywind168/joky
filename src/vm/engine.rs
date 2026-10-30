//! Embedding API with immutable compiled exports and checked host declarations.
use super::InterpreterLimits;
use crate::core::builtins::{Builtin, ExternAbi};
use crate::{
    backends::{
        interp,
        jit::{self, CompiledFunction, CompiledModule},
    },
    core::prepared::PreparedModule,
    modules::ModuleRegistry,
    parser::{self, ast::Item},
    runtime::{
        bindings::{HostAbiKind, HostBinding, HostEntry},
        borrowed_host::BorrowingHostV1,
        cps::{CpsEntry, CpsFrame},
        suspension::{Continuation, ModuleId, StringContinuation},
    },
    Error, ErrorCode, ErrorPhase, FunctionSignature, Type, TypeRegistry,
};
use std::{any::Any, collections::HashMap, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmMode {
    Jit,
    Interpreter,
}

#[derive(Clone)]
pub(super) enum LoadedModule {
    Jit(Rc<CompiledModule>),
    Interpreter(Rc<interp::BytecodeModule>),
}

pub struct VmEngine {
    mode: VmMode,
    files: Option<crate::runtime::fs::FileService>,
    module_sources: crate::ModuleSources,
    script_contracts: HashMap<String, (String, String)>,
    pub(super) current: Option<LoadedModule>,
    pub(super) instances: Rc<super::instances::Instances>,
    pub(super) domains: Rc<std::cell::RefCell<super::state_domains::Domains>>,
    pub(super) executor: Rc<crate::runtime::executor::HostExecutor>,
    hosts: HashMap<String, HostBinding>,
    types: TypeRegistry,
    next_version: u64,
    registry: Rc<ModuleRegistry>,
    interpreter_limits: InterpreterLimits,
}

// Keep signature checks, interpreter values and native dispatch in one definition.
macro_rules! scalar_calls {
    ($($variant:ident($rust:ty) {
        $(fn $method:ident($($arg:ident: $arg_ty:ty => $param:ident),*) = $native:ident;)*
    })*) => {
        $($(#[doc = concat!("Call a synchronous `", stringify!($variant),
            "` export with the declared parameter types.")]
        pub fn $method(&self, name: &str, $($arg: $arg_ty),*) -> Result<$rust, Error> {
            let params = &[$(Type::$param),*];
            if self.mode == VmMode::Interpreter {
                return match self.call_interpreter_values(
                    name, &[$(interp::Value::$param($arg)),*], params, &Type::$variant,
                )? {
                    interp::Value::$variant(value) => Ok(value),
                    _ => Err(runtime_error(
                        ErrorCode::InvalidState,
                        format!("Function `{name}` did not return {}", stringify!($variant)),
                    )),
                };
            }
            let ptr = self.sync_ptr(name, params, Type::$variant)?;
            self.native_result(unsafe { crate::runtime::abi::$native(ptr, $($arg),*) })
        })*)*
    };
}

macro_rules! async_scalar_calls {
    ($($(#[$doc:meta])* fn $method:ident($rust:ty => $variant:ident);)*) => {
        $($(#[$doc])*
        pub fn $method(&self, name: &str, args: &[$rust]) -> Result<Continuation, Error> {
            let values: Vec<_> = args.iter().copied().map(interp::Value::$variant).collect();
            self.start_async(name, &values, Type::$variant)
        })*
    };
}

impl VmEngine {
    pub fn new() -> Self {
        Self::with_mode(VmMode::Jit)
    }

    pub fn with_mode(mode: VmMode) -> Self {
        Self::with_mode_and_registry(mode, crate::stdlib::module_registry())
    }

    /// Freeze one entry and owning inputs for transfer to a worker. Submission
    /// runs join/race/branch on the selected parallel executor.
    /// Unsupported storage, calls and legacy host bindings fail before execution.
    pub fn prepare_parallel_task(
        &self,
        name: &str,
        input: Vec<super::TaskValue>,
        budgets: Vec<crate::runtime::parallel::TaskBudget>,
    ) -> Result<super::PreparedParallelTask, Error> {
        match self
            .current
            .as_ref()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No module loaded"))?
        {
            LoadedModule::Jit(module) => super::PreparedParallelTask::native(
                module.freeze(name)?,
                name,
                input,
                budgets,
                self.interpreter_limits,
            ),
            LoadedModule::Interpreter(module) => super::PreparedParallelTask::interpreted(
                module.freeze(name)?,
                name,
                input,
                budgets,
                self.interpreter_limits,
            ),
        }
    }

    /// Exact source-visible task types, including aggregates outside the scalar host ABI.
    pub fn parallel_signature(&self, name: &str) -> Result<FunctionSignature, Error> {
        match self
            .current
            .as_ref()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No module loaded"))?
        {
            LoadedModule::Jit(module) => Ok(module.freeze(name)?.info.signature),
            LoadedModule::Interpreter(module) => Ok(module.freeze(name)?.info.signature),
        }
    }

    /// Explicitly admit a scalar CPS host on any execution worker.
    ///
    /// # Safety
    /// In addition to the ordinary CPS ABI, entry and all transitive callees,
    /// stop hooks and destructors must support simultaneous calls on arbitrary
    /// threads. Every poll must return. V1 retains progress only in the declared
    /// scalar slots: no host_data allocation, host_context, stop hook or retained
    /// waker. Such capabilities need a separately audited adapter.
    /// No VM, thread-bound handle or hidden local capability may escape into the
    /// frame. `owner` keeps every code/data dependency alive and is safe to drop
    /// on any thread. Complete, exact slot metadata is mandatory. Old registrations
    /// stay Local; replacing this binding with one revokes admission for new loads.
    pub unsafe fn register_parallel_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        entry: CpsEntry,
        frame_slots: usize,
        owner: std::sync::Arc<dyn Any + Send + Sync>,
    ) -> Result<(), Error> {
        if !signature
            .params
            .iter()
            .chain(std::iter::once(&signature.ret))
            .all(crate::core::instance::scalar)
        {
            return Err(abi_error(
                ErrorCode::AbiError,
                "Parallel host v1 requires scalar inputs and result",
            ));
        }
        let types = self.validate_registration(name, &signature, false, Some(frame_slots))?;
        let host = super::parallel::host(name, signature, entry, frame_slots, owner);
        self.hosts.insert(name.into(), host.binding());
        self.types = types;
        Ok(())
    }

    /// Inject one immutable binding snapshot for all loads and both VM backends.
    pub(crate) fn with_mode_and_registry(mode: VmMode, registry: Rc<ModuleRegistry>) -> Self {
        let executor = Rc::new(crate::runtime::executor::HostExecutor::default());
        let domains = executor.domains();
        Self {
            mode,
            files: None,
            module_sources: Default::default(),
            script_contracts: Default::default(),
            current: None,
            instances: Default::default(),
            domains,
            executor,
            hosts: HashMap::new(),
            types: registry.types().clone(),
            next_version: 1,
            registry,
            interpreter_limits: InterpreterLimits::default(),
        }
    }

    /// Configure the capability before loading code; compiled snapshots retain it.
    pub fn set_file_service(
        &mut self,
        service: crate::runtime::fs::FileService,
    ) -> Result<(), Error> {
        if self.current.is_some() {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "Configure file service before loading a module",
            ));
        }
        self.files = Some(service);
        Ok(())
    }

    pub fn new_interpreter() -> Self {
        Self::with_mode(VmMode::Interpreter)
    }

    pub fn mode(&self) -> VmMode {
        self.mode
    }

    /// Enable or disable the default prelude for subsequent source loads.
    /// Existing compiled modules and tasks retain their original imports.
    pub fn set_prelude_enabled(&mut self, enabled: bool) {
        self.types.set_prelude_enabled(enabled);
    }

    /// Borrow the configured type and host declaration snapshot for source resolution.
    pub fn type_registry(&self) -> &TypeRegistry {
        &self.types
    }

    pub fn prelude_enabled(&self) -> bool {
        self.types.prelude_enabled()
    }

    /// Configure subsequent interpreter calls without changing existing tasks.
    /// JIT execution does not implement these limits and rejects this operation.
    pub fn set_interpreter_limits(&mut self, limits: InterpreterLimits) -> Result<(), Error> {
        if self.mode != VmMode::Interpreter {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                "Execution limits require interpreter mode",
            ));
        }
        if limits.max_frames == 0 || limits.poll_budget == Some(0) {
            return Err(runtime_error(
                ErrorCode::InvalidArgument,
                "Frame and poll limits must be positive",
            ));
        }
        self.interpreter_limits = limits;
        Ok(())
    }

    /// Limits that will be copied into subsequent interpreter invocations.
    pub fn interpreter_limits(&self) -> InterpreterLimits {
        self.interpreter_limits
    }

    /// Register an opaque host type used by source-level declarations.
    pub fn register_opaque_type(&mut self, name: impl Into<String>) -> Result<(), Error> {
        self.types.register_opaque(name)
    }

    /// Register a non-Copy external resource with automatic cleanup.
    pub fn register_resource_type(&mut self, name: impl Into<String>) -> Result<Type, Error> {
        self.types.register_resource(name)
    }

    /// Register a synchronous UInt64 -> resource factory.
    /// # Safety
    /// Callbacks obey ResourceFactoryV1. owner pins callback code until every
    /// task and persistent instance using the registration has been destroyed.
    /// With no owner, callbacks must remain valid for the process lifetime.
    pub unsafe fn register_resource_factory(
        &mut self,
        name: &str,
        resource: &str,
        factory: crate::runtime::resource::ResourceFactoryV1,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        self.validate_entry(name, false)?;
        factory.validate()?;
        crate::runtime::resource::validate_name(name)?;
        crate::runtime::tcp::validate_external_resource(resource)?;
        let result = self.types.resource_type(resource)?;
        self.types.resource_factories.insert(
            name.into(),
            FunctionSignature::new(vec![Type::UInt64], result.clone()),
        );
        self.types.resource_async.remove(name);
        self.types.resource_io.remove(name);
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature: FunctionSignature::new(vec![Type::UInt64], result),
                entry: HostEntry::ResourceFactory(Rc::new(factory)),
                owner,
            },
        );
        Ok(())
    }

    /// Register a suspending UInt64 -> resource factory.
    /// # Safety
    /// Callbacks obey ResourceFactoryCpsV1. owner pins callback code until every
    /// task and persistent instance using the registration has been destroyed.
    /// With no owner, callbacks must remain valid for the process lifetime.
    pub unsafe fn register_resource_factory_async(
        &mut self,
        name: &str,
        resource: &str,
        factory: crate::runtime::resource::ResourceFactoryCpsV1,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        self.validate_entry(name, false)?;
        factory.validate()?;
        crate::runtime::resource::validate_name(name)?;
        crate::runtime::tcp::validate_external_resource(resource)?;
        let result = self.types.resource_type(resource)?;
        self.types.resource_factories.insert(
            name.into(),
            FunctionSignature::new(vec![Type::UInt64], result.clone()),
        );
        self.types.resource_io.remove(name);
        self.types.resource_async.insert(name.into());
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature: FunctionSignature::new(vec![Type::UInt64], result),
                entry: HostEntry::ResourceFactoryCps(Rc::new(factory)),
                owner,
            },
        );
        Ok(())
    }

    /// Register bounded byte I/O with an exclusive resource receiver.
    /// Read: (&mut Resource, UInt64) @suspends -> Array<UInt8>.
    /// Write: (&mut Resource, Array<UInt8>) @suspends -> UInt64.
    /// # Safety
    /// Callbacks obey ResourceIoV1 and understand this resource's token type.
    /// owner retains their code; with None the callbacks have process lifetime.
    pub unsafe fn register_resource_io(
        &mut self,
        name: &str,
        resource: &str,
        kind: crate::runtime::byte_request::ByteIoKind,
        descriptor: crate::runtime::byte_request::ResourceIoV1,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        self.validate_entry(name, false)?;
        crate::runtime::resource::validate_name(name)?;
        descriptor.validate()?;
        crate::runtime::tcp::validate_external_resource(resource)?;
        let signature = self.types.resource_io_signature(resource, kind)?;
        self.types
            .resource_factories
            .insert(name.into(), signature.clone());
        self.types.resource_async.insert(name.into());
        self.types.resource_io.insert(name.into(), kind);
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature,
                entry: HostEntry::ResourceIo {
                    descriptor: Rc::new(descriptor),
                    kind,
                },
                owner,
            },
        );
        Ok(())
    }

    /// Register a direct host function. Replaces any registration with this name.
    /// Existing compiled modules retain their previously resolved functions.
    ///
    /// # Safety
    /// `ptr` must be a live C ABI function with exactly `signature`. It must remain
    /// callable for every compiled module using it, and must not unwind into JIT code.
    /// String signatures require the explicit TextViewV2 registration APIs.
    /// Returns an ABI error without changing the registration table when the
    /// name, pointer, signature types, or built-in binding conflict is invalid.
    pub unsafe fn register_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        ptr: *const u8,
    ) -> Result<(), Error> {
        self.register_owned_fn(name, signature, ptr, None)
    }

    pub(crate) unsafe fn register_owned_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        ptr: *const u8,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        let types = self.validate_registration(name, &signature, ptr.is_null(), None)?;
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature,
                entry: HostEntry::Direct(ptr),
                owner,
            },
        );
        self.types = types;
        Ok(())
    }

    /// Register a UTF-8 callback using explicit JokyText descriptors.
    /// Text inputs are valid only during the callback; results are copied before
    /// the adapter returns. A supplied owner retains callback code and dependencies.
    ///
    /// # Safety
    /// `ptr` must match the scalar C wire signature (String and &String use
    /// pointers to JokyText descriptors). Returned views must remain readable until copied,
    /// and the callback must not unwind, retain inputs, or reenter with conflicting accesses.
    pub unsafe fn register_text_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        ptr: *const u8,
    ) -> Result<(), Error> {
        self.register_owned_text_fn(name, signature, ptr, None)
    }

    /// Register a text callback with a code lifetime owner.
    ///
    /// # Safety
    /// The contract is identical to register_text_fn; owner must retain the callback.
    pub unsafe fn register_owned_text_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        ptr: *const u8,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        self.validate_entry(name, ptr.is_null())?;
        let mut types = self.types.clone();
        crate::modules::validate_text_host_types(&mut types, &signature, false)?;
        if !signature.has_text() {
            return Err(abi_error(
                ErrorCode::AbiError,
                "TextViewV2 requires a text signature",
            ));
        }
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::TextViewV2,
                signature,
                entry: HostEntry::Direct(ptr),
                owner,
            },
        );
        self.types = types;
        Ok(())
    }

    /// Register a text CPS callback, borrowing owned inputs through Pending.
    ///
    /// # Safety
    /// Each String parameter slot holds a pointer to a JokyText descriptor; a
    /// String result is a descriptor pointer in frame.result.
    /// The callback obeys the CPS contract and may retain input views only until
    /// completion or cancellation. Results must remain readable until the adapter copies them.
    /// Frame slots, if supplied, cover all parameters and match this callback.
    /// Owner, if supplied, retains all callback dependencies.
    pub unsafe fn register_text_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        entry: CpsEntry,
    ) -> Result<(), Error> {
        self.register_owned_text_suspending_fn(name, signature, entry, None, None)
    }

    /// Register text CPS code with optional complete frame metadata and owner.
    ///
    /// # Safety
    /// The contract is identical to register_text_suspending_fn.
    pub unsafe fn register_owned_text_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        entry: CpsEntry,
        frame_slots: Option<usize>,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        self.validate_entry(name, false)?;
        if !signature.has_text()
            || frame_slots.is_some_and(|slots| {
                !crate::modules::frame_slots_valid(slots, signature.params.len())
            })
        {
            return Err(abi_error(
                ErrorCode::AbiError,
                "Invalid TextViewV2 CPS signature or frame size",
            ));
        }
        let mut types = self.types.clone();
        crate::modules::validate_text_host_types(&mut types, &signature, true)?;
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::TextViewV2,
                signature,
                entry: HostEntry::Cps { entry, frame_slots },
                owner,
            },
        );
        self.types = types;
        Ok(())
    }

    /// Register a synchronous C import, separate from the legacy host ABI.
    /// Parameters are zero through four Ptr values or references to fixed-width
    /// numerics, named #repr(C) records, or nonempty fixed arrays of these types;
    /// the result is an integer, Ptr or Unit. Record names within arrays also
    /// resolve in each loaded module. Existing modules retain their bindings.
    ///
    /// # Safety
    /// The entry must have exactly the declared C ABI and target layout, including
    /// const/mutable reference permissions. Reference arguments may be accessed only
    /// through their exact pointee shape: an array reference is a pointer-to-array,
    /// not a decayed element pointer. Use a C wrapper to adapt an element-pointer API.
    /// Reference arguments may be accessed only
    /// during this call, never retained (including in a Ptr result), freed,
    /// reallocated, or accessed out of bounds. Ptr arguments and results use the
    /// C object-pointer ABI (void *); their nullability, validity, ownership and
    /// paired release follow the foreign API contract, which callers must uphold.
    /// Copying a Ptr does not keep its object or library alive. Joky never frees it.
    /// The entry must not unwind or reenter Joky with conflicting accesses.
    /// The calling thread must be allowed to invoke the entry.
    /// A supplied owner must retain the entry and all dependencies; without one,
    /// the entry must remain callable for every compiled module using it.
    pub unsafe fn register_c_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        ptr: *const u8,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        // C pointee names belong to the source module, not the opaque registry.
        self.validate_entry(name, ptr.is_null())?;
        crate::core::ffi::validate_registration(&signature)?;
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature,
                entry: HostEntry::C(ptr),
                owner,
            },
        );
        Ok(())
    }

    /// Register a synchronous C text adapter. V1 accepts up to four String or
    /// &String inputs and a String, integer or Unit result. Source uses extern fn.
    /// Existing modules retain their descriptor and code owner across replacement.
    ///
    /// # Safety
    /// Callbacks, context and returned pointers obey TextFfiHostV1's contract.
    /// Owner must retain every dependency; without one they must remain static.
    pub unsafe fn register_text_ffi_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        host: crate::runtime::text_ffi::TextFfiHostV1,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        self.validate_entry(name, false)?;
        host.validate(&signature)?;
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::TextFfiV1,
                signature,
                entry: HostEntry::TextFfi(Rc::new(host)),
                owner,
            },
        );
        Ok(())
    }

    /// Register an operation that borrows Joky objects across Pending.
    /// Its source declaration is `extern fn name(ref: &T) @suspends`; v1 accepts
    /// one through four references to integers or named #repr(C) records and an
    /// integer/Unit result. Record names resolve in each loaded source module.
    ///
    /// # Safety
    /// Callbacks must obey BorrowingHostV1, the declared layouts and permissions.
    /// In particular stop must synchronously end ALL external access, including
    /// callbacks queued before cancellation, on success, failure or cancellation.
    /// Operations requiring asynchronous cancellation cannot use this API.
    /// A supplied owner must retain callback code and dependencies until all
    /// compiled users and tasks drop; without one those dependencies are static.
    /// The runtime copies the descriptor; later registrations cannot change it.
    pub unsafe fn register_borrowing_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        host: BorrowingHostV1,
        owner: Option<Rc<dyn Any>>,
    ) -> Result<(), Error> {
        self.validate_entry(name, false)?;
        host.validate()?;
        crate::core::ffi::validate_borrowing_registration(&signature)?;
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature,
                entry: HostEntry::BorrowingCps(Rc::new(host)),
                owner,
            },
        );
        Ok(())
    }

    /// Register a static native callback. Only parameter slots are allocated.
    ///
    /// # Safety
    /// The callback must match `signature`, stay alive for every caller, and obey
    /// the CPS ABI. It must not retain frame pointers, resize slots, or unwind.
    /// Save native progress in state/host_data before returning Pending.
    /// Returns an ABI error without changing the registration table when the name,
    /// signature types, or built-in binding conflict is invalid.
    pub unsafe fn register_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        entry: CpsEntry,
    ) -> Result<(), Error> {
        let types = self.validate_registration(name, &signature, false, None)?;
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature,
                entry: HostEntry::Cps {
                    entry,
                    frame_slots: None,
                },
                owner: None,
            },
        );
        self.types = types;
        Ok(())
    }

    /// Register generated code with complete frame metadata and a lifetime owner.
    ///
    /// # Safety
    /// `entry` must match `signature` and the shared CPS runtime ABI. `frame_slots`
    /// must come from AotCompiler::cps_frame_slots for this exact entry. `owner`
    /// must keep the entry and all native dependencies alive while callers exist.
    /// Returns an ABI error without changing the registration table when the name,
    /// signature types, frame size, or built-in binding conflict is invalid.
    pub unsafe fn register_aot_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        entry: CpsEntry,
        frame_slots: usize,
        owner: Rc<dyn Any>,
    ) -> Result<(), Error> {
        let types = self.validate_registration(name, &signature, false, Some(frame_slots))?;
        self.hosts.insert(
            name.into(),
            HostBinding {
                abi: HostAbiKind::Scalar,
                signature,
                entry: HostEntry::Cps {
                    entry,
                    frame_slots: Some(frame_slots),
                },
                owner: Some(owner),
            },
        );
        self.types = types;
        Ok(())
    }

    pub(super) fn stage_module(
        &self,
        ast: &parser::ast::Module,
    ) -> Result<(LoadedModule, u64), Error> {
        let mut staging = Self::with_mode_and_registry(self.mode, self.registry.clone());
        staging.hosts = self.hosts.clone();
        staging.types = self.types.clone();
        staging.next_version = self.next_version;
        staging.interpreter_limits = self.interpreter_limits;
        staging.files = self.files.clone();
        staging.module_sources = self.module_sources.clone();
        staging.script_contracts = self.script_contracts.clone();
        staging.load_module(ast)?;
        Ok((staging.current.take().unwrap(), staging.next_version))
    }
    // The caller has completed all fallible checks; retire the old owner later.
    pub(super) fn install_staged(
        &mut self,
        module: LoadedModule,
        next_version: u64,
    ) -> Option<LoadedModule> {
        self.next_version = next_version;
        self.current.replace(module)
    }

    /// Parse, compile and publish a replacement only after all checks succeed.
    pub fn load_source(&mut self, source: &str) -> Result<Vec<String>, Error> {
        self.load_module(&parser::parse(source)?)
    }

    /// Supply an explicit dependency snapshot for subsequent compilations.
    /// This does not publish code or change already-running modules.
    pub fn add_source_module(&mut self, name: &str, source: &str) -> Result<(), Error> {
        self.module_sources.insert(name, source)
    }

    /// Require a complete typed Script entry before any subsequent load or publication.
    /// Schemas use the same encoding as the typed Script call bridge.
    pub fn require_script_entry(
        &mut self,
        name: &str,
        input: &str,
        output: &str,
    ) -> Result<(), Error> {
        if let Some(current) = &self.current {
            current.check_script_contract(name, input, output)?;
        }
        if let Some((old_input, old_output)) = self.script_contracts.get(name) {
            if old_input == input && old_output == output {
                return Ok(());
            }
            return Err(runtime_error(
                ErrorCode::TypeMismatch,
                "Conflicting required Script contract",
            ));
        }
        self.script_contracts.try_reserve(1).map_err(|_| {
            runtime_error(
                ErrorCode::ResourceLimit,
                "Script contract allocation failed",
            )
        })?;
        self.script_contracts
            .insert(name.to_owned(), (input.to_owned(), output.to_owned()));
        Ok(())
    }

    /// Load a source snapshot with a diagnostic filename, independently of module identity.
    pub fn load_named_source(&mut self, name: &str, source: &str) -> Result<Vec<String>, Error> {
        self.load_module(&Self::parse_named_source(name, source)?)
    }

    pub(super) fn parse_named_source(
        name: &str,
        source: &str,
    ) -> Result<parser::ast::Module, Error> {
        let file = crate::SourceFile::new(name, source);
        let mut sources = crate::SourceMap::default();
        sources.insert(file.clone());
        let mut ast = parser::parse_with_source(file.text(), file.id())
            .map_err(|error| Error::from(error).with_sources(&sources))?;
        ast.sources = std::sync::Arc::new(sources);
        Ok(ast)
    }

    /// Resolve user imports only from caller-supplied source snapshots.
    pub fn load_source_with_modules(
        &mut self,
        source: &str,
        modules: &crate::ModuleSources,
    ) -> Result<Vec<String>, Error> {
        let entry = crate::SourceFile::new("<entry>", source);
        let mut sources = crate::SourceMap::default();
        sources.insert(entry.clone());
        let mut ast = parser::parse_with_source(entry.text(), entry.id())
            .map_err(|e| Error::from(e).with_sources(&sources))?;
        ast.sources = std::sync::Arc::new(sources);
        let ast = self.types.resolve_with_modules(&ast, modules)?;
        self.load_module(&ast)
    }

    /// Compile a parsed or resolved module, publishing only after all checks pass.
    pub fn load_module(&mut self, ast: &parser::ast::Module) -> Result<Vec<String>, Error> {
        let mut types = self.types.clone();
        types.resource_factories.extend(
            self.hosts
                .iter()
                .filter(|(_, host)| {
                    matches!(
                        host.entry,
                        HostEntry::ResourceFactory(_)
                            | HostEntry::ResourceFactoryCps(_)
                            | HostEntry::ResourceIo { .. }
                    )
                })
                .map(|(name, host)| (name.clone(), host.signature.clone()))
                .collect::<HashMap<_, _>>(),
        );
        types.resource_async = self
            .hosts
            .iter()
            .filter(|(_, host)| {
                matches!(
                    host.entry,
                    HostEntry::ResourceFactoryCps(_) | HostEntry::ResourceIo { .. }
                )
            })
            .map(|(name, _)| name.clone())
            .collect();
        types.resource_io = self
            .hosts
            .iter()
            .filter_map(|(name, host)| match host.entry {
                HostEntry::ResourceIo { kind, .. } => Some((name.clone(), kind)),
                _ => None,
            })
            .collect();
        types.text_ffi = self
            .hosts
            .iter()
            .filter(|(_, host)| matches!(host.entry, HostEntry::TextFfi(_)))
            .map(|(name, host)| (name.clone(), host.signature.clone()))
            .collect();
        let ast = types.resolve_with_modules(ast, &self.module_sources)?;
        let prepared = PreparedModule::new(&ast, &types, self.registry.metadata())?;
        self.load_prepared(&prepared)
            .map_err(|error| error.with_sources(prepared.ast().sources()))
    }

    fn load_prepared(&mut self, prepared: &PreparedModule) -> Result<Vec<String>, Error> {
        let ast = prepared.ast();
        let mut missing_lazy_hosts = Vec::new();
        let mut symbols = Vec::new();
        let mut layouts = HashMap::new();
        let mut dependencies = Vec::new();
        let mut interpreter_hosts = HashMap::new();
        let nominals = if ast
            .items
            .iter()
            .any(|item| matches!(item, Item::ExternFn(f) if f.c_abi || crate::core::ffi::is_borrowing_import(f)))
        {
            crate::core::types::nominal_types(prepared.types(), ast)?
        } else {
            HashMap::new()
        };
        for item in &ast.items {
            let Item::ExternFn(f) = item else { continue };
            let builtin = Builtin::lookup(&f.name);
            if ast
                .scope
                .as_ref()
                .is_some_and(|scope| scope.lazy_hosts.contains(&f.name))
                && !builtin.as_ref().is_some_and(Builtin::needs_adapter)
                && self.registry.binding(&f.name).is_none()
                && !self.hosts.contains_key(&f.name)
            {
                missing_lazy_hosts.push(f);
                continue;
            }
            if let Some(builtin) = builtin.as_ref().filter(|op| op.needs_adapter()) {
                builtin.validate_declaration(item)?;
                let signature = FunctionSignature::new(
                    f.params.iter().map(|p| p.ty.clone()).collect(),
                    f.ret_ty.clone(),
                );
                let entry = match builtin {
                    Builtin::VmCall => HostEntry::VmCall,
                    Builtin::VmSource => HostEntry::VmSource,
                    Builtin::VmMigration(op) => HostEntry::VmMigration(*op),
                    Builtin::Domain(op) => HostEntry::Domain(*op),
                    Builtin::Tcp(op) => HostEntry::Tcp(*op),
                    Builtin::Fs(op) => HostEntry::Fs(*op),
                    Builtin::Dns => {
                        crate::runtime::dns::initialize()?;
                        HostEntry::Dns
                    }
                    Builtin::Vm(_) | Builtin::Stdout => unreachable!(),
                };
                if self.mode == VmMode::Interpreter {
                    interpreter_hosts.insert(
                        f.name.clone(),
                        HostBinding {
                            abi: HostAbiKind::Scalar,
                            signature,
                            entry,
                            owner: None,
                        },
                    );
                }
                continue;
            }
            let (symbol, mut host) = if let Some(binding) = self.registry.binding(&f.name) {
                let host = binding.host().ok_or_else(|| {
                    abi_error(
                        ErrorCode::HostError,
                        format!("Module binding `{}` has no native entry", f.name),
                    )
                    .with_span(f.span)
                })?;
                (binding.spec.symbol.as_deref().unwrap_or(&f.name), host)
            } else {
                let host = self.hosts.get(&f.name).ok_or_else(|| {
                    abi_error(
                        ErrorCode::HostError,
                        format!("Host function `{}` is not registered", f.name),
                    )
                    .with_span(f.span)
                })?;
                (f.name.as_str(), host.clone())
            };
            let signature = FunctionSignature::new(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                f.ret_ty.clone(),
            );
            if signature.has_text()
                && !matches!(host.abi, HostAbiKind::TextViewV2 | HostAbiKind::TextFfiV1)
            {
                return Err(abi_error(
                    ErrorCode::AbiError,
                    "String requires a TextViewV2 or TextFfiV1 registration",
                )
                .with_span(f.span));
            }
            let borrowing =
                ExternAbi::classify(f, prepared.types(), builtin.as_ref()) == ExternAbi::Borrowing;
            if borrowing != matches!(host.entry, HostEntry::BorrowingCps(_)) {
                return Err(abi_error(
                    ErrorCode::AbiError,
                    format!(
                        "Host function {} requires the matching borrowing host v1 registration",
                        f.name
                    ),
                )
                .with_span(f.span));
            }
            if f.c_abi != matches!(host.entry, HostEntry::C(_)) {
                return Err(abi_error(
                    ErrorCode::AbiError,
                    format!("Host function {} has an incompatible C/legacy ABI", f.name),
                )
                .with_span(f.span));
            }
            if borrowing {
                crate::core::ffi::validate_borrowing_signature(
                    &signature,
                    &nominals,
                    &self.types,
                    f.span,
                )?;
                host.signature = crate::core::ffi::resolve_registration(&host.signature, &nominals)
                    .map_err(|e| e.with_span(f.span))?;
            } else if f.c_abi {
                crate::core::ffi::validate_signature(
                    &signature,
                    f.suspends,
                    &nominals,
                    &self.types,
                    f.span,
                )?;
                host.signature = crate::core::ffi::resolve_registration(&host.signature, &nominals)
                    .map_err(|e| e.with_span(f.span))?;
            }
            if signature != host.signature
                || f.suspends
                    != matches!(
                        host.entry,
                        HostEntry::Cps { .. }
                            | HostEntry::BorrowingCps(_)
                            | HostEntry::ResourceFactoryCps(_)
                            | HostEntry::ResourceIo { .. }
                    )
            {
                return Err(abi_error(
                    ErrorCode::AbiError,
                    format!(
                        "Host function `{}` has an incompatible signature or @suspends ABI",
                        f.name
                    ),
                )
                .with_span(f.span));
            }
            let mut symbol = symbol.to_owned();
            let entry_ptr = match &host.entry {
                HostEntry::Domain(_)
                | HostEntry::Tcp(_)
                | HostEntry::Fs(_)
                | HostEntry::Dns
                | HostEntry::VmCall
                | HostEntry::VmSource
                | HostEntry::VmMigration(_) => {
                    unreachable!("Network runtime adapter")
                }
                HostEntry::Direct(ptr) | HostEntry::C(ptr) => {
                    if ptr.is_null() {
                        return Err(abi_error(
                            ErrorCode::AbiError,
                            format!("Host function `{}` has a null entry", f.name),
                        )
                        .with_span(f.span));
                    }
                    *ptr
                }
                HostEntry::Cps { entry, frame_slots } => {
                    if let Some(count) = frame_slots {
                        layouts.insert(f.name.clone(), *count);
                    }
                    *entry as *const u8
                }
                HostEntry::ResourceIo { descriptor, .. } => {
                    symbol = crate::runtime::byte_request::symbol(&symbol);
                    let ptr = Rc::as_ptr(descriptor) as *const u8;
                    dependencies.push(descriptor.clone() as Rc<dyn Any>);
                    ptr
                }
                HostEntry::ResourceFactoryCps(descriptor) => {
                    symbol = crate::runtime::resource::cps_symbol(&symbol);
                    let ptr = Rc::as_ptr(descriptor) as *const u8;
                    dependencies.push(descriptor.clone() as Rc<dyn Any>);
                    ptr
                }
                HostEntry::ResourceFactory(descriptor) => {
                    symbol = crate::runtime::resource::symbol(&symbol);
                    let ptr = Rc::as_ptr(descriptor) as *const u8;
                    dependencies.push(descriptor.clone() as Rc<dyn Any>);
                    ptr
                }
                HostEntry::TextFfi(descriptor) => {
                    symbol = crate::runtime::text_ffi::symbol(&symbol);
                    let ptr = Rc::as_ptr(descriptor) as *const u8;
                    dependencies.push(descriptor.clone() as Rc<dyn Any>);
                    ptr
                }
                HostEntry::BorrowingCps(descriptor) => {
                    symbol = crate::core::ffi::borrowing_symbol(&symbol);
                    let ptr = Rc::as_ptr(descriptor) as *const u8;
                    dependencies.push(descriptor.clone() as Rc<dyn Any>);
                    ptr
                }
            };
            match self.mode {
                VmMode::Jit => {
                    symbols.push((symbol, entry_ptr));
                    if let Some(owner) = host.owner {
                        dependencies.push(owner);
                    }
                }
                VmMode::Interpreter => {
                    interpreter_hosts.insert(f.name.clone(), host);
                }
            }
        }
        if !missing_lazy_hosts.is_empty() {
            let plan = prepared.plan(&layouts)?;
            for f in missing_lazy_hosts {
                if plan.functions.contains_key(&f.name) {
                    return Err(abi_error(
                        ErrorCode::HostError,
                        format!("Host function `{}` is not registered", f.name),
                    )
                    .with_span(f.span));
                }
            }
        }
        let next = self
            .next_version
            .checked_add(1)
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "Module version exhausted"))?;
        let version = ModuleId(self.next_version);
        let loaded = match self.mode {
            VmMode::Jit => {
                let mut compiled = jit::compile(
                    prepared,
                    &symbols
                        .iter()
                        .map(|(name, ptr)| (name.as_str(), *ptr))
                        .collect::<Vec<_>>(),
                    &layouts,
                    dependencies,
                    version,
                )?;
                compiled.files = self.files.clone();
                let compiled = Rc::new(compiled);
                crate::stdlib::track_module(&compiled);
                LoadedModule::Jit(compiled)
            }
            VmMode::Interpreter => {
                let mut compiled = interp::compile(prepared, &layouts, interpreter_hosts, version)?;
                compiled.files = self.files.clone();
                LoadedModule::Interpreter(Rc::new(compiled))
            }
        };
        let mut names: Vec<_> = match &loaded {
            LoadedModule::Jit(module) => module.exports.keys().cloned().collect(),
            LoadedModule::Interpreter(module) => interp::function_names(module),
        };
        names.sort();
        for (name, (input, output)) in &self.script_contracts {
            loaded.check_script_contract(name, input, output)?;
        }
        self.current = Some(loaded);
        self.next_version = next;
        Ok(names)
    }

    fn validate_entry(&self, name: &str, null_entry: bool) -> Result<(), Error> {
        if name.is_empty() || name.contains('\0') {
            return Err(abi_error(
                ErrorCode::AbiError,
                format!("Invalid host function name `{name}`"),
            ));
        }
        if name.starts_with("__joky_dns_")
            || name.starts_with("joky_dns_")
            || name.starts_with("__joky_vm_")
            || name.starts_with("__joky_resource_")
            || name.starts_with("joky_byte_request_")
            || name.starts_with("__joky_text_ffi_")
            || name.starts_with("__joky_string_")
            || name.starts_with("__joky_user_text_v2_")
            || name.starts_with("joky_string_result_")
        {
            return Err(abi_error(
                ErrorCode::ReservedName,
                "Managed runtime names are reserved",
            ));
        }
        if Builtin::lookup(name).is_some()
            || self.registry.binding(name).is_some()
            || matches!(name, "Ptr.null" | "Ptr.is_null")
            || crate::core::integer::IntegerType::conversion(name).is_some()
        {
            return Err(abi_error(
                ErrorCode::DuplicateDefinition,
                format!("Host function `{name}` conflicts with a built-in binding"),
            ));
        }
        if null_entry {
            return Err(abi_error(
                ErrorCode::AbiError,
                "Cannot register a null function entry",
            ));
        }
        Ok(())
    }

    fn validate_registration(
        &self,
        name: &str,
        signature: &FunctionSignature,
        null_entry: bool,
        frame_slots: Option<usize>,
    ) -> Result<TypeRegistry, Error> {
        self.validate_entry(name, null_entry)?;
        if signature.has_text() {
            return Err(abi_error(
                ErrorCode::AbiError,
                "String requires register_text_fn, register_text_suspending_fn or register_text_ffi_fn",
            ));
        }
        if frame_slots
            .is_some_and(|slots| !crate::modules::frame_slots_valid(slots, signature.params.len()))
        {
            return Err(abi_error(
                ErrorCode::AbiError,
                "Invalid CPS frame size for registered host function",
            ));
        }
        let mut types = self.types.clone();
        crate::modules::validate_host_types(&mut types, signature)?;
        Ok(types)
    }

    /// An owned snapshot; modifying it cannot change a compiled entry's ABI.
    pub fn function_signature(&self, name: &str) -> Option<FunctionSignature> {
        match self.current.as_ref()? {
            LoadedModule::Jit(module) => module.exports.get(name).map(|f| f.info.signature.clone()),
            LoadedModule::Interpreter(module) => interp::function_signature(module, name).cloned(),
        }
    }

    /// Physical ABI metadata; text entries require typed text root APIs.
    pub fn function_entry_abi(&self, name: &str) -> Option<crate::EntryAbi> {
        match self.current.as_ref()? {
            LoadedModule::Jit(module) => module.exports.get(name).map(|f| f.abi),
            LoadedModule::Interpreter(module) => interp::function_info(module, name).map(|f| {
                if f.signature.has_text() {
                    crate::EntryAbi::OwnedTextV2
                } else {
                    crate::EntryAbi::Scalar
                }
            }),
        }
    }

    pub fn module_version(&self) -> Option<ModuleId> {
        self.current.as_ref().map(|module| match module {
            LoadedModule::Jit(module) => module.version,
            LoadedModule::Interpreter(module) => module.version,
        })
    }

    fn function(&self, name: &str) -> Result<&CompiledFunction, Error> {
        let module = self
            .current
            .as_ref()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No module loaded"))?;
        let LoadedModule::Jit(module) = module else {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "Native function lookup is unavailable in interpreter mode",
            ));
        };
        module.exports.get(name).ok_or_else(|| {
            runtime_error(ErrorCode::NotFound, format!("Function `{name}` not found"))
        })
    }

    fn sync_ptr(&self, name: &str, params: &[Type], ret: Type) -> Result<*const u8, Error> {
        let function = self.function(name)?;
        if function.abi != crate::EntryAbi::Scalar {
            return Err(abi_error(
                ErrorCode::AbiError,
                "Owned text entries require typed String root APIs",
            ));
        }
        if function.info.suspends {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Function `{name}` requires call_async"),
            ));
        }
        if function.info.signature.params != params || function.info.signature.ret != ret {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Call signature mismatch for `{name}`"),
            ));
        }
        Ok(function.entry)
    }

    fn native_result<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        result.map_err(|error| match &self.current {
            Some(LoadedModule::Jit(module)) => error.with_sources(&module.sources),
            _ => error,
        })
    }

    /// Copy a synchronous String root result into a host Rust String.
    pub fn call_string_0(&self, name: &str) -> Result<String, Error> {
        self.call_text(name, None)
    }
    /// Copy the input into the task and the completed output back into host storage.
    pub fn call_string_1(&self, name: &str, text: &str) -> Result<String, Error> {
        self.call_text(name, Some(text))
    }

    fn text_signature(&self, name: &str, input: Option<&str>, suspends: bool) -> Result<(), Error> {
        if input.is_some_and(|text| text.len() > crate::runtime::arena::MAX_BYTES) {
            return Err(runtime_error(
                ErrorCode::ResourceLimit,
                "Text input exceeds the task byte limit",
            ));
        }
        let info = match self.current.as_ref() {
            Some(LoadedModule::Jit(module)) => module.exports.get(name).map(|f| &f.info),
            Some(LoadedModule::Interpreter(module)) => interp::function_info(module, name),
            None => None,
        }
        .ok_or_else(|| {
            runtime_error(ErrorCode::NotFound, format!("Function `{name}` not found"))
        })?;
        let params = if input.is_some() {
            vec![Type::String]
        } else {
            vec![]
        };
        if info.suspends != suspends
            || info.signature != FunctionSignature::new(params, Type::String)
        {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                "Text call signature mismatch",
            ));
        }
        Ok(())
    }

    fn call_text(&self, name: &str, input: Option<&str>) -> Result<String, Error> {
        self.text_signature(name, input, false)?;
        if let Some(LoadedModule::Interpreter(module)) = self.current.as_ref() {
            let args: Vec<_> = input
                .into_iter()
                .map(|text| interp::Value::String(std::sync::Arc::from(text)))
                .collect();
            let interp::Value::String(text) =
                interp::call_sync(module, name, &args, self.interpreter_limits)?
            else {
                return Err(abi_error(ErrorCode::AbiError, "Invalid text root result"));
            };
            return Ok(text.to_string());
        }
        let entry = self.function(name)?.entry;
        let mut output = crate::runtime::string::JokyOwnedTextV2::default();
        let result = crate::runtime::error::checked(|| unsafe {
            match input {
                Some(text) => {
                    let call: unsafe extern "C" fn(
                        *const u8,
                        u64,
                        *mut crate::runtime::string::JokyOwnedTextV2,
                    ) -> i32 = std::mem::transmute(entry);
                    call(text.as_ptr(), text.len() as u64, &mut output)
                }
                None => {
                    let call: unsafe extern "C" fn(
                        *mut crate::runtime::string::JokyOwnedTextV2,
                    ) -> i32 = std::mem::transmute(entry);
                    call(&mut output)
                }
            }
        });
        if !matches!(result, Ok(0)) {
            unsafe {
                crate::runtime::string::joky_string_result_free_v2(&mut output);
            }
            return self.native_result(result.and_then(|_| {
                Err(abi_error(
                    ErrorCode::AbiError,
                    "Text root failed without a runtime error",
                ))
            }));
        }
        if output.data.is_null() {
            return Ok(String::new());
        }
        // The checked v2 descriptor guarantees a matching allocator and valid UTF-8.
        let bytes = unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                output.data,
                output.byte_length as usize,
            ))
        }
        .into_vec();
        String::from_utf8(bytes)
            .map_err(|_| abi_error(ErrorCode::AbiError, "Invalid UTF-8 root result"))
    }

    /// Start a text task; completion captures host-owned text before task cleanup.
    pub fn call_async_string_0(&self, name: &str) -> Result<StringContinuation, Error> {
        self.start_text(name, None)
    }
    /// Start a text task with a copied input. Pending never retains the host borrow.
    pub fn call_async_string_1(&self, name: &str, text: &str) -> Result<StringContinuation, Error> {
        self.start_text(name, Some(text))
    }

    fn start_text(&self, name: &str, input: Option<&str>) -> Result<StringContinuation, Error> {
        self.text_signature(name, input, true)?;
        let mut task = match self.current.as_ref().unwrap() {
            LoadedModule::Interpreter(module) => {
                let task =
                    interp::start_text(module.clone(), name, input, self.interpreter_limits)?;
                Continuation::new_interpreted(task, module.version, module.clone())
            }
            LoadedModule::Jit(module) => {
                let function = &module.exports[name];
                let mut frame = CpsFrame::new(function.info.frame_slots);
                frame.root_text = true;
                if let Some(text) = input {
                    crate::runtime::error::checked(|| unsafe {
                        let arena = crate::runtime::cps::arena(&mut frame);
                        let owner = crate::runtime::owned_text::input(
                            arena,
                            text.as_ptr(),
                            text.len() as u64,
                        );
                        if !owner.is_null() {
                            frame.slots[0] = owner as usize as u64;
                            let cleanup = crate::runtime::cps::cleanup(&mut frame);
                            (*cleanup).entries.insert(
                                0,
                                crate::runtime::cleanup::Entry {
                                    keep_alive: None,
                                    storage_size: 0,
                                    storage: None,
                                    owners: vec![Some(owner)],
                                    hooks: vec![],
                                },
                            );
                        }
                    })?;
                }
                unsafe {
                    Continuation::new(
                        crate::runtime::abi::as_cps_entry(function.entry),
                        frame,
                        module.version,
                        module.clone(),
                    )
                }
                .with_sources(module.sources.clone())
            }
        };
        task.resume()?;
        Ok(StringContinuation::new(task))
    }

    scalar_calls! {
        Int32(i32) {
            fn call_int32_2(a: i32 => Int32, b: i32 => Int32) = call_i32_2;
            fn call_int32_1(a: i32 => Int32) = call_i32_1;
            fn call_int32_0() = call_i32_0;
        }
        Int64(i64) {
            fn call_int64_2(a: i64 => Int64, b: i64 => Int64) = call_i64_2;
            fn call_int64_1(a: i64 => Int64) = call_i64_1;
            fn call_int64_0() = call_i64_0;
        }
        Float32(f32) {
            fn call_float32_2(a: f32 => Float32, b: f32 => Float32) = call_f32_2;
            fn call_float32_1(a: f32 => Float32) = call_f32_1;
            fn call_float32_0() = call_f32_0;
        }
        Float64(f64) {
            fn call_float64_2(a: f64 => Float64, b: f64 => Float64) = call_f64_2;
            fn call_float64_3(a: f64 => Float64, b: f64 => Float64, c: f64 => Float64) = call_f64_3;
            fn call_float64_f64_i32(a: f64 => Float64, b: i32 => Int32) = call_f64_i32;
            fn call_float64_i32_f64(a: i32 => Int32, b: f64 => Float64) = call_i32_f64;
            fn call_float64_1(a: f64 => Float64) = call_f64_1;
            fn call_float64_0() = call_f64_0;
        }
        UInt8(u8) {
            fn call_uint8_2(a: u8 => UInt8, b: u8 => UInt8) = call_u8_2;
            fn call_uint8_1(a: u8 => UInt8) = call_u8_1;
            fn call_uint8_0() = call_u8_0;
        }
        Int8(i8) {
            fn call_i8_2(a: i8 => Int8, b: i8 => Int8) = call_i8_2;
            fn call_i8_1(a: i8 => Int8) = call_i8_1;
            fn call_i8_0() = call_i8_0;
        }
        Int16(i16) {
            fn call_i16_2(a: i16 => Int16, b: i16 => Int16) = call_i16_2;
            fn call_i16_1(a: i16 => Int16) = call_i16_1;
            fn call_i16_0() = call_i16_0;
        }
        UInt16(u16) {
            fn call_u16_2(a: u16 => UInt16, b: u16 => UInt16) = call_u16_2;
            fn call_u16_1(a: u16 => UInt16) = call_u16_1;
            fn call_u16_0() = call_u16_0;
        }
        UInt32(u32) {
            fn call_uint32_2(a: u32 => UInt32, b: u32 => UInt32) = call_u32_2;
            fn call_uint32_1(a: u32 => UInt32) = call_u32_1;
            fn call_uint32_0() = call_u32_0;
        }
        UInt64(u64) {
            fn call_uint64_2(a: u64 => UInt64, b: u64 => UInt64) = call_u64_2;
            fn call_uint64_1(a: u64 => UInt64) = call_u64_1;
            fn call_uint64_0() = call_u64_0;
        }
    }

    pub fn call_unit_0(&self, name: &str) -> Result<(), Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[]);
        }
        let ptr = self.sync_ptr(name, &[], Type::Unit)?;
        self.native_result(unsafe { crate::runtime::abi::call_unit_0(ptr) })?;
        Ok(())
    }
    pub fn call_unit_1(&self, name: &str, a: i32) -> Result<(), Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[a]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32], Type::Unit)?;
        self.native_result(unsafe { crate::runtime::abi::call_unit_1(ptr, a) })?;
        Ok(())
    }
    pub fn call_unit_2(&self, name: &str, a: i32, b: i32) -> Result<(), Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[a, b]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Unit)?;
        self.native_result(unsafe { crate::runtime::abi::call_unit_2(ptr, a, b) })?;
        Ok(())
    }

    /// Prepare a scalar async root without running user code. Register it with
    /// HostExecutor to apply its clock and wake context from the first poll.
    pub fn prepare_async(
        &self,
        name: &str,
        args: &[super::InstanceValue],
    ) -> Result<Continuation, Error> {
        self.current
            .as_ref()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No module loaded"))?
            .prepare_async(name, args, self.interpreter_limits)
    }

    pub(crate) fn script_code_owner(&self) -> Option<Rc<dyn Any>> {
        self.current.as_ref().map(|module| match module {
            LoadedModule::Jit(module) => module.clone() as Rc<dyn Any>,
            LoadedModule::Interpreter(module) => module.clone() as Rc<dyn Any>,
        })
    }

    pub(crate) fn script_signature(&self, name: &str) -> Option<FunctionSignature> {
        let info = match self.current.as_ref()? {
            LoadedModule::Jit(module) => module.request_entries.get(name).map(|f| &f.info),
            LoadedModule::Interpreter(module) => interp::script_info(module, name),
        }?;
        Some(info.signature.clone())
    }

    pub(crate) fn prepare_script_call(
        &self,
        name: &str,
        input_schema: &str,
        result_schema: &str,
        bits: &[u64],
    ) -> Result<(Continuation, Option<(usize, usize)>), Error> {
        let module = self
            .current
            .as_ref()
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "No module loaded"))?;
        module.prepare_script_call(
            name,
            input_schema,
            result_schema,
            bits,
            self.interpreter_limits,
        )
    }

    async_scalar_calls! {
        /// Run to the first pending poll or completion. Int32 arguments and an
        /// Int32/Unit result are supported by this convenience entry point.
        fn call_async(i32 => Int32);

        /// Start an Int64/Unit suspending export with Int64 arguments. The result
        /// retains all 64 bits in `Continuation::cps_result`; interpret it as i64.
        fn call_async_int64(i64 => Int64);

        /// Start a Float32/Unit suspending export with Float32 arguments.
        /// `Continuation::cps_result` zero-extends the result to u64.
        fn call_async_float32(f32 => Float32);

        /// Start a Float64/Unit suspending export with Float64 arguments.
        /// `Continuation::cps_result` returns the raw f64 bit pattern as u64.
        fn call_async_float64(f64 => Float64);

        /// Start a UInt8/Unit suspending export with UInt8 arguments.
        /// `Continuation::cps_result` zero-extends the result to u64.
        fn call_async_uint8(u8 => UInt8);

        /// Start a Int8/Unit suspending export with Int8 arguments.
        /// `Continuation::cps_result` zero-extends the result to u64.
        fn call_async_i8(i8 => Int8);

        /// Start a Int16/Unit suspending export with Int16 arguments.
        /// `Continuation::cps_result` zero-extends the result to u64.
        fn call_async_i16(i16 => Int16);

        /// Start a UInt16/Unit suspending export with UInt16 arguments.
        /// `Continuation::cps_result` zero-extends the result to u64.
        fn call_async_u16(u16 => UInt16);

        /// Start a UInt32/Unit suspending export with UInt32 arguments.
        /// `Continuation::cps_result` zero-extends the result to u64.
        fn call_async_uint32(u32 => UInt32);

        /// Start a UInt64/Unit suspending export with UInt64 arguments.
        /// `Continuation::cps_result` retains all 64 bits.
        fn call_async_uint64(u64 => UInt64);
    }

    fn start_async(
        &self,
        name: &str,
        args: &[interp::Value],
        integer: Type,
    ) -> Result<Continuation, Error> {
        if self.mode == VmMode::Interpreter {
            let Some(LoadedModule::Interpreter(module)) = self.current.as_ref() else {
                return Err(runtime_error(ErrorCode::InvalidState, "No module loaded"));
            };
            let task = interp::start(
                module.clone(),
                name,
                args,
                &integer,
                self.interpreter_limits,
            )?;
            let mut task = Continuation::new_interpreted(task, module.version, module.clone());
            task.resume()?;
            return Ok(task);
        }
        let function = self.function(name)?;
        let info = &function.info;
        if !info.suspends {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Function `{name}` is not marked @suspends"),
            ));
        }
        if !info.signature.accepts_integer_async(args.len(), &integer) {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Async call signature mismatch for `{name}`"),
            ));
        }
        let Some(LoadedModule::Jit(module)) = self.current.as_ref() else {
            return Err(runtime_error(
                ErrorCode::InvalidState,
                "Native continuation is unavailable in interpreter mode",
            ));
        };
        let mut frame = CpsFrame::new(info.frame_slots);
        for (slot, arg) in frame.slots.iter_mut().zip(args) {
            *slot = arg.pack_scalar().expect("integer entry arguments");
        }
        let mut task = unsafe {
            Continuation::new(
                crate::runtime::abi::as_cps_entry(function.entry),
                frame,
                module.version,
                module.clone(),
            )
        };
        task = task.with_sources(module.sources.clone());
        task.resume()?;
        Ok(task)
    }

    fn call_interpreter_values(
        &self,
        name: &str,
        args: &[interp::Value],
        params: &[Type],
        ret: &Type,
    ) -> Result<interp::Value, Error> {
        let Some(LoadedModule::Interpreter(module)) = self.current.as_ref() else {
            return Err(runtime_error(ErrorCode::InvalidState, "No module loaded"));
        };
        let info = interp::function_info(module, name).ok_or_else(|| {
            runtime_error(ErrorCode::NotFound, format!("Function `{name}` not found"))
        })?;
        if info.signature.has_text() {
            return Err(abi_error(
                ErrorCode::AbiError,
                "Owned text entries require typed String root APIs",
            ));
        }
        if info.suspends {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Function `{name}` requires call_async"),
            ));
        }
        if info.signature.params != params || &info.signature.ret != ret {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Call signature mismatch for `{name}`"),
            ));
        }
        interp::call_sync(module, name, args, self.interpreter_limits)
    }

    fn call_interpreter_sync(
        &self,
        name: &str,
        args: &[i32],
        ret: &Type,
    ) -> Result<interp::Value, Error> {
        let values = args
            .iter()
            .copied()
            .map(interp::Value::Int32)
            .collect::<Vec<_>>();
        self.call_interpreter_values(name, &values, &vec![Type::Int32; args.len()], ret)
    }

    fn call_interpreter_unit(&self, name: &str, args: &[i32]) -> Result<(), Error> {
        match self.call_interpreter_sync(name, args, &Type::Unit)? {
            interp::Value::Unit => Ok(()),
            _ => Err(runtime_error(
                ErrorCode::InvalidState,
                format!("Function `{name}` did not return Unit"),
            )),
        }
    }
}
impl Default for VmEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn runtime_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, code, message)
}

fn abi_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, code, message)
}

impl LoadedModule {
    fn check_script_contract(&self, name: &str, input: &str, output: &str) -> Result<(), Error> {
        let info = match self {
            Self::Jit(module) => module.request_entries.get(name).map(|f| &f.info),
            Self::Interpreter(module) => interp::script_info(module, name),
        };
        if info.is_some_and(|info| {
            info.suspends
                && info.domain_request_entry
                && info.signature.params.len() == 1
                && crate::core::script::domain_schema(&info.signature.params[0]).as_deref()
                    == Some(input)
                && crate::core::script::domain_schema(&info.signature.ret).as_deref()
                    == Some(output)
        }) {
            Ok(())
        } else {
            Err(runtime_error(
                ErrorCode::TypeMismatch,
                format!("Required Script entry `{name}` is missing or incompatible"),
            ))
        }
    }
    pub(super) fn prepare_script_call(
        &self,
        name: &str,
        input_schema: &str,
        result_schema: &str,
        bits: &[u64],
        limits: InterpreterLimits,
    ) -> Result<(Continuation, Option<(usize, usize)>), Error> {
        let module = self;
        let info = match module {
            LoadedModule::Jit(module) => module
                .request_entries
                .get(name)
                .or_else(|| module.exports.get(name))
                .filter(|f| !f.info.internal || f.info.script_entry)
                .map(|f| &f.info),
            LoadedModule::Interpreter(module) => interp::script_info(module, name),
        }
        .ok_or_else(|| {
            runtime_error(ErrorCode::NotFound, format!("Function `{name}` not found"))
        })?;
        if !info.suspends
            || info.signature.params.len() != 1
            || crate::core::script::domain_schema(&info.signature.params[0]).as_deref()
                != Some(input_schema)
            || crate::core::script::domain_schema(&info.signature.ret).as_deref()
                != Some(result_schema)
            || !info.domain_request_entry
        {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                "Script entry contract mismatch",
            ));
        }
        if crate::runtime::domain_packet::has_owners(&info.signature.params[0])
            || crate::runtime::domain_packet::has_owners(&info.signature.ret)
        {
            return self.prepare_domain_call(name, bits, limits);
        }
        if !crate::core::script::valid_words(&info.signature.params[0], bits) {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                "Invalid Script input value",
            ));
        }
        let layout = (info.signature.ret.is_aggregate() || info.signature.ret == Type::Unit)
            .then_some((
                info.aggregate_result_offset(),
                info.signature.ret.scalar_types().len(),
            ));
        let task = match module {
            LoadedModule::Interpreter(module) => Continuation::new_interpreted(
                interp::prepare_script(module.clone(), name, bits, limits)?,
                module.version,
                module.clone(),
            ),
            LoadedModule::Jit(module) => {
                let function = &module.request_entries[name];
                let mut frame = CpsFrame::new(info.frame_slots);
                frame.slots[..bits.len()].copy_from_slice(bits);
                unsafe {
                    Continuation::new(
                        crate::runtime::abi::as_cps_entry(function.entry),
                        frame,
                        module.version,
                        module.clone(),
                    )
                }
                .with_sources(module.sources.clone())
            }
        };
        Ok((task, layout))
    }
    pub(super) fn prepare_domain_call(
        &self,
        name: &str,
        bits: &[u64],
        limits: InterpreterLimits,
    ) -> Result<(Continuation, Option<(usize, usize)>), Error> {
        let info = match self {
            LoadedModule::Jit(module) => module
                .request_entries
                .get(name)
                .or_else(|| module.exports.get(name))
                .map(|f| &f.info),
            LoadedModule::Interpreter(module) => interp::request_info(module, name),
        }
        .ok_or_else(|| runtime_error(ErrorCode::NotFound, "Unknown domain request"))?;
        if !info.domain_request_entry
            || !info.suspends
            || info.signature.params.len() != 1
            || crate::core::script::domain_schema(&info.signature.params[0]).is_none()
            || crate::core::script::domain_schema(&info.signature.ret).is_none()
        {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                "Invalid domain request contract",
            ));
        }
        crate::runtime::domain_packet::validate(&info.signature.params, bits)?;
        let layout = (info.signature.ret.is_aggregate() || info.signature.ret == Type::Unit)
            .then_some((
                info.aggregate_result_offset(),
                info.signature.ret.scalar_types().len(),
            ));
        let task = match self {
            LoadedModule::Interpreter(module) => Continuation::new_interpreted(
                interp::prepare_domain(module.clone(), name, bits, limits)?,
                module.version,
                module.clone(),
            ),
            LoadedModule::Jit(module) => {
                let mut frame = CpsFrame::new(info.frame_slots);
                let input =
                    crate::runtime::domain_packet::decode(&info.signature.params, bits, unsafe {
                        &mut *crate::runtime::cps::arena(&mut frame)
                    })?;
                frame.slots[..input.len()].copy_from_slice(&input);
                if let Some((key, entry)) =
                    crate::runtime::domain_packet::input_cleanup(&info.signature.params[0], &input)
                {
                    unsafe {
                        (*crate::runtime::cps::cleanup(&mut frame))
                            .entries
                            .insert(key, entry);
                    }
                }
                if crate::runtime::domain_packet::has_owners(&info.signature.ret) {
                    frame.domain_return =
                        Some((info.signature.ret.clone(), info.aggregate_result_offset()));
                }
                unsafe {
                    Continuation::new(
                        crate::runtime::abi::as_cps_entry(module.request_entries[name].entry),
                        frame,
                        module.version,
                        module.clone(),
                    )
                }
                .with_sources(module.sources.clone())
            }
        };
        Ok((task, layout))
    }
    pub(super) fn prepare_async(
        &self,
        name: &str,
        args: &[super::InstanceValue],
        limits: InterpreterLimits,
    ) -> Result<Continuation, Error> {
        match self {
            Self::Interpreter(module) => {
                let task = interp::prepare_scalar(module.clone(), name, args, limits)?;
                Ok(Continuation::new_interpreted(
                    task,
                    module.version,
                    module.clone(),
                ))
            }
            Self::Jit(module) => {
                let function = module.exports.get(name).ok_or_else(|| {
                    runtime_error(ErrorCode::NotFound, format!("Function `{name}` not found"))
                })?;
                let info = &function.info;
                if !info.suspends
                    || !crate::core::instance::scalar(&info.signature.ret)
                    || info.signature.params
                        != args.iter().map(|v| v.value_type()).collect::<Vec<_>>()
                {
                    return Err(runtime_error(
                        ErrorCode::InvalidCall,
                        "Async scalar call signature mismatch",
                    ));
                }
                let mut frame = CpsFrame::new(info.frame_slots);
                for (slot, value) in frame.slots.iter_mut().zip(args) {
                    *slot = value.bits();
                }
                // The compiler supplies the complete frame and exact CPS entry;
                // the immutable module owns every reachable code dependency.
                Ok(unsafe {
                    Continuation::new(
                        crate::runtime::abi::as_cps_entry(function.entry),
                        frame,
                        module.version,
                        module.clone(),
                    )
                }
                .with_sources(module.sources.clone()))
            }
        }
    }
}
