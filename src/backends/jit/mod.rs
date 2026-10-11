//! JIT compilation produces an immutable, independently owned code module.
use crate::core::ir::FunctionInfo;
use crate::{
    backends::native,
    core::prepared::PreparedModule,
    runtime::{cps, suspension::ModuleId},
    Error, ErrorCode, ErrorPhase,
};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::default_libcall_names;
use std::{any::Any, collections::HashMap, rc::Rc};

pub(crate) struct CompiledFunction {
    pub abi: crate::EntryAbi,
    pub entry: *const u8,
    pub info: FunctionInfo,
}

/// Contains code and export metadata, never compilation contexts or source ASTs.
pub(crate) struct CompiledModule {
    pub version: ModuleId,
    pub sources: std::sync::Arc<crate::SourceMap>,
    pub exports: HashMap<String, CompiledFunction>,
    pub instance_entries: HashMap<String, CompiledFunction>,
    pub instance_hooks:
        HashMap<String, Vec<(crate::core::ir::DropHook, crate::runtime::cleanup::Callback)>>,
    _code: OwnedCode,
    _dependencies: Vec<Rc<dyn Any>>,
}

#[cfg(test)]
impl CompiledModule {
    /// AOT test entries use this same VM dispatcher and the same Rust runtime.
    pub(crate) fn instance_test_module(
        sources: std::sync::Arc<crate::SourceMap>,
        instance_entries: HashMap<String, CompiledFunction>,
        instance_hooks: HashMap<
            String,
            Vec<(crate::core::ir::DropHook, crate::runtime::cleanup::Callback)>,
        >,
        owner: Rc<dyn Any>,
    ) -> Self {
        Self {
            version: ModuleId(1),
            sources,
            exports: HashMap::new(),
            instance_entries,
            instance_hooks,
            _code: OwnedCode(None),
            _dependencies: vec![owner],
        }
    }
}

impl CompiledModule {
    pub(crate) fn domain_module(
        exports: HashMap<String, CompiledFunction>,
        instance_entries: HashMap<String, CompiledFunction>,
        owner: Rc<dyn Any>,
    ) -> Self {
        Self {
            version: ModuleId(0),
            sources: Default::default(),
            exports,
            instance_entries,
            instance_hooks: HashMap::new(),
            _code: OwnedCode(None),
            _dependencies: vec![owner],
        }
    }
}
struct OwnedCode(Option<JITModule>);
impl Drop for OwnedCode {
    fn drop(&mut self) {
        // The last module owner drops only after all continuations are gone.
        unsafe {
            if let Some(code) = self.0.take() {
                code.free_memory();
            }
        }
    }
}

pub(crate) fn compile(
    prepared: &PreparedModule,
    symbols: &[(&str, *const u8)],
    layouts: &HashMap<String, usize>,
    dependencies: Vec<Rc<dyn Any>>,
    version: ModuleId,
) -> Result<CompiledModule, Error> {
    let plan = prepared.plan(layouts)?;
    let mut builder = JITBuilder::new(default_libcall_names())
        .map_err(|error| compile_error(error.to_string()))?;
    for &(name, ptr) in symbols {
        builder.symbol(name, ptr);
    }
    for (name, ptr) in cps::symbols() {
        builder.symbol(name, ptr);
    }
    for (name, ptr) in crate::runtime::error::symbols() {
        builder.symbol(name, ptr);
    }
    for (name, ptr) in crate::runtime::buffer::symbols()
        .into_iter()
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain([(
            "__joky_text_ffi_call_v1",
            crate::runtime::text_ffi::call as *const u8,
        )])
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::resource::symbols())
        .chain(crate::runtime::byte_request::symbols())
        .chain([(
            "__joky_tcp_poll",
            crate::runtime::tcp::poll_native as *const u8,
        )])
        .chain([(
            "__joky_dns_poll",
            crate::runtime::dns::poll_native as *const u8,
        )])
        .chain(crate::runtime::float_env::symbols())
        .chain([(
            "__joky_domain_poll_v2",
            crate::runtime::domain::poll_native as *const u8,
        )])
    {
        builder.symbol(name, ptr);
    }
    let mut code = OwnedCode(Some(JITModule::new(builder)));
    let module = code.0.as_mut().unwrap();
    let compiled = native::compile(module, &plan, "", prepared.bindings())?;
    module
        .finalize_definitions()
        .map_err(|error| compile_error(error.to_string()))?;
    let exports = compiled
        .exports
        .into_iter()
        .map(|(name, (id, info))| {
            (
                name,
                CompiledFunction {
                    abi: if info.signature.has_text() {
                        crate::EntryAbi::OwnedTextV2
                    } else {
                        crate::EntryAbi::Scalar
                    },
                    entry: module.get_finalized_function(id),
                    info,
                },
            )
        })
        .collect();
    Ok(CompiledModule {
        instance_hooks: compiled
            .instance_hooks
            .into_iter()
            .map(|(name, hooks)| {
                (
                    name,
                    hooks
                        .into_iter()
                        .map(|(hook, id)| {
                            (hook, unsafe {
                                std::mem::transmute::<*const u8, crate::runtime::cleanup::Callback>(
                                    module.get_finalized_function(id),
                                )
                            })
                        })
                        .collect(),
                )
            })
            .collect(),
        instance_entries: compiled
            .instance_entries
            .into_iter()
            .map(|(name, (id, info))| {
                (
                    name,
                    CompiledFunction {
                        entry: module.get_finalized_function(id),
                        info,
                        abi: crate::EntryAbi::Scalar,
                    },
                )
            })
            .collect(),
        sources: prepared.ast().sources.clone(),
        version,
        exports,
        _code: code,
        _dependencies: dependencies,
    })
}

/// Test adapter for exercising compiler failures from source ASTs.
#[cfg(test)]
pub(crate) fn compile_with_registry(
    ast: &crate::parser::ast::Module,
    symbols: &[(&str, *const u8)],
    layouts: &HashMap<String, usize>,
    dependencies: Vec<Rc<dyn Any>>,
    version: ModuleId,
    bindings: &crate::core::resolver::BindingRegistry,
    types: &crate::TypeRegistry,
) -> Result<CompiledModule, Error> {
    let prepared = PreparedModule::new(ast, types, bindings)?;
    compile(&prepared, symbols, layouts, dependencies, version)
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::resolver::BindingRegistry, TypeRegistry};

    #[test]
    fn bool_values_and_shadowed_locals_survive_multiple_states() {
        use cps::{CpsEntry, CpsFrame, DONE, INVALID_STATE};
        unsafe extern "C" fn yield_once(frame: *mut CpsFrame) -> i32 {
            let frame = &mut *frame;
            if frame.state == 0 {
                frame.state = 1;
                1
            } else {
                0
            }
        }
        extern "C" fn identity(value: bool) -> bool {
            value
        }
        let ast = crate::parser::parse(
            r#"
            extern fn yield_once() @suspends
            extern fn identity(value: Bool) -> Bool
            fn f(value: Bool) @suspends -> Bool {
                let saved: Bool = identity(value)
                let local = { let saved = false yield_once() saved }
                yield_once()
                yield_once()
                saved
            }
        "#,
        )
        .unwrap();
        let module = compile_with_registry(
            &ast,
            &[
                ("yield_once", yield_once as *const u8),
                ("identity", identity as *const u8),
            ],
            &HashMap::new(),
            vec![],
            ModuleId(1),
            &BindingRegistry::default(),
            &TypeRegistry::default(),
        )
        .unwrap();
        let function = &module.exports["f"];
        let entry: CpsEntry = unsafe { std::mem::transmute(function.entry) };
        assert!(function.info.frame_slots > 1);
        let mut frame = CpsFrame::new(function.info.frame_slots);
        frame.slots[0] = 1;
        for state in 1..=3 {
            assert_eq!(unsafe { entry(&mut frame) }, 1);
            assert_eq!(frame.state, state);
        }
        assert_eq!(unsafe { entry(&mut frame) }, 0);
        assert_eq!(frame.result, 1);
        assert_eq!(frame.state, DONE);
        assert_eq!(unsafe { entry(&mut frame) }, INVALID_STATE);
        frame.state = 999;
        assert_eq!(unsafe { entry(&mut frame) }, INVALID_STATE);
    }
}
