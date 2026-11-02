//! Shared Cranelift code generation for the JIT and AOT backends.
mod domain;
mod float_env;
mod lower;
mod task;

use crate::core::builtins::{Builtin, ExternAbi};
use crate::{
    core::{
        ir::{FunctionInfo, ModulePlan, Op, TextSource},
        resolver::BindingRegistry,
    },
    Error, ErrorCode, ErrorPhase,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};

/// Compiled definitions returned to a backend.
pub(crate) struct CompiledDefinitions {
    pub parallel_entries: HashMap<String, (FuncId, FunctionInfo)>,
    /// Exported functions in source order of their names.
    pub exports: HashMap<String, (FuncId, FunctionInfo)>,
    /// Raw CPS entries for checked requests, without public host ABI wrappers.
    pub request_entries: HashMap<String, (FuncId, FunctionInfo)>,
    pub instance_entries: HashMap<String, (FuncId, FunctionInfo)>,
    pub instance_hooks: HashMap<String, Vec<(crate::core::ir::DropHook, FuncId)>>,
    /// Whether any compiled body, exported or internal, needs the native
    /// runtime. Private module functions can fail or suspend without being
    /// reachable from an export, so this must not be inferred from `exports`.
    pub requires_runtime: bool,
}

pub(crate) fn task_root_symbol(prefix: &str, name: &str) -> String {
    format!("__joky_task_root_v1_{prefix}{name}")
}

/// Each imported function selects exactly one native adapter.
enum Adapter<'a> {
    Builtin(&'a Builtin),
    Resource(DataId),
    TextFfi(DataId),
    Text(FuncId),
    Borrowing(DataId),
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
        .filter(|(_, info)| {
            !info.internal || info.instance_entry || info.script_entry || info.parallel_entry
        })
        .map(|(name, _)| name.clone())
        .collect();
    let indirect = crate::core::ir::parallel::scoped::indirect_entries(
        plan,
        &crate::core::ir::parallel::scoped::rejections(plan),
    );
    let mut pending: Vec<_> = reachable.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        let Some(body) = plan.bodies.get(&name) else {
            continue;
        };
        let frozen = body
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Concurrent { branches, .. } => Some(branches),
                _ => None,
            })
            .flatten()
            .filter_map(|branch| indirect.get(&branch.name).and_then(|e| e.as_ref().ok()))
            .flat_map(|e| &e.targets);
        for dependency in body.ops.iter().flat_map(Op::dependencies).chain(frozen) {
            if reachable.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }
    // Checkpoints are needed on CPS roots and their entire synchronous call
    // graph. Standalone synchronous scalar objects keep their runtime-free ABI.
    let mut cancellable: HashSet<String> = plan
        .functions
        .iter()
        .filter(|(_, f)| f.suspends)
        .map(|(n, _)| n.clone())
        .collect();
    let mut pending: Vec<_> = cancellable.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        if let Some(body) = plan.bodies.get(&name) {
            for dependency in body.ops.iter().flat_map(Op::dependencies) {
                if cancellable.insert(dependency.clone()) {
                    pending.push(dependency.clone());
                }
            }
        }
    }
    let mut functions = plan.functions.clone();
    for name in &cancellable {
        if let Some(info) = functions.get_mut(name) {
            info.may_fail = true;
        }
    }
    let mut ids = lower::declare_helpers(module)?;
    let float_effects = float_env::Effects::runtime_helpers(&ids);
    let mut declared_symbols: HashSet<_> = ids.keys().cloned().collect();
    let mut entry_ids = HashMap::new();
    let mut adapters = HashMap::new();
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
            if plan.functions[name].extern_abi == ExternAbi::C {
                name.to_owned()
            } else {
                format!("{symbol_prefix}{name}")
            }
        });
        if let Some(builtin) = plan.functions[name]
            .builtin
            .as_ref()
            .filter(|op| op.needs_adapter())
        {
            let family = match builtin {
                Builtin::VmCall | Builtin::VmSource | Builtin::VmMigration(_) => "vm",
                Builtin::Domain(_) => "domain",
                Builtin::Dns => "dns",
                Builtin::Password => "password",
                Builtin::Postgres(_) => "postgres",
                Builtin::Tls(_) => "tls",
                Builtin::Shutdown(_) => "shutdown",
                Builtin::Fs(_) => "fs",
                Builtin::Tcp(_) => "tcp",
                Builtin::Vm(_) | Builtin::Stdout | Builtin::Stderr => unreachable!(),
            };
            adapters.insert(name.clone(), Adapter::Builtin(builtin));
            linkage = Linkage::Local;
            symbol = format!("__joky_{family}_adapter_{symbol}");
        } else if plan.functions[name].extern_abi.is_resource() {
            let descriptor_symbol = if plan.functions[name].extern_abi.byte_io().is_some() {
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
            adapters.insert(name.clone(), Adapter::Resource(descriptor));
            linkage = Linkage::Local;
            symbol = format!("__joky_resource_adapter_{symbol}");
        } else if plan.functions[name].extern_abi == ExternAbi::TextFfi {
            let descriptor_symbol = crate::runtime::text_ffi::symbol(&symbol);
            if !declared_symbols.insert(descriptor_symbol.clone()) {
                return Err(compile_error(format!(
                    "Conflicting text FFI descriptor {descriptor_symbol}"
                )));
            }
            let descriptor = module
                .declare_data(&descriptor_symbol, Linkage::Import, false, false)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            adapters.insert(name.clone(), Adapter::TextFfi(descriptor));
            linkage = Linkage::Local;
            symbol = format!("__joky_text_ffi_adapter_{symbol}");
        } else if plan.functions[name].extern_abi == ExternAbi::TextView {
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
            adapters.insert(name.clone(), Adapter::Text(wire));
            linkage = Linkage::Local;
            symbol = format!("__joky_host_text_adapter_{symbol}");
        }
        if plan.functions[name].extern_abi == ExternAbi::Borrowing {
            let descriptor_symbol = crate::core::ffi::borrowing_symbol(&symbol);
            if !declared_symbols.insert(descriptor_symbol.clone()) {
                return Err(compile_error(format!(
                    "Conflicting borrowing descriptor {descriptor_symbol}"
                )));
            }
            let descriptor = module
                .declare_data(&descriptor_symbol, Linkage::Import, false, false)
                .map_err(|e| compile_error(format!("{e:?}")))?;
            adapters.insert(name.clone(), Adapter::Borrowing(descriptor));
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
        // Source-visible aggregate CPS roots need a private task entry while
        // remaining unavailable through the ordinary scalar/text host ABI.
        let info = &plan.functions[name];
        if info.internal && info.parallel_entry {
            symbol = task_root_symbol(symbol_prefix, name);
            linkage = Linkage::Export;
        }
        let id = module
            .declare_function(&symbol, linkage, &signature)
            .map_err(|error| compile_error(error.to_string()))?;
        ids.insert(name.clone(), id);
    }
    // Runtime-backed definitions have private direct-call bodies. All published
    // IDs and function addresses retain the protected entry, including CPS
    // descriptors and Drop hooks. Scalar-only objects keep their runtime-free ABI.
    let mut float_bodies = HashMap::new();
    for name in &names {
        let info = &functions[*name];
        if (plan.bodies.contains_key(*name) || adapters.contains_key(*name))
            && (info.needs_arena || info.suspends)
        {
            let body = module
                .declare_anonymous_function(&lower::signature(module, info))
                .map_err(|e| compile_error(e.to_string()))?;
            float_bodies.insert(ids[*name], body);
        }
    }
    let mut instance_ids = HashMap::new();
    for name in &names {
        if plan.functions[*name].instance_entry {
            let id = module
                .declare_function(
                    &crate::core::instance::symbol(symbol_prefix, name),
                    Linkage::Export,
                    &lower::instance_signature(module),
                )
                .map_err(|e| compile_error(e.to_string()))?;
            instance_ids.insert((*name).clone(), id);
        }
    }
    let domain_data = if plan
        .bodies
        .values()
        .any(|body| body.ops.iter().any(|op| matches!(op, Op::DomainModule(_))))
    {
        Some(domain::metadata(module, plan, &names, &ids, &instance_ids)?)
    } else {
        None
    };
    let mut place_ids = Vec::new();
    for body in plan.bodies.values() {
        let types = body
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Place(op) => Some(op.ty()),
                Op::Buffer(crate::core::ir::BufferOp::Storage { element, .. })
                    if element.is_place() =>
                {
                    Some(element)
                }
                _ => None,
            })
            .chain(body.result.iter().map(|s| &s.ty).filter(|ty| ty.is_place()))
            .flat_map(|ty| {
                crate::core::types::walk::TypeWalk::new(
                    ty,
                    crate::core::types::walk::TypeEdges::Fields,
                )
            });
        for ty in types {
            if place_ids.iter().any(|(known, _, _)| known == ty) {
                continue;
            }
            let program = ty.place_copy_program();
            let mut bytes = Vec::new();
            for node in &program {
                for word in [node.offset, node.kind, node.count, node.stride, node.end] {
                    bytes.extend(match module.isa().endianness() {
                        cranelift_codegen::ir::Endianness::Little => word.to_le_bytes(),
                        cranelift_codegen::ir::Endianness::Big => word.to_be_bytes(),
                    });
                }
            }
            let id = module
                .declare_anonymous_data(false, false)
                .map_err(|e| compile_error(e.to_string()))?;
            let mut data = DataDescription::new();
            data.define(bytes.into_boxed_slice());
            data.set_align(8);
            module
                .define_data(id, &data)
                .map_err(|e| compile_error(e.to_string()))?;
            place_ids.push((ty.clone(), id, program.len()));
        }
    }
    let task_ids = task::descriptors(module, plan, &reachable, &ids)?;
    let mut ctx = Context::new();
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut exports = HashMap::new();
    let mut requires_runtime = false;
    let mut data_ids = HashMap::new();
    let mut domain_owner_ids = HashMap::new();
    let mut callable_ids = HashMap::new();
    let mut dyn_ids = HashMap::new();
    for (_, body) in plan
        .bodies
        .iter()
        .filter(|(name, _)| reachable.contains(*name))
    {
        for op in &body.ops {
            if let Op::DomainOwner { owner, .. } = op {
                let bytes = crate::runtime::domain::metadata::encode_value_type(owner);
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    domain_owner_ids.entry(bytes)
                {
                    let id = module
                        .declare_anonymous_data(false, false)
                        .map_err(|e| compile_error(e.to_string()))?;
                    let mut data = DataDescription::new();
                    data.define(entry.key().clone().into_boxed_slice());
                    module
                        .define_data(id, &data)
                        .map_err(|e| compile_error(e.to_string()))?;
                    entry.insert(id);
                }
            }
            if let Op::DynTable { table, .. } = op {
                let methods = &table.methods;
                let key = table.id;
                if let std::collections::hash_map::Entry::Vacant(entry) = dyn_ids.entry(key) {
                    let width = module.target_config().pointer_bytes() as usize;
                    let mut data = DataDescription::new();
                    data.define(vec![0; width * methods.len().max(1)].into_boxed_slice());
                    data.set_align(width as u64);
                    for (slot, name) in methods.iter().enumerate() {
                        if plan.functions[name].suspends {
                            let descriptor = match callable_ids.get(name) {
                                Some(id) => *id,
                                None => {
                                    let id = declare_callable(
                                        module,
                                        ids[name],
                                        plan.functions[name].frame_slots,
                                    )?;
                                    callable_ids.insert(name.clone(), id);
                                    id
                                }
                            };
                            let descriptor = module.declare_data_in_data(descriptor, &mut data);
                            data.write_data_addr((slot * width) as u32, descriptor, 0);
                        } else {
                            let function = module.declare_func_in_data(ids[name], &mut data);
                            data.write_function_addr((slot * width) as u32, function);
                        }
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
                    let id = declare_callable(module, ids[name], plan.functions[name].frame_slots)?;
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
        if let Some(adapter) = adapters.get(name) {
            ctx.clear();
            let info = &functions[name];
            ctx.func.signature = lower::signature(module, info);
            let target = module.target_config();
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let refs = ids
                .iter()
                .map(|(name, &id)| (name.clone(), module.declare_func_in_func(id, builder.func)))
                .collect();
            match adapter {
                Adapter::Builtin(builtin) => match builtin {
                    Builtin::Dns => lower::runtime_entry(&mut builder, &refs, "__joky_dns_poll"),
                    Builtin::Password => {
                        lower::runtime_entry(&mut builder, &refs, "__joky_password_poll")
                    }
                    Builtin::VmCall => lower::runtime_entry(&mut builder, &refs, "__joky_vm_call"),
                    Builtin::VmMigration(op) => {
                        lower::runtime_entry(&mut builder, &refs, op.symbol())
                    }
                    Builtin::VmSource => {
                        lower::runtime_entry(&mut builder, &refs, "__joky_vm_source")
                    }
                    Builtin::Domain(op) => lower::domain_entry(&mut builder, *op, &refs),
                    Builtin::Postgres(op) => lower::postgres_entry(&mut builder, *op, &refs),
                    Builtin::Tls(op) => lower::tls_entry(&mut builder, *op, &refs),
                    Builtin::Shutdown(op) => lower::shutdown_entry(&mut builder, *op, &refs),
                    Builtin::Tcp(op) => lower::tcp_entry(&mut builder, *op, &refs),
                    Builtin::Fs(op) => lower::fs_entry(&mut builder, *op, &refs),
                    Builtin::Vm(_) | Builtin::Stdout | Builtin::Stderr => unreachable!(),
                },
                Adapter::Resource(descriptor) => {
                    let data = module.declare_data_in_func(*descriptor, builder.func);
                    lower::resource_factory_entry(
                        &mut builder,
                        info.suspends,
                        info.extern_abi.byte_io(),
                        data,
                        &refs,
                        target.pointer_type(),
                    );
                }
                Adapter::TextFfi(descriptor) => {
                    let data = module.declare_data_in_func(*descriptor, builder.func);
                    lower::text_ffi_entry(&mut builder, data, info, &refs, target.pointer_type());
                }
                Adapter::Text(wire) => {
                    let body = module.declare_func_in_func(*wire, builder.func);
                    if info.suspends {
                        lower::host_text_cps_entry(
                            &mut builder,
                            body,
                            info,
                            &refs,
                            target.pointer_type(),
                        );
                    } else {
                        lower::host_text_direct_entry(
                            &mut builder,
                            body,
                            info,
                            &refs,
                            target.pointer_type(),
                        );
                    }
                }
                Adapter::Borrowing(descriptor) => {
                    let data = module.declare_data_in_func(*descriptor, builder.func);
                    let poll = refs["joky_cps_poll_borrowing_v1"];
                    lower::borrowing_entry(&mut builder, data, poll, target.pointer_type());
                }
            }
            builder.finalize(target);
            float_env::define(
                module,
                &mut ctx,
                ids[name],
                &ids,
                &float_effects,
                &float_bodies,
            )
            .map_err(|e| compile_error(e.to_string()))?;
            requires_runtime = true;
            continue;
        }
        if !plan.bodies.contains_key(name) {
            continue;
        }
        ctx.clear();
        let info = &functions[name];
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
                checkpoints: cancellable.contains(name),
                tasks: task_ids
                    .get(name)
                    .into_iter()
                    .flatten()
                    .map(|(name, id)| {
                        (name.clone(), module.declare_data_in_func(*id, builder.func))
                    })
                    .collect(),
                places: place_ids
                    .iter()
                    .map(|(ty, id, count)| {
                        (
                            ty.clone(),
                            module.declare_data_in_func(*id, builder.func),
                            *count,
                        )
                    })
                    .collect(),
                domain: domain_data.map(|id| module.declare_data_in_func(id, builder.func)),
                dyn_tables: dyn_ids
                    .iter()
                    .map(|(key, &id)| (*key, module.declare_data_in_func(id, builder.func)))
                    .collect(),
                domain_owners: domain_owner_ids
                    .iter()
                    .map(|(bytes, &id)| {
                        (bytes.clone(), module.declare_data_in_func(id, builder.func))
                    })
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
                &functions,
                &refs,
                target.pointer_type(),
                &data,
            );
            builder.finalize(target);
        }
        requires_runtime |= float_env::define(
            module,
            &mut ctx,
            ids[name],
            &ids,
            &float_effects,
            &float_bodies,
        )
        .map_err(|e| compile_error(format!("Failed to define {name}: {e:?}")))?;
        requires_runtime |= info.suspends
            || info.may_fail
            || plan.bodies.get(name).is_some_and(|body| body.has_buffers());
        if !info.internal
            || (info.suspends
                && plan.bodies.values().any(|body| {
                    body.ops
                        .iter()
                        .any(|op| matches!(op,Op::FunctionRef(_,target) if target==name))
                }))
        {
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
        float_env::instrument(module, &mut ctx.func, &ids, &float_effects)
            .map_err(|e| compile_error(e.to_string()))?;
        module
            .define_function(id, &mut ctx)
            .map_err(|e| compile_error(format!("{e:?}")))?;
        requires_runtime = true;
    }
    let mut instance_entries = HashMap::new();
    let mut instance_hooks = HashMap::new();
    for name in &names {
        let name = *name;
        let info = &functions[name];
        if !info.instance_entry {
            continue;
        }
        let entry = ids[name];
        ctx.clear();
        ctx.func.signature = lower::instance_signature(module);
        let id = instance_ids[name];
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let body = module.declare_func_in_func(entry, builder.func);
        lower::instance_entry(&mut builder, body, info);
        builder.finalize(module.target_config());
        requires_runtime |= float_env::instrument(module, &mut ctx.func, &ids, &float_effects)
            .map_err(|e| compile_error(e.to_string()))?;
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
    let request_entries = names
        .iter()
        .filter_map(|name| {
            let info = &plan.functions[*name];
            info.domain_request_entry
                .then(|| ((*name).clone(), (ids[*name], info.clone())))
        })
        .collect();
    let parallel_entries = names
        .iter()
        .filter_map(|name| {
            let info = &plan.functions[*name];
            (info.parallel_entry && plan.bodies.contains_key(*name)).then(|| {
                (
                    (*name).clone(),
                    (
                        exports.get(*name).map_or(ids[*name], |(id, _)| *id),
                        info.clone(),
                    ),
                )
            })
        })
        .collect();
    Ok(CompiledDefinitions {
        parallel_entries,
        request_entries,
        exports,
        instance_entries,
        instance_hooks,
        requires_runtime,
    })
}

fn compile_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, message)
}

/// Module-owned CPS descriptor shared by function values and dynamic methods.
fn declare_callable<M: Module>(
    module: &mut M,
    entry: FuncId,
    frame_slots: usize,
) -> Result<DataId, Error> {
    // The value points to read-only metadata, never a compiler address.
    // Its entry relocation and complete slot count share module ownership.
    let width = module.target_config().pointer_bytes() as usize;
    let count = frame_slots as u64;
    let bytes = match module.isa().endianness() {
        cranelift_codegen::ir::Endianness::Little => count.to_le_bytes()[..width].to_vec(),
        cranelift_codegen::ir::Endianness::Big => count.to_be_bytes()[8 - width..].to_vec(),
    };
    let mut contents = vec![0; width];
    contents.extend(bytes);
    let mut data = DataDescription::new();
    data.define(contents.into_boxed_slice());
    data.set_align(width as u64);
    let function = module.declare_func_in_data(entry, &mut data);
    data.write_function_addr(0, function);
    let id = module
        .declare_anonymous_data(false, false)
        .map_err(|e| compile_error(format!("{e:?}")))?;
    module
        .define_data(id, &data)
        .map_err(|e| compile_error(format!("{e:?}")))?;
    Ok(id)
}
