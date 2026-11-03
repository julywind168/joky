//! Test-only direct-call experiment for the ordinary Apple AArch64 ABI.
//! Compilation and execution may run on different threads.
use std::cell::Cell;

use cranelift_module::{DataDescription, DataId, Linkage, Module, ModuleError};

pub(super) const TARGET: &str = "probe_descend";

thread_local! {
    // None for every ordinary compilation. A baseline records code size only.
    static GUARDED: Cell<Option<bool>> = const { Cell::new(None) };
    static CODE_BYTES: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn compile<T>(guarded: bool, f: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            GUARDED.set(None);
        }
    }
    assert!(GUARDED.replace(Some(guarded)).is_none());
    let _reset = Reset;
    CODE_BYTES.set(0);
    let compiled = f();
    assert!(CODE_BYTES.get() > 0, "missing final private body");
    (compiled, CODE_BYTES.get())
}

pub(super) fn descriptor<M: Module>(module: &mut M) -> Result<Option<DataId>, Box<ModuleError>> {
    if GUARDED.get() != Some(true) {
        return Ok(None);
    }
    module
        .declare_data("__joky_probe_frame_bytes", Linkage::Local, false, false)
        .map(Some)
        .map_err(Box::new)
}

pub(super) fn record_body<M: Module>(
    module: &mut M,
    ctx: &cranelift_codegen::Context,
) -> Result<(), Box<ModuleError>> {
    if GUARDED.get().is_none() {
        return Ok(());
    }
    let code = ctx.compiled_code().unwrap();
    CODE_BYTES.set(code.code_buffer().len());
    if let Some(id) = descriptor(module)? {
        let layout = code.buffer.frame_layout().unwrap();
        // Regular calls, no tail-call arguments: the Apple AArch64 FP/LR pair
        // is above the spills, explicit locals, callee saves and outgoing args.
        let bytes = u64::from(layout.frame_to_fp_offset) + 16;
        let mut desc = DataDescription::new();
        desc.define(bytes.to_le_bytes().to_vec().into_boxed_slice());
        desc.set_align(8);
        module.define_data(id, &desc)?;
    }
    Ok(())
}
