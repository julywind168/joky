/// Continuation 与挂起点（阶段1：数据结构占位，挂起执行在阶段5实现）

/// 恢复入口
#[derive(Debug)]
pub enum ResumePtr {
    /// AOT 函数指针（native）
    Native(fn()),
    /// JIT 代码入口（裸指针）
    Jit(*const u8),
}

// ResumePtr 含裸指针，需手动声明 Send/Sync（阶段1仅单线程使用）
unsafe impl Send for ResumePtr {}
unsafe impl Sync for ResumePtr {}

/// 模块版本标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u64);

/// 保存挂起点所需的所有状态
pub struct Continuation {
    /// 恢复时跳转的入口
    pub resume_ptr: ResumePtr,
    /// 保存的栈帧字节（CPS lowering 后填充）
    pub frame_data: Vec<u8>,
    /// 归属模块版本（热替换时用于识别旧版 continuation）
    pub module_version: ModuleId,
}

impl Continuation {
    pub fn new_native(resume: fn(), version: ModuleId) -> Self {
        Continuation {
            resume_ptr: ResumePtr::Native(resume),
            frame_data: Vec::new(),
            module_version: version,
        }
    }
}

impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuation")
            .field("resume_ptr", &self.resume_ptr)
            .field("frame_data_len", &self.frame_data.len())
            .field("module_version", &self.module_version)
            .finish()
    }
}
