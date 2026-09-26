//! Embedding API with immutable compiled exports and checked host declarations.
use crate::{
    jit::{self, CompiledFunction, CompiledModule},
    parser::{self, ast::Item},
    runtime::{
        cps::{CpsEntry, CpsFrame},
        suspension::{Continuation, ModuleId},
    },
    FunctionSignature, Type,
};
use std::{any::Any, collections::HashMap, rc::Rc};

enum HostEntry {
    Direct(*const u8),
    Cps {
        entry: CpsEntry,
        frame_slots: Option<usize>,
    },
}
struct HostFunction {
    signature: FunctionSignature,
    entry: HostEntry,
    owner: Option<Rc<dyn Any>>,
}

pub struct VmEngine {
    current: Option<Rc<CompiledModule>>,
    hosts: HashMap<String, HostFunction>,
    next_version: u64,
}

impl VmEngine {
    pub fn new() -> Self {
        Self {
            current: None,
            hosts: HashMap::new(),
            next_version: 1,
        }
    }

    /// Register a direct host function. Replaces any registration with this name.
    /// Existing compiled modules retain their previously resolved functions.
    ///
    /// # Safety
    /// `ptr` must be a live C ABI function with exactly `signature`. It must remain
    /// callable for every compiled module using it, and must not unwind into JIT code.
    pub unsafe fn register_fn(&mut self, name: &str, signature: FunctionSignature, ptr: *const u8) {
        self.hosts.insert(
            name.into(),
            HostFunction {
                signature,
                entry: HostEntry::Direct(ptr),
                owner: None,
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
            HostFunction {
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
            HostFunction {
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
        let mut symbols = Vec::new();
        let mut layouts = HashMap::new();
        let mut dependencies = Vec::new();
        for item in &ast.items {
            let Item::ExternFn(f) = item else { continue };
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
            let ptr = match host.entry {
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
            symbols.push((f.name.as_str(), ptr));
            if let Some(owner) = &host.owner {
                dependencies.push(owner.clone());
            }
        }
        let next = self
            .next_version
            .checked_add(1)
            .ok_or("Module version exhausted")?;
        let compiled = jit::compile(
            &ast,
            &symbols,
            &layouts,
            dependencies,
            ModuleId(self.next_version),
        )?;
        let mut names: Vec<_> = compiled.exports.keys().cloned().collect();
        names.sort();
        self.current = Some(Rc::new(compiled));
        self.next_version = next;
        Ok(names)
    }

    /// An owned snapshot; modifying it cannot change a compiled entry's ABI.
    pub fn function_signature(&self, name: &str) -> Option<FunctionSignature> {
        self.current
            .as_ref()?
            .exports
            .get(name)
            .map(|f| f.info.signature.clone())
    }

    pub fn module_version(&self) -> Option<ModuleId> {
        self.current.as_ref().map(|module| module.version)
    }

    fn function(&self, name: &str) -> Result<&CompiledFunction, String> {
        self.current
            .as_ref()
            .ok_or("No module loaded")?
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
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Int32)?;
        let f: extern "C" fn(i32, i32) -> i32 = unsafe { std::mem::transmute(ptr) };
        Ok(f(a, b))
    }
    pub fn call_int32_1(&self, name: &str, a: i32) -> Result<i32, String> {
        let ptr = self.sync_ptr(name, &[Type::Int32], Type::Int32)?;
        let f: extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(ptr) };
        Ok(f(a))
    }
    pub fn call_int32_0(&self, name: &str) -> Result<i32, String> {
        let ptr = self.sync_ptr(name, &[], Type::Int32)?;
        let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(ptr) };
        Ok(f())
    }
    pub fn call_unit_2(&self, name: &str, a: i32, b: i32) -> Result<(), String> {
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Unit)?;
        let f: extern "C" fn(i32, i32) = unsafe { std::mem::transmute(ptr) };
        f(a, b);
        Ok(())
    }

    /// Run to the first pending poll or completion. Int32 arguments and an
    /// Int32/Unit result are supported by this convenience entry point.
    pub fn call_async(&self, name: &str, args: &[i32]) -> Result<Continuation, String> {
        let function = self.function(name)?;
        let info = &function.info;
        if !info.suspends {
            return Err(format!("Function `{name}` is not marked @suspends"));
        }
        if info.signature.params.len() != args.len()
            || info.signature.params.iter().any(|t| *t != Type::Int32)
            || !matches!(info.signature.ret, Type::Int32 | Type::Unit)
        {
            return Err(format!("Async call signature mismatch for `{name}`"));
        }
        let module = self.current.as_ref().unwrap();
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

    pub fn reload_source(&mut self, source: &str) -> Result<Vec<String>, String> {
        self.load_source(source)
    }
}
impl Default for VmEngine {
    fn default() -> Self {
        Self::new()
    }
}
