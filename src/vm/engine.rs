/// VM 引擎（阶段4：版本化入口表 + 宿主函数注册）
use crate::jit::compiler::JitCompiler;
use crate::jit::module::{ModuleEntryTable, ModuleVersion};
use crate::parser;
use crate::parser::ast::Module;

/// 宿主函数注册项：名称 + 裸函数指针（C ABI）
pub struct HostFn {
    pub name: String,
    pub ptr: *const u8,
}

// 裸指针跨线程传递仅在单线程场景下使用（阶段4不涉及多线程）
unsafe impl Send for HostFn {}
unsafe impl Sync for HostFn {}

pub struct VmEngine {
    pub module: Option<Module>,
    compiler: Option<JitCompiler>,
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

    /// 注册宿主函数（必须在 load_source 之前调用）
    pub fn register_fn(&mut self, name: &str, ptr: *const u8) {
        self.host_fns.push(HostFn { name: name.to_string(), ptr });
    }

    /// 从源码加载并 JIT 编译模块
    ///
    /// 会把已注册的宿主函数符号注入给编译器，使 `extern fn` 能正确链接。
    pub fn load_source(&mut self, source: &str) -> Result<Vec<String>, String> {
        let module = parser::parse(source)?;
        self.module = Some(module.clone());

        // 把宿主函数符号传给 JIT 编译器
        let symbols: Vec<(&str, *const u8)> = self.host_fns
            .iter()
            .map(|h| (h.name.as_str(), h.ptr))
            .collect();

        let mut compiler = JitCompiler::new(&symbols)?;
        let compiled = compiler.compile_module(&module)?;

        // 将已编译函数注册进版本化入口表
        let version = self.entry_table.next_version_id();
        for name in &compiled {
            if let Some(ptr) = compiler.get_fn_ptr(name) {
                self.entry_table.insert(name.clone(), ptr, version);
            }
        }

        self.compiler = Some(compiler);
        Ok(compiled)
    }

    // ── 调用接口 ──────────────────────────────────────────────────────────

    /// `fn name(a: Int32, b: Int32) -> Int32`
    pub fn call_int32_2(&self, name: &str, a: i32, b: i32) -> Result<i32, String> {
        let ptr = self.get_ptr(name)?;
        Ok(unsafe {
            let f: extern "C" fn(i32, i32) -> i32 = std::mem::transmute(ptr);
            f(a, b)
        })
    }

    /// `fn name(a: Int32) -> Int32`
    pub fn call_int32_1(&self, name: &str, a: i32) -> Result<i32, String> {
        let ptr = self.get_ptr(name)?;
        Ok(unsafe {
            let f: extern "C" fn(i32) -> i32 = std::mem::transmute(ptr);
            f(a)
        })
    }

    /// `fn name() -> Int32`
    pub fn call_int32_0(&self, name: &str) -> Result<i32, String> {
        let ptr = self.get_ptr(name)?;
        Ok(unsafe {
            let f: extern "C" fn() -> i32 = std::mem::transmute(ptr);
            f()
        })
    }

    /// `fn name(a: Int32, b: Int32)` — 无返回值
    pub fn call_unit_2(&self, name: &str, a: i32, b: i32) -> Result<(), String> {
        let ptr = self.get_ptr(name)?;
        unsafe {
            let f: extern "C" fn(i32, i32) = std::mem::transmute(ptr);
            f(a, b);
        }
        Ok(())
    }

    fn get_ptr(&self, name: &str) -> Result<*const u8, String> {
        // 优先从版本化入口表取（热替换时总是最新版）
        if let Some(entry) = self.entry_table.get(name) {
            return Ok(entry.fn_ptr);
        }
        // fallback：直接从编译器
        self.compiler
            .as_ref()
            .ok_or("尚未加载模块")?
            .get_fn_ptr(name)
            .ok_or_else(|| format!("函数 `{}` 未找到", name))
    }

    /// 热替换：加载新版本模块，入口表切换到新代码（场景5用）
    pub fn reload_source(&mut self, source: &str) -> Result<Vec<String>, String> {
        let module = parser::parse(source)?;
        self.module = Some(module.clone());

        let symbols: Vec<(&str, *const u8)> = self.host_fns
            .iter()
            .map(|h| (h.name.as_str(), h.ptr))
            .collect();

        let mut compiler = JitCompiler::new(&symbols)?;
        let compiled = compiler.compile_module(&module)?;

        let version = self.entry_table.next_version_id();
        for name in &compiled {
            if let Some(ptr) = compiler.get_fn_ptr(name) {
                self.entry_table.reload(name, ptr, version);
            }
        }

        // 新编译器替换旧的（旧 JIT 内存随 compiler 一起释放）
        self.compiler = Some(compiler);
        Ok(compiled)
    }
}

impl Default for VmEngine {
    fn default() -> Self { Self::new() }
}
