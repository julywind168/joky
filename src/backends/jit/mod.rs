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
use std::{
    any::Any,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub(crate) struct CompiledFunction {
    pub abi: crate::EntryAbi,
    pub entry: *const u8,
    pub info: FunctionInfo,
}

/// Contains code and export metadata, never compilation contexts or source ASTs.
pub(crate) struct CompiledModule {
    pub(crate) request_contracts: crate::core::script::RequestContracts,
    pub version: ModuleId,
    pub files: Option<crate::runtime::fs::FileService>,
    pub sources: std::sync::Arc<crate::SourceMap>,
    pub exports: HashMap<String, CompiledFunction>,
    /// Raw CPS request entries; source visibility is checked separately.
    pub request_entries: HashMap<String, CompiledFunction>,
    pub instance_entries: HashMap<String, CompiledFunction>,
    pub(crate) parallel: crate::core::ir::parallel::Effects,
    pub(crate) parallel_entries: HashMap<String, CompiledFunction>,
    _code: Arc<Mutex<OwnedCode>>,
    _dependencies: Vec<Rc<dyn Any>>,
}

#[cfg(test)]
impl CompiledModule {
    /// AOT test entries use this same VM dispatcher and the same Rust runtime.
    pub(crate) fn instance_test_module(
        sources: std::sync::Arc<crate::SourceMap>,
        instance_entries: HashMap<String, CompiledFunction>,
        owner: Rc<dyn Any>,
    ) -> Self {
        Self {
            request_contracts: Default::default(),
            files: None,
            version: ModuleId(1),
            sources,
            exports: HashMap::new(),
            request_entries: HashMap::new(),
            instance_entries,
            parallel: Default::default(),
            parallel_entries: HashMap::new(),
            _code: Arc::new(Mutex::new(OwnedCode(None))),
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
            request_contracts: Default::default(),
            files: None,
            version: ModuleId(0),
            sources: Default::default(),
            request_entries: exports.clone(),
            exports,
            instance_entries,
            parallel: Default::default(),
            parallel_entries: HashMap::new(),
            _code: Arc::new(Mutex::new(OwnedCode(None))),
            _dependencies: vec![owner],
        }
    }
}
/// Finalized executable pages and checked metadata, independent of the local VM.
pub(crate) struct FrozenEntry {
    pub(crate) files: Option<crate::runtime::fs::FileService>,
    pub(crate) entry: cps::CpsEntry,
    pub(crate) info: FunctionInfo,
    pub(crate) sources: Arc<crate::SourceMap>,
    _code: Arc<dyn Any + Send + Sync>,
    _hosts: Vec<crate::runtime::parallel::Host>,
}
impl CompiledModule {
    pub(crate) fn freeze(&self, name: &str) -> Result<FrozenEntry, Error> {
        let signature = self.parallel.require_packet(name)?;
        let function = self
            .parallel_entries
            .get(name)
            .ok_or_else(|| compile_error("Unknown parallel export"))?;
        Ok(FrozenEntry {
            files: self.files.clone(),
            entry: unsafe { crate::runtime::abi::as_cps_entry(function.entry) },
            info: FunctionInfo {
                signature: signature.clone(),
                ..function.info.clone()
            },
            sources: self.sources.clone(),
            _code: self._code.clone(),
            _hosts: self
                ._dependencies
                .iter()
                .filter_map(|owner| {
                    owner
                        .downcast_ref::<crate::runtime::parallel::Host>()
                        .cloned()
                })
                .collect(),
        })
    }
}
impl CompiledModule {
    /// The caller retains the complete local module until the granted call ends.
    pub(crate) fn freeze_domain(&self, name: &str) -> Result<FrozenEntry, Error> {
        let function = self
            .instance_entries
            .get(name)
            .filter(|f| f.info.domain_entry)
            .ok_or_else(|| compile_error("Unknown checked domain entry"))?;
        Ok(FrozenEntry {
            files: None,
            entry: unsafe { crate::runtime::abi::as_cps_entry(function.entry) },
            info: function.info.clone(),
            sources: self.sources.clone(),
            _code: self._code.clone(),
            _hosts: Vec::new(),
        })
    }
}
impl FrozenEntry {
    pub(crate) unsafe fn aot(
        entry: cps::CpsEntry,
        info: FunctionInfo,
        sources: Arc<crate::SourceMap>,
        owner: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            files: None,
            entry,
            info,
            sources,
            _code: owner,
            _hosts: Vec::new(),
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
    let mut builder = JITBuilder::with_flags(
        &[("opt_level", crate::DEFAULT_CODEGEN_OPT_LEVEL)],
        default_libcall_names(),
    )
    .map_err(|error| compile_error(error.to_string()))?;
    for &(name, ptr) in symbols {
        builder.symbol(name, ptr);
    }
    for (name, ptr) in cps::symbols() {
        builder.symbol(name, ptr);
    }
    for (name, ptr) in crate::runtime::error::symbols() {
        builder.symbol(
            "__joky_vm_migration_open",
            crate::stdlib::migration_open_native as *const u8,
        );
        builder.symbol(
            "__joky_vm_migration_command",
            crate::stdlib::migration_command_native as *const u8,
        );
        builder.symbol(
            "__joky_vm_migration_diagnostic",
            crate::stdlib::migration_diagnostic_native as *const u8,
        );
        builder.symbol(name, ptr);
    }
    builder.symbol(
        "__joky_outbox_emit_v1",
        crate::runtime::domain::emit_native as *const u8,
    );
    for (name, ptr) in crate::runtime::buffer::symbols()
        .into_iter()
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain([(
            "__joky_domain_owner_v1",
            crate::runtime::domain_packet::transfer as *const u8,
        )])
        .chain([(
            "__joky_text_ffi_call_v1",
            crate::runtime::text_ffi::call as *const u8,
        )])
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::place::symbols())
        .chain(crate::runtime::resource::symbols())
        .chain(crate::runtime::byte_request::symbols())
        .chain([(
            "__joky_fs_poll",
            crate::runtime::fs::poll_native as *const u8,
        )])
        .chain([(
            "__joky_tcp_poll",
            crate::runtime::tcp::poll_native as *const u8,
        )])
        .chain([(
            "__joky_password_poll",
            crate::runtime::password::poll_native as *const u8,
        )])
        .chain([(
            "__joky_dns_poll",
            crate::runtime::dns::poll_native as *const u8,
        )])
        .chain([(
            "__joky_vm_call",
            crate::stdlib::call_poll_native as *const u8,
        )])
        .chain([(
            "__joky_vm_source",
            crate::stdlib::source_poll_native as *const u8,
        )])
        .chain([(
            "__joky_postgres_poll",
            crate::runtime::postgres::poll_native as *const u8,
        )])
        .chain([(
            "__joky_tls_poll",
            crate::runtime::tls::poll_native as *const u8,
        )])
        .chain([(
            "__joky_shutdown_poll",
            crate::runtime::shutdown::poll_native as *const u8,
        )])
        .chain(crate::runtime::float_env::symbols())
        .chain([(
            "__joky_domain_poll_v4",
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
        request_contracts: Default::default(),
        parallel_entries: compiled
            .parallel_entries
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
        request_entries: compiled
            .request_entries
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
        files: None,
        sources: prepared.ast().sources.clone(),
        version,
        exports,
        parallel: crate::core::ir::parallel::analyse(
            &plan,
            prepared,
            &dependencies
                .iter()
                .filter_map(|owner| {
                    owner
                        .downcast_ref::<crate::runtime::parallel::Host>()
                        .map(|host| host.name.clone())
                })
                .collect(),
        ),
        _code: Arc::new(Mutex::new(code)),
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
                let local = { let saved = false; yield_once(); saved }
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
