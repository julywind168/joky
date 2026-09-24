//! Cranelift object emission using the shared MIR function compiler.

use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::default_libcall_names;
use cranelift_object::{ObjectBuilder, ObjectModule};

#[cfg(test)]
use cranelift_codegen::ir::{types, AbiParam, Function, InstBuilder, Signature};
#[cfg(test)]
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
#[cfg(test)]
use cranelift_module::{Linkage, Module};

use super::cranelift::CraneliftBackend;
use crate::diagnostic::CodegenError;
use crate::mir::MirProgram;

/// Native object bytes and the continuation entry symbols used by its launcher.
pub(crate) struct EmittedObject {
    pub(crate) bytes: Vec<u8>,
    pub(crate) machine_entries: Vec<(usize, String)>,
}

/// Emit a tiny object containing `joky_aot_probe() -> i64` returning `0`.
///
/// The helper is intentionally small and side-effect free so it can establish
/// the object writer contract before the full MIR backend is generalized.
#[cfg(test)]
fn emit_probe_object() -> Result<Vec<u8>, String> {
    let module = object_module(crate::AotTarget::native(), false)?;
    let mut signature = Signature::new(module.isa().default_call_conv());
    signature.returns.push(AbiParam::new(types::I64));
    let mut function =
        Function::with_name_signature(cranelift_codegen::ir::UserFuncName::user(0, 0), signature);
    let mut context = FunctionBuilderContext::new();
    {
        let entry = function.dfg.make_block();
        let mut builder = FunctionBuilder::new(&mut function, &mut context);
        builder.switch_to_block(entry);
        builder.append_block_params_for_function_params(entry);
        let zero = builder.ins().iconst(types::I64, 0);
        builder.ins().return_(&[zero]);
        builder.seal_block(entry);
        builder.finalize(module.isa().frontend_config());
    }
    emit_function_object(module, "joky_aot_probe", function)
}

/// Construct an object backend for the host target.
pub(crate) fn object_module(
    target: crate::AotTarget,
    release: bool,
) -> Result<ObjectModule, String> {
    let isa_builder = target.isa_builder()?;
    let mut flag_builder = settings::builder();
    // Match the JIT's private aggregate-return convention. Persisted modules
    // are ABI-versioned; this convention is not exposed as a native C ABI.
    flag_builder
        .set("enable_multi_ret_implicit_sret", "true")
        .map_err(|error| error.to_string())?;
    flag_builder
        .set("use_colocated_libcalls", "false")
        .map_err(|error| error.to_string())?;
    flag_builder
        .set("is_pic", "true")
        .map_err(|error| error.to_string())?;
    flag_builder
        .set("opt_level", if release { "speed" } else { "none" })
        .map_err(|error| error.to_string())?;
    let flags = settings::Flags::new(flag_builder);
    let isa = isa_builder
        .finish(flags)
        .map_err(|error| error.to_string())?;
    if isa.frontend_config().pointer_bits() != target.pointer_width() {
        return Err(format!(
            "AOT target '{}' pointer width does not match the backend",
            target.triple()
        ));
    }
    let object_builder = ObjectBuilder::new(isa, "joky", default_libcall_names())
        .map_err(|error| error.to_string())?;
    Ok(ObjectModule::new(object_builder))
}

/// Compile a complete MIR program into a native object file.
pub(crate) fn emit_mir_object_with_entries(
    mir: &MirProgram,
    release: bool,
) -> Result<EmittedObject, CodegenError> {
    emit_mir_object_with_debug(mir, crate::AotTarget::native(), release, None)
}

pub(crate) fn emit_mir_object_with_debug(
    mir: &MirProgram,
    target: crate::AotTarget,
    release: bool,
    debug: Option<super::debug::DebugInfo>,
) -> Result<EmittedObject, CodegenError> {
    let mut backend = CraneliftBackend::new_object(target, release)?;
    backend.debug_info = debug;
    backend.compile_program(mir)?;
    let entries = backend.aot_machine_entries().to_vec();
    let object = backend
        .finish_object()
        .map_err(|message| CodegenError::RuntimeError { message })?;
    Ok(EmittedObject {
        bytes: object,
        machine_entries: entries,
    })
}

/// Add one already-lowered Cranelift function to an object and finish it.
#[cfg(test)]
fn emit_function_object(
    mut module: ObjectModule,
    name: &str,
    function: Function,
) -> Result<Vec<u8>, String> {
    let id = module
        .declare_function(name, Linkage::Export, &function.signature)
        .map_err(|error| error.to_string())?;
    let mut context = module.make_context();
    context.func = function;
    module
        .define_function(id, &mut context)
        .map_err(|error| error.to_string())?;
    module.finish().emit().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn emits_native_object_bytes() {
        let bytes = super::emit_probe_object().expect("object emission");
        assert!(bytes.len() > 64);
        assert!(bytes.starts_with(&[0x7f, b'E', b'L', b'F']) || bytes.starts_with(&[0xcf, 0xfa]));
    }
}
