//! Frame descriptors for synchronous stack admission. Never embed compiler
//! addresses: descriptors and helper calls use ordinary module relocations.
use super::lower;
use crate::core::ir::FunctionInfo;
use crate::Type;
use cranelift_codegen::{
    ir::{types, InstBuilder, MemFlagsData},
    Context,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Module, ModuleError};
use std::collections::HashMap;

pub(super) const AUXILIARY_FRAME: u64 = 4096;
// At most a caller FP reset bridge and the guarded target entry coexist
// between two body checks. Keep a third adapter allowance for ABI forwarding.
// Every generated adapter in these paths is audited after final compilation.
pub(super) const ENVELOPE: u64 = 3 * AUXILIARY_FRAME;

pub(super) fn enabled<M: Module>(module: &M) -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
        && module
            .isa()
            .triple()
            .to_string()
            .starts_with("aarch64-apple-darwin")
}

pub(super) struct Entry<'a> {
    pub descriptor: DataId,
    pub info: &'a FunctionInfo,
    pub places: &'a [(Type, DataId, usize)],
}

pub(super) fn frame(ctx: &Context) -> Result<u64, Box<ModuleError>> {
    let layout = ctx
        .compiled_code()
        .and_then(|code| code.buffer.frame_layout())
        .ok_or_else(|| {
            ModuleError::Backend(std::io::Error::other("Missing native stack frame layout").into())
        })?;
    // Apple AArch64 ordinary calls: spills/locals/callee-saves/outgoing area
    // below FP, plus saved FP/LR. No tail calls are emitted by this backend.
    Ok(u64::from(layout.frame_to_fp_offset) + 16)
}

pub(super) fn audit<M: Module>(module: &M, ctx: &Context) -> Result<(), Box<ModuleError>> {
    if enabled(module) && frame(ctx)? > AUXILIARY_FRAME {
        return Err(Box::new(ModuleError::Backend(
            std::io::Error::other("Native stack adapter exceeds its verified frame budget").into(),
        )));
    }
    Ok(())
}

pub(super) fn define_descriptor<M: Module>(
    module: &mut M,
    id: DataId,
    bytes: u64,
) -> Result<(), Box<ModuleError>> {
    let mut data = DataDescription::new();
    data.define(bytes.to_le_bytes().to_vec().into_boxed_slice());
    data.set_align(8);
    module.define_data(id, &data)?;
    Ok(())
}

pub(super) fn wrap<M: Module>(
    module: &mut M,
    ctx: &mut Context,
    body: FuncId,
    helpers: &HashMap<String, FuncId>,
    entry: &Entry<'_>,
) {
    let ptr = module.target_config().pointer_type();
    let refs = helpers
        .iter()
        .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, &mut ctx.func)))
        .collect::<HashMap<_, _>>();
    let body = module.declare_func_in_func(body, &mut ctx.func);
    let descriptor = module.declare_data_in_func(entry.descriptor, &mut ctx.func);
    let places = entry
        .places
        .iter()
        .map(|(ty, id, count)| {
            (
                ty.clone(),
                module.declare_data_in_func(*id, &mut ctx.func),
                *count,
            )
        })
        .collect::<Vec<_>>();
    let mut frontend = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut frontend);
    let start = b.create_block();
    b.append_block_params_for_function_params(start);
    b.switch_to_block(start);
    let args = b.block_params(start).to_vec();
    let saved = b.ins().call(refs["__joky_float_env_enter"], &[]);
    let saved = b.inst_results(saved)[0];
    let address = b.ins().symbol_value(ptr, descriptor);
    let bytes = b.ins().load(ptr, MemFlagsData::trusted(), address, 0);
    let sp = b.ins().get_stack_pointer(ptr);
    let span = entry.info.span;
    let source = b.ins().iconst(types::I64, span.source_id().raw() as i64);
    let first = b.ins().iconst(ptr, span.start() as i64);
    let last = b.ins().iconst(ptr, span.end() as i64);
    let check = b.ins().call(
        refs["__joky_native_stack_room_v1"],
        &[sp, bytes, source, first, last],
    );
    let room = b.inst_results(check)[0];
    let ready = b.create_block();
    let failed = b.create_block();
    b.ins().brif(room, ready, &[], failed, &[]);
    b.switch_to_block(failed);
    lower::reject_arguments(&mut b, entry.info, &args, &refs, &places, ptr);
    b.ins().call(refs["__joky_float_env_restore"], &[saved]);
    lower::failure_return(&mut b, &refs, None, None);
    b.switch_to_block(ready);
    let call = b.ins().call(body, &args);
    let values = b.inst_results(call).to_vec();
    b.ins().call(refs["__joky_float_env_restore"], &[saved]);
    b.ins().return_(&values);
    b.seal_all_blocks();
    b.finalize(module.target_config());
}
