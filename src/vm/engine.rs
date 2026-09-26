/// VM 引擎（阶段2：集成 JIT 编译器）
use crate::jit::compiler::JitCompiler;
use crate::parser;
use crate::parser::ast::Module;

pub struct VmEngine {
    pub module: Option<Module>,
    compiler: Option<JitCompiler>,
}

impl VmEngine {
    pub fn new() -> Self {
        VmEngine { module: None, compiler: None }
    }

    /// 从源码加载并 JIT 编译模块
    pub fn load_source(&mut self, source: &str) -> Result<Vec<String>, String> {
        let module = parser::parse(source)?;
        self.module = Some(module.clone());

        let mut compiler = JitCompiler::new()?;
        let compiled = compiler.compile_module(&module)?;
        self.compiler = Some(compiler);
        Ok(compiled)
    }

    /// 调用已编译的 `fn name(a: Int32, b: Int32) -> Int32`
    pub fn call_int32_2(&self, name: &str, a: i32, b: i32) -> Result<i32, String> {
        let compiler = self.compiler.as_ref()
            .ok_or("尚未加载模块")?;
        let ptr = compiler.get_fn_ptr(name)
            .ok_or_else(|| format!("函数 `{}` 未找到", name))?;
        // Safety: 我们确认签名为 (i32, i32) -> i32，由 JIT 编译保证
        let result = unsafe {
            let f: extern "C" fn(i32, i32) -> i32 = std::mem::transmute(ptr);
            f(a, b)
        };
        Ok(result)
    }

    /// 调用已编译的 `fn name(a: Int32) -> Int32`
    pub fn call_int32_1(&self, name: &str, a: i32) -> Result<i32, String> {
        let compiler = self.compiler.as_ref()
            .ok_or("尚未加载模块")?;
        let ptr = compiler.get_fn_ptr(name)
            .ok_or_else(|| format!("函数 `{}` 未找到", name))?;
        let result = unsafe {
            let f: extern "C" fn(i32) -> i32 = std::mem::transmute(ptr);
            f(a)
        };
        Ok(result)
    }
}

impl Default for VmEngine {
    fn default() -> Self { Self::new() }
}
