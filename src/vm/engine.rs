//! Embedding API with immutable compiled exports and checked host declarations.
use crate::{
    interp::{self, HostBinding, HostEntry},
    jit::{self, CompiledFunction, CompiledModule},
    parser::{self, ast::Item},
    runtime::{
        cps::{CpsEntry, CpsFrame},
        suspension::{Continuation, ModuleId},
    },
    FunctionSignature, Type,
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
    next_version: u64,
}

impl VmEngine {
    pub fn new() -> Self {
        Self::with_mode(VmMode::Jit)
    }

    pub fn with_mode(mode: VmMode) -> Self {
        Self {
            mode,
            current: None,
            hosts: HashMap::new(),
            next_version: 1,
        }
    }

    pub fn new_interpreter() -> Self {
        Self::with_mode(VmMode::Interpreter)
    }

    pub fn mode(&self) -> VmMode {
        self.mode
    }

    /// Register a direct host function. Replaces any registration with this name.
    /// Existing compiled modules retain their previously resolved functions.
    ///
    /// # Safety
    /// `ptr` must be a live C ABI function with exactly `signature`. It must remain
    /// callable for every compiled module using it, and must not unwind into JIT code.
    /// String arguments/results use the borrowed ABI in runtime::string. Returned
    /// views must remain valid for all generated callers that can retain them.
    pub unsafe fn register_fn(&mut self, name: &str, signature: FunctionSignature, ptr: *const u8) {
        self.register_owned_fn(name, signature, ptr, None);
    }

    pub(crate) unsafe fn register_owned_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        ptr: *const u8,
        owner: Option<Rc<dyn Any>>,
    ) {
        self.hosts.insert(
            name.into(),
            HostBinding {
                signature,
                entry: HostEntry::Direct(ptr),
                owner,
            },
        );
    }

    /// Register a static native callback. Only parameter slots are allocated.
    ///
    /// # Safety
    /// The callback must match `signature`, stay alive for every caller, and obey
    /// the CPS ABI. It must not retain frame pointers, resize slots, or unwind.
    /// Save native progress in state/host_data before returning Pending.
    pub unsafe fn register_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        entry: CpsEntry,
    ) {
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
    }

    /// Register generated code with complete frame metadata and a lifetime owner.
    ///
    /// # Safety
    /// `entry` must match `signature` and the shared CPS runtime ABI. `frame_slots`
    /// must come from AotCompiler::cps_frame_slots for this exact entry. `owner`
    /// must keep the entry and all native dependencies alive while callers exist.
    pub unsafe fn register_aot_suspending_fn(
        &mut self,
        name: &str,
        signature: FunctionSignature,
        entry: CpsEntry,
        frame_slots: usize,
        owner: Rc<dyn Any>,
    ) {
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
    }

    /// Parse, compile and publish a replacement only after all checks succeed.
    pub fn load_source(&mut self, source: &str) -> Result<Vec<String>, String> {
        let ast = parser::parse(source).map_err(|e| e.to_string())?;
        let ast = crate::stdlib::resolve(&ast)?;
        let mut symbols = Vec::new();
        let mut layouts = HashMap::new();
        let mut dependencies = Vec::new();
        let mut interpreter_hosts = HashMap::new();
        for item in &ast.items {
            let Item::ExternFn(f) = item else { continue };
            if let Some(binding) = crate::stdlib::binding(&f.name) {
                symbols.push((binding.symbol, binding.entry));
                interpreter_hosts.insert(
                    f.name.clone(),
                    HostBinding {
                        signature: binding.signature,
                        entry: HostEntry::Direct(binding.entry),
                        owner: None,
                    },
                );
                continue;
            }
            let host = self
                .hosts
                .get(&f.name)
                .ok_or_else(|| format!("Host function `{}` is not registered", f.name))?;
            let signature = FunctionSignature::new(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                f.ret_ty.clone(),
            );
            if signature != host.signature
                || f.suspends != matches!(host.entry, HostEntry::Cps { .. })
            {
                return Err(format!(
                    "Host function `{}` has an incompatible signature or @suspends ABI",
                    f.name
                ));
            }
            let entry_ptr = match host.entry {
                HostEntry::Direct(ptr) => {
                    if ptr.is_null() {
                        return Err(format!("Host function `{}` has a null entry", f.name));
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
            symbols.push((f.name.as_str(), entry_ptr));
            interpreter_hosts.insert(f.name.clone(), host.clone());
            if let Some(owner) = &host.owner {
                dependencies.push(owner.clone());
            }
        }
        let next = self
            .next_version
            .checked_add(1)
            .ok_or("Module version exhausted")?;
        let version = ModuleId(self.next_version);
        let loaded = match self.mode {
            VmMode::Jit => {
                let compiled = jit::compile(&ast, &symbols, &layouts, dependencies, version)?;
                let compiled = Rc::new(compiled);
                crate::stdlib::track_module(&compiled);
                LoadedModule::Jit(compiled)
            }
            VmMode::Interpreter => LoadedModule::Interpreter(Rc::new(interp::compile(
                &ast,
                &layouts,
                interpreter_hosts,
                version,
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

    /// An owned snapshot; modifying it cannot change a compiled entry's ABI.
    pub fn function_signature(&self, name: &str) -> Option<FunctionSignature> {
        match self.current.as_ref()? {
            LoadedModule::Jit(module) => module.exports.get(name).map(|f| f.info.signature.clone()),
            LoadedModule::Interpreter(module) => interp::function_signature(module, name),
        }
    }

    pub fn module_version(&self) -> Option<ModuleId> {
        self.current.as_ref().map(|module| match module {
            LoadedModule::Jit(module) => module.version,
            LoadedModule::Interpreter(module) => module.version,
        })
    }

    fn function(&self, name: &str) -> Result<&CompiledFunction, String> {
        let Some(LoadedModule::Jit(module)) = self.current.as_ref() else {
            return Err("Native function lookup is unavailable in interpreter mode".into());
        };
        module
            .exports
            .get(name)
            .ok_or_else(|| format!("Function `{name}` not found"))
    }

    fn sync_ptr(&self, name: &str, params: &[Type], ret: Type) -> Result<*const u8, String> {
        let function = self.function(name)?;
        if function.info.suspends {
            return Err(format!("Function `{name}` requires call_async"));
        }
        if function.info.signature.params != params || function.info.signature.ret != ret {
            return Err(format!("Call signature mismatch for `{name}`"));
        }
        Ok(function.entry)
    }

    pub fn call_int32_2(&self, name: &str, a: i32, b: i32) -> Result<i32, String> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_int32(name, &[a, b], &[Type::Int32, Type::Int32]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Int32)?;
        let f: extern "C" fn(i32, i32) -> i32 = unsafe { std::mem::transmute(ptr) };
        Ok(f(a, b))
    }
    pub fn call_int32_1(&self, name: &str, a: i32) -> Result<i32, String> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_int32(name, &[a], &[Type::Int32]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32], Type::Int32)?;
        let f: extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(ptr) };
        Ok(f(a))
    }
    pub fn call_int32_0(&self, name: &str) -> Result<i32, String> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_int32(name, &[], &[]);
        }
        let ptr = self.sync_ptr(name, &[], Type::Int32)?;
        let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(ptr) };
        Ok(f())
    }
    pub fn call_unit_0(&self, name: &str) -> Result<(), String> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[], &[]);
        }
        let ptr = self.sync_ptr(name, &[], Type::Unit)?;
        let f: extern "C" fn() = unsafe { std::mem::transmute(ptr) };
        f();
        Ok(())
    }
    pub fn call_unit_2(&self, name: &str, a: i32, b: i32) -> Result<(), String> {
        if self.mode == VmMode::Interpreter {
            return self.call_interpreter_unit(name, &[a, b], &[Type::Int32, Type::Int32]);
        }
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Unit)?;
        let f: extern "C" fn(i32, i32) = unsafe { std::mem::transmute(ptr) };
        f(a, b);
        Ok(())
    }

    /// Run to the first pending poll or completion. Int32 arguments and an
    /// Int32/Unit result are supported by this convenience entry point.
    pub fn call_async(&self, name: &str, args: &[i32]) -> Result<Continuation, String> {
        if self.mode == VmMode::Interpreter {
            let Some(LoadedModule::Interpreter(module)) = self.current.as_ref() else {
                return Err("No module loaded".into());
            };
            let task = interp::start(module.clone(), name, args)?;
            let mut task = Continuation::new_interpreted(task, module.version, module.clone());
            task.resume()
                .map_err(|e| format!("CPS start failed: {e:?}"))?;
            return Ok(task);
        }
        let function = self.function(name)?;
        let info = &function.info;
        if !info.suspends {
            return Err(format!("Function `{name}` is not marked @suspends"));
        }
        if !info.signature.accepts_async_call(args.len()) {
            return Err(format!("Async call signature mismatch for `{name}`"));
        }
        let Some(LoadedModule::Jit(module)) = self.current.as_ref() else {
            return Err("Native continuation is unavailable in interpreter mode".into());
        };
        let mut frame = CpsFrame::new(info.frame_slots);
        for (slot, arg) in frame.slots.iter_mut().zip(args) {
            *slot = *arg as u32 as u64;
        }
        let mut task = unsafe {
            Continuation::new(
                std::mem::transmute::<*const u8, CpsEntry>(function.entry),
                frame,
                module.version,
                module.clone(),
            )
        };
        task.resume()
            .map_err(|e| format!("CPS start failed: {e:?}"))?;
        Ok(task)
    }

    fn call_interpreter_sync(
        &self,
        name: &str,
        args: &[i32],
        params: &[Type],
        ret: &Type,
    ) -> Result<interp::Value, String> {
        let Some(LoadedModule::Interpreter(module)) = self.current.as_ref() else {
            return Err("No module loaded".into());
        };
        let signature = interp::function_signature(module, name)
            .ok_or_else(|| format!("Function `{name}` not found"))?;
        if signature.params != params || &signature.ret != ret {
            return Err(format!("Call signature mismatch for `{name}`"));
        }
        interp::call_sync(
            module,
            name,
            &args
                .iter()
                .copied()
                .map(interp::Value::Int32)
                .collect::<Vec<_>>(),
        )
    }

    fn call_interpreter_int32(
        &self,
        name: &str,
        args: &[i32],
        params: &[Type],
    ) -> Result<i32, String> {
        match self.call_interpreter_sync(name, args, params, &Type::Int32)? {
            interp::Value::Int32(value) => Ok(value),
            _ => Err(format!("Function `{name}` did not return Int32")),
        }
    }

    fn call_interpreter_unit(
        &self,
        name: &str,
        args: &[i32],
        params: &[Type],
    ) -> Result<(), String> {
        match self.call_interpreter_sync(name, args, params, &Type::Unit)? {
            interp::Value::Unit => Ok(()),
            _ => Err(format!("Function `{name}` did not return Unit")),
        }
    }
}
impl Default for VmEngine {
    fn default() -> Self {
        Self::new()
    }
}
