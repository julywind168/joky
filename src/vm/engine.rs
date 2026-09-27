//! Embedding API with immutable compiled exports and checked host declarations.
use super::InterpreterLimits;
use crate::{
    backends::{
        interp,
        jit::{self, CompiledFunction, CompiledModule},
    },
    modules::ModuleRegistry,
    parser::{self, ast::Item},
    runtime::{
        bindings::{HostBinding, HostEntry},
        cps::{CpsEntry, CpsFrame},
        suspension::{Continuation, ModuleId, ResumeError},
    },
    Error, ErrorCode, ErrorPhase, FunctionSignature, Type, TypeRegistry,
};
use std::{any::Any, collections::HashMap, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmMode {
    Jit,
    Interpreter,
}

enum LoadedModule {
    Jit(Rc<CompiledModule>),
    Interpreter(Rc<interp::BytecodeModule>),
}

pub struct VmEngine {
    mode: VmMode,
    current: Option<LoadedModule>,
    hosts: HashMap<String, HostBinding>,
    types: TypeRegistry,
    next_version: u64,
    registry: Rc<ModuleRegistry>,
    interpreter_limits: InterpreterLimits,
}

impl VmEngine {
    pub fn new() -> Self {
        Self::with_mode(VmMode::Jit)
    }

    pub fn with_mode(mode: VmMode) -> Self {
        Self::with_mode_and_registry(mode, crate::stdlib::module_registry())
    }

    /// Inject one immutable binding snapshot for all loads and both VM backends.
    pub(crate) fn with_mode_and_registry(mode: VmMode, registry: Rc<ModuleRegistry>) -> Self {
        Self {
            mode,
            current: None,
            hosts: HashMap::new(),
            types: registry.types().clone(),
            next_version: 1,
            registry,
            interpreter_limits: InterpreterLimits::default(),
        }
    }

    pub fn new_interpreter() -> Self {
        Self::with_mode(VmMode::Interpreter)
    }

    pub fn mode(&self) -> VmMode {
        self.mode
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

    /// Register a direct host function. Replaces any registration with this name.
    /// Existing compiled modules retain their previously resolved functions.
    ///
    /// # Safety
    /// `ptr` must be a live C ABI function with exactly `signature`. It must remain
    /// callable for every compiled module using it, and must not unwind into JIT code.
    /// String arguments/results use the borrowed ABI in runtime::string. Returned
    /// views must remain valid for all generated callers that can retain them.
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
                signature,
                entry: HostEntry::Direct(ptr),
                owner,
            },
        );
        self.types = types;
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

    /// Parse, compile and publish a replacement only after all checks succeed.
    pub fn load_source(&mut self, source: &str) -> Result<Vec<String>, Error> {
        let ast = parser::parse(source)?;
        let ast = crate::core::resolver::resolve(&ast, self.registry.metadata())?;
        self.registry.metadata().validate_declarations(&ast)?;
        let mut symbols = Vec::new();
        let mut layouts = HashMap::new();
        let mut dependencies = Vec::new();
        let mut interpreter_hosts = HashMap::new();
        for item in &ast.items {
            let Item::ExternFn(f) = item else { continue };
            let (symbol, host) = if let Some(binding) = self.registry.binding(&f.name) {
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
            if signature != host.signature
                || f.suspends != matches!(host.entry, HostEntry::Cps { .. })
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
            let entry_ptr = match host.entry {
                HostEntry::Direct(ptr) => {
                    if ptr.is_null() {
                        return Err(abi_error(
                            ErrorCode::AbiError,
                            format!("Host function `{}` has a null entry", f.name),
                        )
                        .with_span(f.span));
                    }
                    ptr
                }
                HostEntry::Cps { entry, frame_slots } => {
                    if let Some(count) = frame_slots {
                        layouts.insert(f.name.clone(), count);
                    }
                    entry as *const u8
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
        let next = self
            .next_version
            .checked_add(1)
            .ok_or_else(|| runtime_error(ErrorCode::InvalidState, "Module version exhausted"))?;
        let version = ModuleId(self.next_version);
        let loaded = match self.mode {
            VmMode::Jit => {
                let compiled = jit::compile_with_registry(
                    &ast,
                    &symbols,
                    &layouts,
                    dependencies,
                    version,
                    self.registry.metadata(),
                    &self.types,
                )?;
                let compiled = Rc::new(compiled);
                crate::stdlib::track_module(&compiled);
                LoadedModule::Jit(compiled)
            }
            VmMode::Interpreter => LoadedModule::Interpreter(Rc::new(interp::compile_with_types(
                &ast,
                &layouts,
                interpreter_hosts,
                version,
                &self.types,
            )?)),
        };
        let mut names: Vec<_> = match &loaded {
            LoadedModule::Jit(module) => module.exports.keys().cloned().collect(),
            LoadedModule::Interpreter(module) => interp::function_names(module),
        };
        names.sort();
        self.current = Some(loaded);
        self.next_version = next;
        Ok(names)
    }

    fn validate_registration(
        &self,
        name: &str,
        signature: &FunctionSignature,
        null_entry: bool,
        frame_slots: Option<usize>,
    ) -> Result<TypeRegistry, Error> {
        if name.is_empty() || name.contains('\0') {
            return Err(abi_error(
                ErrorCode::AbiError,
                format!("Invalid host function name `{name}`"),
            ));
        }
        if self.registry.binding(name).is_some() {
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
        if frame_slots
            .is_some_and(|slots| slots < signature.params.len() || slots >= i32::MAX as usize / 8)
        {
            return Err(abi_error(
                ErrorCode::AbiError,
                "Invalid CPS frame size for registered host function",
            ));
        }
        let mut types = self.types.clone();
        for ty in signature
            .params
            .iter()
            .chain(std::iter::once(&signature.ret))
        {
            if let Type::Named(name) = ty {
                if !types.contains(name) {
                    types.register_opaque(name.clone())?;
                }
            }
        }
        Ok(types)
    }

    /// An owned snapshot; modifying it cannot change a compiled entry's ABI.
    pub fn function_signature(&self, name: &str) -> Option<FunctionSignature> {
        match self.current.as_ref()? {
            LoadedModule::Jit(module) => module.exports.get(name).map(|f| f.info.signature.clone()),
            LoadedModule::Interpreter(module) => interp::function_signature(module, name).cloned(),
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

    pub fn call_int32_2(&self, name: &str, a: i32, b: i32) -> Result<i32, Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_int32(name, &[a, b]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Int32)?;
        Ok(unsafe { crate::runtime::abi::call_i32_2(ptr, a, b) })
    }
    pub fn call_int32_1(&self, name: &str, a: i32) -> Result<i32, Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_int32(name, &[a]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32], Type::Int32)?;
        Ok(unsafe { crate::runtime::abi::call_i32_1(ptr, a) })
    }
    pub fn call_int32_0(&self, name: &str) -> Result<i32, Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_int32(name, &[]);
        }
        let ptr = self.sync_ptr(name, &[], Type::Int32)?;
        Ok(unsafe { crate::runtime::abi::call_i32_0(ptr) })
    }
    pub fn call_unit_0(&self, name: &str) -> Result<(), Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[]);
        }
        let ptr = self.sync_ptr(name, &[], Type::Unit)?;
        unsafe { crate::runtime::abi::call_unit_0(ptr) };
        Ok(())
    }
    pub fn call_unit_1(&self, name: &str, a: i32) -> Result<(), Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[a]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32], Type::Unit)?;
        unsafe { crate::runtime::abi::call_unit_1(ptr, a) };
        Ok(())
    }
    pub fn call_unit_2(&self, name: &str, a: i32, b: i32) -> Result<(), Error> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[a, b]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Unit)?;
        unsafe { crate::runtime::abi::call_unit_2(ptr, a, b) };
        Ok(())
    }

    /// Run to the first pending poll or completion. Int32 arguments and an
    /// Int32/Unit result are supported by this convenience entry point.
    pub fn call_async(&self, name: &str, args: &[i32]) -> Result<Continuation, Error> {
        if self.mode == VmMode::Interpreter {
            let Some(LoadedModule::Interpreter(module)) = self.current.as_ref() else {
                return Err(runtime_error(ErrorCode::InvalidState, "No module loaded"));
            };
            let task = interp::start(module.clone(), name, args, self.interpreter_limits)?;
            let mut task = Continuation::new_interpreted(task, module.version, module.clone());
            task.resume().map_err(cps_start_error)?;
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
        if !info.signature.accepts_async_call(args.len()) {
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
            *slot = *arg as u32 as u64;
        }
        let mut task = unsafe {
            Continuation::new(
                crate::runtime::abi::as_cps_entry(function.entry),
                frame,
                module.version,
                module.clone(),
            )
        };
        task.resume().map_err(cps_start_error)?;
        Ok(task)
    }

    fn call_interpreter_sync(
        &self,
        name: &str,
        args: &[i32],
        ret: &Type,
    ) -> Result<interp::Value, Error> {
        let Some(LoadedModule::Interpreter(module)) = self.current.as_ref() else {
            return Err(runtime_error(ErrorCode::InvalidState, "No module loaded"));
        };
        let info = interp::function_info(module, name).ok_or_else(|| {
            runtime_error(ErrorCode::NotFound, format!("Function `{name}` not found"))
        })?;
        if info.suspends {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Function `{name}` requires call_async"),
            ));
        }
        if !info.signature.accepts_int32_args(args.len()) || &info.signature.ret != ret {
            return Err(runtime_error(
                ErrorCode::InvalidCall,
                format!("Call signature mismatch for `{name}`"),
            ));
        }
        interp::call_sync(
            module,
            name,
            &args
                .iter()
                .copied()
                .map(interp::Value::Int32)
                .collect::<Vec<_>>(),
            self.interpreter_limits,
        )
    }

    fn call_interpreter_int32(&self, name: &str, args: &[i32]) -> Result<i32, Error> {
        match self.call_interpreter_sync(name, args, &Type::Int32)? {
            interp::Value::Int32(value) => Ok(value),
            _ => Err(runtime_error(
                ErrorCode::InvalidState,
                format!("Function `{name}` did not return Int32"),
            )),
        }
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

/// Normalize a failed first poll of `call_async` into a structured error.
/// Host failure statuses surface as `HostError` on both backends; the native
/// path cannot name the failing binding because the C entry returns raw
/// statuses only.
fn cps_start_error(error: ResumeError) -> Error {
    match error {
        ResumeError::Failed(error) => error,
        ResumeError::InvalidCpsStatus(value) => runtime_error(
            ErrorCode::HostError,
            format!("Suspending host returned failure status {value}"),
        ),
        other => runtime_error(
            ErrorCode::RuntimeError,
            format!("CPS start failed: {other:?}"),
        ),
    }
}

fn abi_error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, code, message)
}
