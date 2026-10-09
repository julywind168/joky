//! Shared Cranelift code generation for the JIT and AOT backends.
mod float_env;
mod lower;

use crate::{
    core::{
        ir::{FunctionInfo, ModulePlan, Op, TextSource},
        resolver::BindingRegistry,
    },
    Error, ErrorCode, ErrorPhase,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};

/// Compiled definitions returned to a backend.
pub(crate) struct CompiledDefinitions {
    /// Exported functions in source order of their names.
    pub exports: HashMap<String, (FuncId, FunctionInfo)>,
    pub instance_entries: HashMap<String, (FuncId, FunctionInfo)>,
    pub instance_hooks: HashMap<String, Vec<(crate::core::ir::DropHook, FuncId)>>,
    /// Whether any compiled body, exported or internal, needs the native
    /// runtime. Private module functions can fail or suspend without being
    /// reachable from an export, so this must not be inferred from `exports`.
    pub requires_runtime: bool,
}

/// Lower a checked plan into temporary Cranelift contexts and owned definitions.
pub(crate) fn compile<M: Module>(
    module: &mut M,
    plan: &ModulePlan,
    symbol_prefix: &str,
    bindings: &BindingRegistry,
) -> Result<CompiledDefinitions, Error> {
    let _float_env = crate::runtime::float_env::Guard::enter();
    // Imported text methods are available through the prelude, but unused
    // library code must not add a runtime dependency to scalar-only objects.
    let mut reachable: HashSet<String> = plan
        .functions
        .iter()
        .filter(|(_, info)| !info.internal || info.instance_entry)
        .map(|(name, _)| name.clone())
        .collect();
    let mut pending: Vec<_> = reachable.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        let Some(body) = plan.bodies.get(&name) else {
            continue;
        };
        let mut dependencies = Vec::new();
        for op in &body.ops {
            match op {
                Op::Call { name, .. } | Op::FunctionRef(_, name) => dependencies.push(name),
                Op::DynTable { table, .. } => dependencies.extend(&table.methods),
                Op::Scope { branches, .. } => dependencies.extend(branches.iter().map(|b| &b.name)),
                Op::TaskBranch { entry, .. } => dependencies.push(entry),
                Op::Cleanup(crate::core::ir::CleanupOp::Register {
                    hooks, elements, ..
                }) => {
                    dependencies.extend(hooks.iter().map(|h| &h.name));
                    dependencies.extend(
                        elements
                            .iter()
                            .flat_map(|e| e.hooks.iter().map(|h| &h.name)),
                    );
                }
                _ => {}
            }
        }
        for dependency in dependencies {
            if reachable.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }
    let mut ids = lower::declare_helpers(module)?;
    let mut declared_symbols: HashSet<_> = ids.keys().cloned().collect();
    let mut entry_ids = HashMap::new();
    let mut borrowing_hosts = HashMap::new();
    let mut text_ffi_hosts = HashMap::new();
    let mut resource_factories = HashMap::new();
    let mut text_hosts = HashMap::new();
    let mut names: Vec<_> = plan
        .functions
        .keys()
        .filter(|name| reachable.contains(*name) && !name.starts_with("__joky_indirect_"))
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
        if plan.functions[name].tcp.is_some() {
            linkage = Linkage::Local;
            symbol = format!("__joky_tcp_adapter_{symbol}");
        } else if plan.functions[name].resource_factory {
            let descriptor_symbol = if plan.functions[name].byte_io.is_some() {
                crate::runtime::byte_request::symbol(&symbol)
            } else if plan.functions[name].suspends {
                crate::runtime::resource::cps_symbol(&symbol)
            } else {
                crate::runtime::resource::symbol(&symbol)
            };
            if !declared_symbols.insert(descriptor_symbol.clone()) {
                return Err(compile_error("Conflicting resource factory symbol"));
            }
            let descriptor = module
                .declare_data(&descriptor_symbol, Linkage::Import, false, false)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            resource_factories.insert(name.clone(), descriptor);
            linkage = Linkage::Local;
            symbol = format!("__joky_resource_adapter_{symbol}");
        } else if plan.functions[name].text_ffi {
            let descriptor_symbol = crate::runtime::text_ffi::symbol(&symbol);
            if !declared_symbols.insert(descriptor_symbol.clone()) {
                return Err(compile_error(format!(
                    "Conflicting text FFI descriptor {descriptor_symbol}"
                )));
            }
            let descriptor = module
                .declare_data(&descriptor_symbol, Linkage::Import, false, false)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            text_ffi_hosts.insert(name.clone(), descriptor);
            linkage = Linkage::Local;
            symbol = format!("__joky_text_ffi_adapter_{symbol}");
        } else if plan.functions[name].host_text {
            let mut wire_info = plan.functions[name].clone();
            wire_info.needs_arena = false;
            let wire = module
                .declare_function(
                    &symbol,
                    Linkage::Import,
                    &lower::signature(module, &wire_info),
                )
                .map_err(|e| compile_error(format!("{e:?}")))?;
            declared_symbols.insert(symbol.clone());
            text_hosts.insert(name.clone(), wire);
            linkage = Linkage::Local;
            symbol = format!("__joky_host_text_adapter_{symbol}");
        }
        if plan.functions[name].borrowing_host {
            let descriptor_symbol = crate::core::ffi::borrowing_symbol(&symbol);
            if !declared_symbols.insert(descriptor_symbol.clone()) {
                return Err(compile_error(format!(
                    "Conflicting borrowing descriptor {descriptor_symbol}"
                )));
            }
            let descriptor = module
                .declare_data(&descriptor_symbol, Linkage::Import, false, false)
                .map_err(|e| compile_error(format!("{e:?}")))?;
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
        let text_entry = linkage == Linkage::Export && plan.functions[name].signature.has_text();
        if text_entry {
            symbol = format!("__joky_user_text_v2_{symbol}");
        }
        if linkage == Linkage::Export
            && (text_entry || plan.functions[name].needs_arena && !plan.functions[name].suspends)
        {
            let mut public_info = plan.functions[name].clone();
            public_info.needs_arena = false;
            let entry = module
                .declare_function(
                    &symbol,
                    Linkage::Export,
                    &if text_entry {
                        lower::text_signature(module, &public_info)
                    } else {
                        lower::signature(module, &public_info)
                    },
                )
                .map_err(|e| compile_error(format!("{e:?}")))?;
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
    let mut dyn_ids = HashMap::new();
    for (_, body) in plan
        .bodies
        .iter()
        .filter(|(name, _)| reachable.contains(*name))
    {
        for op in &body.ops {
            if let Op::DynTable { table, .. } = op {
                let methods = &table.methods;
                let key = table.id;
                if let std::collections::hash_map::Entry::Vacant(entry) = dyn_ids.entry(key) {
                    let width = module.target_config().pointer_bytes() as usize;
                    let mut data = DataDescription::new();
                    data.define(vec![0; width * methods.len().max(1)].into_boxed_slice());
                    data.set_align(width as u64);
                    for (slot, name) in methods.iter().enumerate() {
                        let function = module.declare_func_in_data(ids[name], &mut data);
                        data.write_function_addr((slot * width) as u32, function);
                    }
                    let id = module
                        .declare_anonymous_data(false, false)
                        .map_err(|e| compile_error(format!("{e:?}")))?;
                    module
                        .define_data(id, &data)
                        .map_err(|e| compile_error(format!("{e:?}")))?;
                    entry.insert(id);
                }
            }
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
                        .map_err(|e| compile_error(format!("{e:?}")))?;
                    module
                        .define_data(id, &data)
                        .map_err(|e| compile_error(format!("{e:?}")))?;
                    callable_ids.insert(name.clone(), id);
                }
            }
            if let Op::Text {
                source: TextSource::Literal(text),
                ..
            } = op
            {
                if data_ids.contains_key(text) {
                    continue;
                }
                let id = module
                    .declare_anonymous_data(false, false)
                    .map_err(|error| compile_error(error.to_string()))?;
                let mut data = DataDescription::new();
                data.define(text.as_bytes().to_vec().into_boxed_slice());
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
        if let Some(op) = plan.functions[name].tcp {
            ctx.clear();
            ctx.func.signature = lower::signature(module, &plan.functions[name]);
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let refs = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            lower::tcp_entry(&mut builder, op, &refs);
            builder.finalize(module.target_config());
            float_env::instrument(module, &mut ctx.func, &ids);
            module
                .define_function(ids[name], &mut ctx)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            requires_runtime = true;
            continue;
        }
        if let Some(&descriptor) = resource_factories.get(name) {
            ctx.clear();
            ctx.func.signature = lower::signature(module, &plan.functions[name]);
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let refs = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            let data = module.declare_data_in_func(descriptor, builder.func);
            lower::resource_factory_entry(
                &mut builder,
                plan.functions[name].suspends,
                plan.functions[name].byte_io,
                data,
                &refs,
                module.target_config().pointer_type(),
            );
            builder.finalize(module.target_config());
            float_env::instrument(module, &mut ctx.func, &ids);
            module
                .define_function(ids[name], &mut ctx)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            requires_runtime = true;
            continue;
        }
        if let Some(&descriptor) = text_ffi_hosts.get(name) {
            ctx.clear();
            let info = &plan.functions[name];
            ctx.func.signature = lower::signature(module, info);
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let refs = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            let data = module.declare_data_in_func(descriptor, builder.func);
            lower::text_ffi_entry(
                &mut builder,
                data,
                info,
                &refs,
                module.target_config().pointer_type(),
            );
            builder.finalize(module.target_config());
            float_env::instrument(module, &mut ctx.func, &ids);
            module
                .define_function(ids[name], &mut ctx)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            requires_runtime = true;
            continue;
        }
        if let Some(&wire) = text_hosts.get(name) {
            ctx.clear();
            let info = &plan.functions[name];
            ctx.func.signature = lower::signature(module, info);
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let refs = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            let body = module.declare_func_in_func(wire, builder.func);
            if info.suspends {
                lower::host_text_cps_entry(
                    &mut builder,
                    body,
                    info,
                    &refs,
                    module.target_config().pointer_type(),
                );
            } else {
                lower::host_text_direct_entry(
                    &mut builder,
                    body,
                    info,
                    &refs,
                    module.target_config().pointer_type(),
                );
            }
            builder.finalize(module.target_config());
            float_env::instrument(module, &mut ctx.func, &ids);
            module
                .define_function(ids[name], &mut ctx)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            requires_runtime = true;
            continue;
        }
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
            float_env::instrument(module, &mut ctx.func, &ids);
            module
                .define_function(ids[name], &mut ctx)
                .map_err(|e| compile_error(format!("{e:?}")))?;
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
                dyn_tables: dyn_ids
                    .iter()
                    .map(|(key, &id)| (*key, module.declare_data_in_func(id, builder.func)))
                    .collect(),
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
        requires_runtime |= float_env::instrument(module, &mut ctx.func, &ids);
        module
            .define_function(ids[name], &mut ctx)
            .map_err(|e| compile_error(format!("Failed to define {name}: {e:?}")))?;
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
        ctx.func.signature = if info.signature.has_text() {
            lower::text_signature(module, &info)
        } else {
            lower::signature(module, &info)
        };
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let refs = ids
            .iter()
            .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
            .collect();
        let body = module.declare_func_in_func(ids[&name], builder.func);
        if info.signature.has_text() {
            lower::text_entry(
                &mut builder,
                body,
                &info,
                &refs,
                module.target_config().pointer_type(),
            );
        } else {
            lower::arena_entry(&mut builder, body, &refs);
        }
        builder.finalize(module.target_config());
        float_env::instrument(module, &mut ctx.func, &ids);
        module
            .define_function(id, &mut ctx)
            .map_err(|e| compile_error(format!("{e:?}")))?;
        requires_runtime = true;
    }
    let mut instance_entries = HashMap::new();
    let mut instance_hooks = HashMap::new();
    for name in &names {
        let name = *name;
        let info = &plan.functions[name];
        if !info.instance_entry {
            continue;
        }
        let entry = ids[name];
        ctx.clear();
        ctx.func.signature = lower::instance_signature(module);
        let id = module
            .declare_function(
                &crate::core::instance::symbol(symbol_prefix, name),
                Linkage::Export,
                &ctx.func.signature,
            )
            .map_err(|e| compile_error(e.to_string()))?;
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let body = module.declare_func_in_func(entry, builder.func);
        lower::instance_entry(&mut builder, body, info);
        builder.finalize(module.target_config());
        requires_runtime |= float_env::instrument(module, &mut ctx.func, &ids);
        module
            .define_function(id, &mut ctx)
            .map_err(|e| compile_error(e.to_string()))?;
        if crate::core::instance::constructor(info) {
            instance_hooks.insert(
                name.clone(),
                plan.bodies[name]
                    .instance_hooks(&info.signature.ret)
                    .into_iter()
                    .map(|hook| {
                        let id = ids[&hook.name];
                        (hook, id)
                    })
                    .collect(),
            );
        }
        instance_entries.insert(name.clone(), (id, info.clone()));
    }
    Ok(CompiledDefinitions {
        exports,
        instance_entries,
        instance_hooks,
        requires_runtime,
    })
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}
