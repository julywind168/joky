//! Shared Cranelift code generation for the JIT and AOT backends.
mod lower;

use crate::parser::ast::{Item, Module as AstModule};
use crate::{
    core::{
        ir::{Constant, FunctionInfo, ModulePlan, Op},
        resolver::BindingRegistry,
    },
    Error, ErrorCode, ErrorPhase, TypeRegistry,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};

/// Compile an already-resolved module (see resolver::resolve) and its Cranelift
/// contexts. Compilation contexts are temporary; only exports survive.
pub(crate) fn compile_with_bindings<M: Module>(
    module: &mut M,
    ast: &AstModule,
    imports: &HashMap<String, usize>,
    symbol_prefix: &str,
    bindings: &BindingRegistry,
    types: &TypeRegistry,
) -> Result<HashMap<String, (FuncId, FunctionInfo)>, Error> {
    bindings.validate_declarations(ast)?;
    let plan = ModulePlan::with_import_layouts_and_types(ast, imports, types)?;
    let mut ids = lower::declare_helpers(module)?;
    let mut declared_symbols: HashSet<_> = ids.keys().cloned().collect();
    for item in &ast.items {
        let (name, linkage) = match item {
            Item::FnDef(f) => (&f.name, Linkage::Export),
            Item::ExternFn(f) => (&f.name, Linkage::Import),
        };
        let signature = lower::signature(module, &plan.functions[name]);
        let symbol = if linkage == Linkage::Import {
            bindings
                .binding(name)
                .and_then(|binding| binding.symbol.clone())
        } else {
            None
        }
        .unwrap_or_else(|| format!("{symbol_prefix}{name}"));
        if !declared_symbols.insert(symbol.clone()) {
            let span = match item {
                Item::FnDef(f) => f.span,
                Item::ExternFn(f) => f.span,
            };
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                format!("Native symbol `{symbol}` conflicts with another declaration"),
            )
            .with_span(span));
        }
        let id = module
            .declare_function(&symbol, linkage, &signature)
            .map_err(|error| compile_error(error.to_string()))?;
        ids.insert(name.clone(), id);
    }
    let mut ctx = Context::new();
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut exports = HashMap::new();
    let mut data_ids = HashMap::new();
    for body in plan.bodies.values() {
        for op in &body.ops {
            if let Op::Literal(_, Constant::String(text)) = op {
                if data_ids.contains_key(text) {
                    continue;
                }
                let id = module
                    .declare_anonymous_data(false, false)
                    .map_err(|error| compile_error(error.to_string()))?;
                let mut data = DataDescription::new();
                data.define(crate::runtime::string::encode(text));
                data.set_align(8);
                module
                    .define_data(id, &data)
                    .map_err(|error| compile_error(error.to_string()))?;
                data_ids.insert(text.clone(), id);
            }
        }
    }
    for item in &ast.items {
        let Item::FnDef(f) = item else { continue };
        ctx.clear();
        let info = &plan.functions[&f.name];
        ctx.func.signature = lower::signature(module, info);
        let target = module.target_config();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let refs = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            let strings = data_ids
                .iter()
                .map(|(text, &id)| (text.clone(), module.declare_data_in_func(id, builder.func)))
                .collect();
            lower::lower(
                &mut builder,
                info,
                &plan.bodies[&f.name],
                &plan.functions,
                &refs,
                target.pointer_type(),
                &strings,
            );
            builder.finalize(target);
        }
        module
            .define_function(ids[&f.name], &mut ctx)
            .map_err(|e| compile_error(format!("Failed to define {}: {e}", f.name)))?;
        exports.insert(f.name.clone(), (ids[&f.name], info.clone()));
    }
    Ok(exports)
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}
