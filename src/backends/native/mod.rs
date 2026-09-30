//! Shared Cranelift code generation for the JIT and AOT backends.
mod lower;

use crate::parser::ast::Module as AstModule;
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

/// Compiled definitions returned to a backend.
pub(crate) struct CompiledDefinitions {
    /// Exported functions in source order of their names.
    pub exports: HashMap<String, (FuncId, FunctionInfo)>,
    /// Whether any compiled body, exported or internal, needs the native
    /// runtime. Private module functions can fail or suspend without being
    /// reachable from an export, so this must not be inferred from `exports`.
    pub requires_runtime: bool,
}

/// Compile an already-resolved module (see resolver::resolve) and its Cranelift
/// contexts. Compilation contexts are temporary; only definitions survive.
pub(crate) fn compile_with_bindings<M: Module>(
    module: &mut M,
    ast: &AstModule,
    imports: &HashMap<String, usize>,
    symbol_prefix: &str,
    bindings: &BindingRegistry,
    types: &TypeRegistry,
) -> Result<CompiledDefinitions, Error> {
    bindings.validate_declarations(ast)?;
    let plan = ModulePlan::with_import_layouts_and_types(ast, imports, types)?;
    let mut ids = lower::declare_helpers(module)?;
    let mut declared_symbols: HashSet<_> = ids.keys().cloned().collect();
    let mut entry_ids = HashMap::new();
    let mut borrowing_hosts = HashMap::new();
    let mut names: Vec<_> = plan
        .functions
        .keys()
        .filter(|name| !name.starts_with("__joky_indirect_"))
        .collect();
    names.sort();
    for name in &names {
        let name = *name;
        let mut linkage = if !plan.bodies.contains_key(name) {
            Linkage::Import
        } else if plan.functions[name].internal {
            Linkage::Local
        } else {
            Linkage::Export
        };
        let signature = lower::signature(module, &plan.functions[name]);
        let mut symbol = if linkage == Linkage::Import {
            bindings
                .binding(name)
                .and_then(|binding| binding.symbol.clone())
        } else {
            None
        }
        .unwrap_or_else(|| {
            if plan.functions[name].c_abi {
                name.to_owned()
            } else {
                format!("{symbol_prefix}{name}")
            }
        });
        if plan.functions[name].borrowing_host {
            let descriptor_symbol = crate::core::ffi::borrowing_symbol(&symbol);
            if !declared_symbols.insert(descriptor_symbol.clone()) {
                return Err(compile_error(format!(
                    "Conflicting borrowing descriptor {descriptor_symbol}"
                )));
            }
            let descriptor = module
                .declare_data(&descriptor_symbol, Linkage::Import, false, false)
                .map_err(|e| compile_error(e.to_string()))?;
            borrowing_hosts.insert(name.clone(), descriptor);
            // Callers still use the normal CPS shape. This private wrapper makes
            // every path (sequential call or scope/race) use managed host polling.
            linkage = Linkage::Local;
            symbol = format!("__joky_cps_borrow_adapter_{symbol}");
        }
        if !declared_symbols.insert(symbol.clone()) {
            let span = plan.functions[name].span;
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                format!("Native symbol `{symbol}` conflicts with another declaration"),
            )
            .with_span(span));
        }
        if plan.functions[name].needs_arena
            && !plan.functions[name].suspends
            && linkage == Linkage::Export
        {
            let mut public_info = plan.functions[name].clone();
            public_info.needs_arena = false;
            let entry = module
                .declare_function(
                    &symbol,
                    Linkage::Export,
                    &lower::signature(module, &public_info),
                )
                .map_err(|e| compile_error(e.to_string()))?;
            entry_ids.insert(name.clone(), entry);
            symbol = format!("__joky_arena_body_{symbol}");
            linkage = Linkage::Local;
        }
        let id = module
            .declare_function(&symbol, linkage, &signature)
            .map_err(|error| compile_error(error.to_string()))?;
        ids.insert(name.clone(), id);
    }
    let mut ctx = Context::new();
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut exports = HashMap::new();
    let mut requires_runtime = false;
    let mut data_ids = HashMap::new();
    let mut callable_ids = HashMap::new();
    for body in plan.bodies.values() {
        for op in &body.ops {
            if let Op::FunctionRef(slot, name) = op {
                if matches!(&slot.ty, crate::Type::Function(ty) if ty.suspends)
                    && !callable_ids.contains_key(name)
                {
                    // The value points to read-only metadata, never a compiler address.
                    // Its entry relocation and complete slot count share module ownership.
                    let width = module.target_config().pointer_bytes() as usize;
                    let count = plan.functions[name].frame_slots as u64;
                    let bytes = match module.isa().endianness() {
                        cranelift_codegen::ir::Endianness::Little => {
                            count.to_le_bytes()[..width].to_vec()
                        }
                        cranelift_codegen::ir::Endianness::Big => {
                            count.to_be_bytes()[8 - width..].to_vec()
                        }
                    };
                    let mut contents = vec![0; width];
                    contents.extend(bytes);
                    let mut data = DataDescription::new();
                    data.define(contents.into_boxed_slice());
                    data.set_align(width as u64);
                    let function = module.declare_func_in_data(ids[name], &mut data);
                    data.write_function_addr(0, function);
                    let id = module
                        .declare_anonymous_data(false, false)
                        .map_err(|e| compile_error(e.to_string()))?;
                    module
                        .define_data(id, &data)
                        .map_err(|e| compile_error(e.to_string()))?;
                    callable_ids.insert(name.clone(), id);
                }
            }
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
    for name in &names {
        let name = *name;
        if let Some(&descriptor) = borrowing_hosts.get(name) {
            ctx.clear();
            ctx.func.signature = lower::signature(module, &plan.functions[name]);
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let data = module.declare_data_in_func(descriptor, builder.func);
            let poll = module.declare_func_in_func(ids["joky_cps_poll_borrowing_v1"], builder.func);
            lower::borrowing_entry(
                &mut builder,
                data,
                poll,
                module.target_config().pointer_type(),
            );
            builder.finalize(module.target_config());
            module
                .define_function(ids[name], &mut ctx)
                .map_err(|e| compile_error(e.to_string()))?;
            requires_runtime = true;
            continue;
        }
        if !plan.bodies.contains_key(name) {
            continue;
        }
        ctx.clear();
        let info = &plan.functions[name];
        ctx.func.signature = lower::signature(module, info);
        let target = module.target_config();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let mut refs: HashMap<_, _> = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            for (name, id) in &entry_ids {
                refs.insert(
                    format!("joky/callback/{name}"),
                    module.declare_func_in_func(*id, builder.func),
                );
            }
            let data = lower::DataRefs {
                strings: data_ids
                    .iter()
                    .map(|(text, &id)| {
                        (text.clone(), module.declare_data_in_func(id, builder.func))
                    })
                    .collect(),
                callables: callable_ids
                    .iter()
                    .map(|(name, &id)| {
                        (name.clone(), module.declare_data_in_func(id, builder.func))
                    })
                    .collect(),
            };
            lower::lower(
                &mut builder,
                info,
                &plan.bodies[name],
                &plan.functions,
                &refs,
                target.pointer_type(),
                &data,
            );
            builder.finalize(target);
        }
        module
            .define_function(ids[name], &mut ctx)
            .map_err(|e| compile_error(format!("Failed to define {}: {e}", name)))?;
        requires_runtime |= info.suspends
            || info.may_fail
            || plan.bodies.get(name).is_some_and(|body| body.has_buffers());
        if !info.internal {
            exports.insert(
                name.clone(),
                (*entry_ids.get(name).unwrap_or(&ids[name]), info.clone()),
            );
        }
    }
    for (name, id) in entry_ids {
        ctx.clear();
        let mut info = plan.functions[&name].clone();
        info.needs_arena = false;
        ctx.func.signature = lower::signature(module, &info);
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let refs = ids
            .iter()
            .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
            .collect();
        let body = module.declare_func_in_func(ids[&name], builder.func);
        lower::arena_entry(&mut builder, body, &refs);
        builder.finalize(module.target_config());
        module
            .define_function(id, &mut ctx)
            .map_err(|e| compile_error(e.to_string()))?;
        requires_runtime = true;
    }
    Ok(CompiledDefinitions {
        exports,
        requires_runtime,
    })
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}
