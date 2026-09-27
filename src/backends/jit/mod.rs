//! JIT compilation produces an immutable, independently owned code module.
use crate::core::ir::FunctionInfo;
use crate::{
    backends::native,
    core::resolver::BindingRegistry,
    parser::ast::Module,
    runtime::{cps, suspension::ModuleId},
    Error, ErrorCode, ErrorPhase, TypeRegistry,
};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::default_libcall_names;
use std::{any::Any, collections::HashMap, rc::Rc};

pub(crate) struct CompiledFunction {
    pub entry: *const u8,
    pub info: FunctionInfo,
}

/// Contains code and export metadata, never compilation contexts or source ASTs.
pub(crate) struct CompiledModule {
    pub version: ModuleId,
    pub exports: HashMap<String, CompiledFunction>,
    _code: OwnedCode,
    _dependencies: Vec<Rc<dyn Any>>,
}

struct OwnedCode(Option<JITModule>);
impl Drop for OwnedCode {
    fn drop(&mut self) {
        // The last module owner drops only after all continuations are gone.
        unsafe {
            self.0.take().unwrap().free_memory();
        }
    }
}

pub(crate) fn compile_with_registry(
    ast: &Module,
    symbols: &[(&str, *const u8)],
    layouts: &HashMap<String, usize>,
    dependencies: Vec<Rc<dyn Any>>,
    version: ModuleId,
    bindings: &BindingRegistry,
    types: &TypeRegistry,
) -> Result<CompiledModule, Error> {
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
    let mut code = OwnedCode(Some(JITModule::new(builder)));
    let module = code.0.as_mut().unwrap();
    let ids = native::compile_with_bindings(module, ast, layouts, "", bindings, types)?;
    module
        .finalize_definitions()
        .map_err(|error| compile_error(error.to_string()))?;
    let exports = ids
        .into_iter()
        .map(|(name, (id, info))| {
            (
                name,
                CompiledFunction {
                    entry: module.get_finalized_function(id),
                    info,
                },
            )
        })
        .collect();
    Ok(CompiledModule {
        version,
        exports,
        _code: code,
        _dependencies: dependencies,
    })
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}

#[cfg(test)]
mod tests {
    use super::*;

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
