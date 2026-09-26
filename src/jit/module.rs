/// 版本化入口表（阶段4实现）
///
/// 阶段1占位：定义数据结构，不实现调用逻辑。
use std::collections::HashMap;

/// 模块版本号
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleVersion(pub u64);

pub struct Entry {
    pub fn_ptr: *const u8,
    pub version: ModuleVersion,
}

// 含裸指针，单线程使用
unsafe impl Send for Entry {}
unsafe impl Sync for Entry {}

pub struct ModuleEntryTable {
    entries: HashMap<String, Entry>,
    next_version: u64,
}

impl ModuleEntryTable {
    pub fn new() -> Self {
        ModuleEntryTable {
            entries: HashMap::new(),
            next_version: 1,
        }
    }

    pub fn next_version_id(&mut self) -> ModuleVersion {
        let v = ModuleVersion(self.next_version);
        self.next_version += 1;
        v
    }

    pub fn insert(&mut self, name: impl Into<String>, ptr: *const u8, version: ModuleVersion) {
        self.entries.insert(name.into(), Entry { fn_ptr: ptr, version });
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.get(name)
    }

    pub fn reload(&mut self, name: &str, new_ptr: *const u8, new_version: ModuleVersion) {
        self.entries.insert(name.to_string(), Entry { fn_ptr: new_ptr, version: new_version });
    }
}

impl Default for ModuleEntryTable {
    fn default() -> Self {
        Self::new()
    }
}
