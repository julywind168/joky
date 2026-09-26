/// 极简分配器（阶段1：直接委托给 Rust 全局分配器）
///
/// 阶段1不需要自定义内存策略；此模块是后续 arena/slab 分配器的占位，
/// 保留接口以便后续阶段替换实现而无需修改调用方。

/// 分配 `size` 字节，对齐为 `align`
///
/// # Safety
/// 调用方需确保 `align` 是 2 的幂次且 `size > 0`。
pub unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(size, align)
        .expect("无效的内存布局");
    std::alloc::alloc(layout)
}

/// 释放 `alloc` 返回的指针
///
/// # Safety
/// `ptr` 必须是由本模块 `alloc` 分配、尚未释放的指针，
/// `size` 和 `align` 需与分配时一致。
pub unsafe fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    let layout = std::alloc::Layout::from_size_align(size, align)
        .expect("无效的内存布局");
    std::alloc::dealloc(ptr, layout);
}
