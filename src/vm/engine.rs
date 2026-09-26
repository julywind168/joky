//! VM engine with versioned entries and native host registration.
use crate::jit::compiler::JitCompiler;
use crate::jit::module::ModuleEntryTable;
use crate::parser;
use crate::parser::ast::{FnDef, Item, Module, Type};
use crate::runtime::{
    cps::{CpsEntry, CpsFrame},
    suspension::{Continuation, ModuleId},
};
use std::rc::Rc;

/// Registered host function and ABI metadata.
pub struct HostFn {
    pub name: String,
    pub ptr: *const u8,
    suspends: bool,
    frame_slots: Option<usize>,
    owner: Option<Rc<dyn std::any::Any>>,
}

pub struct VmEngine {
    pub module: Option<Module>,
    compiler: Option<Rc<JitCompiler>>,
    entry_table: ModuleEntryTable,
    host_fns: Vec<HostFn>,
}

impl VmEngine {
    pub fn new() -> Self {
        VmEngine {
            module: None,
            compiler: None,
            entry_table: ModuleEntryTable::new(),
            host_fns: Vec::new(),
        }
    }

    /// Register a C ABI host function before loading source.
    pub fn register_fn(&mut self, name: &str, ptr: *const u8) {
        self.host_fns.push(HostFn {
            name: name.to_string(),
            ptr,
            suspends: false,
            frame_slots: None,
            owner: None,
        });
    }

    /// Register a nonblocking host callback using the CPS frame ABI.
    /// # Safety
    /// The callback must match the declared types, must not retain frame pointers,
    /// panic, or change slot storage. Save progress in the frame before returning 1.
    pub unsafe fn register_suspending_fn(&mut self, name: &str, entry: CpsEntry) {
        self.host_fns.push(HostFn {
            name: name.into(),
            ptr: entry as *const u8,
            suspends: true,
            frame_slots: None,
            owner: None,
        });
    }

    /// Register generated AOT code with its complete frame layout and owner.
    ///
    /// # Safety
    /// `entry` must match the source declaration and runtime CPS ABI.
    /// `frame_slots` must come from AotCompiler::cps_frame_slots for this entry.
    /// `owner` must keep the entry and every native dependency alive.
    pub unsafe fn register_aot_suspending_fn(
        &mut self,
        name: &str,
        entry: CpsEntry,
        frame_slots: usize,
        owner: Rc<dyn std::any::Any>,
    ) {
        self.host_fns.push(HostFn {
            name: name.into(),
            ptr: entry as *const u8,
            suspends: true,
            frame_slots: Some(frame_slots),
            owner: Some(owner),
        });
    }

    /// Load and JIT-compile source.
    ///
    /// Registered host symbols resolve the module's extern declarations.
    pub fn load_source(&mut self, source: &str) -> Result<Vec<String>, String> {
        let module = parser::parse(source)?;
        for item in &module.items {
            if let Item::ExternFn(f) = item {
                let host = self
                    .host_fns
                    .iter()
                    .rev()
                    .find(|h| h.name == f.name)
                    .ok_or_else(|| format!("Host function `{}` is not registered", f.name))?;
                if host.suspends != f.suspends {
                    return Err(format!(
                        "Host function `{}` has an incompatible @suspends ABI",
                        f.name
                    ));
                }
            }
        }

        // Pass registered symbols to the JIT compiler.
        let symbols: Vec<(&str, *const u8)> = self
            .host_fns
            .iter()
            .map(|h| (h.name.as_str(), h.ptr))
            .collect();

        let mut compiler = JitCompiler::new(&symbols)?;
        let mut layouts = std::collections::HashMap::new();
        for item in &module.items {
            if let Item::ExternFn(f) = item {
                let host = self
                    .host_fns
                    .iter()
                    .rev()
                    .find(|h| h.name == f.name)
                    .unwrap();
                if let Some(slots) = host.frame_slots {
                    layouts.insert(f.name.clone(), slots);
                }
                if let Some(owner) = &host.owner {
                    compiler.retain_code_owner(owner.clone());
                }
            }
        }
        let compiled = compiler.compile_with_import_layouts(&module, &layouts)?;

        // Publish compiled functions in the versioned entry table.
        let version = self.entry_table.next_version_id();
        self.entry_table.clear();
        for name in &compiled {
            if let Some(ptr) = compiler.get_fn_ptr(name) {
                self.entry_table.insert(name.clone(), ptr, version);
            }
        }

        self.compiler = Some(Rc::new(compiler));
        self.module = Some(module);
        Ok(compiled)
    }

    // ── Invocation API ──────────────────────────────────────────────────────────

    /// `fn name(a: Int32, b: Int32) -> Int32`
    pub fn call_int32_2(&self, name: &str, a: i32, b: i32) -> Result<i32, String> {
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Int32)?;
        Ok(unsafe {
            let f: extern "C" fn(i32, i32) -> i32 = std::mem::transmute(ptr);
            f(a, b)
        })
    }

    /// `fn name(a: Int32) -> Int32`
    pub fn call_int32_1(&self, name: &str, a: i32) -> Result<i32, String> {
        let ptr = self.sync_ptr(name, &[Type::Int32], Type::Int32)?;
        Ok(unsafe {
            let f: extern "C" fn(i32) -> i32 = std::mem::transmute(ptr);
            f(a)
        })
    }

    /// `fn name() -> Int32`
    pub fn call_int32_0(&self, name: &str) -> Result<i32, String> {
        let ptr = self.sync_ptr(name, &[], Type::Int32)?;
        Ok(unsafe {
            let f: extern "C" fn() -> i32 = std::mem::transmute(ptr);
            f()
        })
    }

    /// `fn name(a: Int32, b: Int32)` — no return value
    pub fn call_unit_2(&self, name: &str, a: i32, b: i32) -> Result<(), String> {
        let ptr = self.sync_ptr(name, &[Type::Int32, Type::Int32], Type::Unit)?;
        unsafe {
            let f: extern "C" fn(i32, i32) = std::mem::transmute(ptr);
            f(a, b);
        }
        Ok(())
    }

    fn definition(&self, name: &str) -> Result<&FnDef, String> {
        self.module
            .as_ref()
            .ok_or("No module loaded")?
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(f) if f.name == name => Some(f),
                _ => None,
            })
            .ok_or_else(|| format!("Function `{name}` not found"))
    }

    fn sync_ptr(&self, name: &str, params: &[Type], ret: Type) -> Result<*const u8, String> {
        let f = self.definition(name)?;
        if f.suspends {
            return Err(format!("Function `{name}` requires call_async"));
        }
        if f.params.iter().map(|p| &p.ty).ne(params.iter()) || f.ret_ty != ret {
            return Err(format!("Call signature mismatch for `{name}`"));
        }
        self.get_ptr(name)
    }

    /// Start a suspending function with Int32 arguments and an Int32/Unit result.
    /// Run until pending or complete; pass the continuation to a Scheduler.
    pub fn call_async(&self, name: &str, args: &[i32]) -> Result<Continuation, String> {
        let f = self.definition(name)?;
        if !f.suspends {
            return Err(format!("Function `{name}` is not marked @suspends"));
        }
        if f.params.len() != args.len()
            || f.params.iter().any(|p| p.ty != Type::Int32)
            || !matches!(f.ret_ty, Type::Int32 | Type::Unit)
        {
            return Err(format!("Async call signature mismatch for `{name}`"));
        }
        let compiler = self.compiler.as_ref().ok_or("No module loaded")?;
        let layout = compiler
            .cps_layout(name)
            .ok_or("Missing CPS frame layout")?;
        let entry = self.entry_table.get(name).ok_or("Missing CPS entry")?;
        let mut frame = CpsFrame::new(layout.slot_count);
        for (slot, arg) in frame.slots.iter_mut().zip(args) {
            *slot = *arg as u32 as u64;
        }
        let mut continuation = unsafe {
            Continuation::new_jit(
                entry.fn_ptr,
                frame,
                ModuleId(entry.version.0),
                compiler.clone(),
            )
        };
        continuation
            .resume()
            .map_err(|e| format!("CPS start failed: {e:?}"))?;
        Ok(continuation)
    }

    fn get_ptr(&self, name: &str) -> Result<*const u8, String> {
        // Prefer the latest version in the entry table.
        if let Some(entry) = self.entry_table.get(name) {
            return Ok(entry.fn_ptr);
        }
        // Fall back to the compiler entry.
        self.compiler
            .as_ref()
            .ok_or("No module loaded")?
            .get_fn_ptr(name)
            .ok_or_else(|| format!("Function `{}` not found", name))
    }

    /// Compile a new module and atomically replace its entries on success.
    pub fn reload_source(&mut self, source: &str) -> Result<Vec<String>, String> {
        self.load_source(source)
    }
}

impl Default for VmEngine {
    fn default() -> Self {
        Self::new()
    }
}
