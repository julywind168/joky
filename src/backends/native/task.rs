//! Immutable Local task entry descriptors shared by JIT and relocatable objects.
use super::*;
use crate::runtime::cps::task::NativeTaskEntryV2;

pub(super) fn descriptors<M: Module>(
    module: &mut M,
    plan: &ModulePlan,
    reachable: &HashSet<String>,
    ids: &HashMap<String, FuncId>,
) -> Result<HashMap<String, HashMap<String, DataId>>, Error> {
    let scoped = crate::core::ir::parallel::scoped::callees(plan);
    let indirect = crate::core::ir::parallel::scoped::indirect_entries(
        plan,
        &crate::core::ir::parallel::scoped::rejections(plan),
    );
    let mut targets = Vec::new();
    for (parent, body) in &plan.bodies {
        if !reachable.contains(parent) {
            continue;
        }
        for op in &body.ops {
            match op {
                Op::Concurrent {
                    branches, result, ..
                } => {
                    for branch in branches {
                        targets.push((
                            parent.clone(),
                            format!("static:{}", branch.done.index),
                            branch.name.clone(),
                            &branch.args,
                            branch
                                .cleanup
                                .as_ref()
                                .and(branch.result.as_ref().or(result.as_ref()))
                                .map(|slot| slot.owner_key()),
                        ));
                    }
                }
                Op::TaskBranch {
                    entry, captures, ..
                } => {
                    targets.push((parent.clone(), entry.clone(), entry.clone(), captures, None));
                }
                _ => {}
            }
        }
    }
    targets.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut result: HashMap<String, HashMap<String, DataId>> = HashMap::new();
    for (parent, key, name, args, result_key) in targets {
        let info = &plan.functions[&name];
        let inputs: Vec<_> = args
            .iter()
            .enumerate()
            .filter_map(|(to, arg)| {
                arg.as_ref()
                    .filter(|arg| arg.ty.is_suspend_safe_owner())
                    .map(|arg| (arg.owner_key(), to))
            })
            .collect();
        let input_data = module
            .declare_anonymous_data(false, false)
            .map_err(|e| compile_error(e.to_string()))?;
        let mut contents = Vec::new();
        for &(from, to) in &inputs {
            for value in [from, to] {
                contents.extend(match module.isa().endianness() {
                    cranelift_codegen::ir::Endianness::Little => value.to_le_bytes(),
                    cranelift_codegen::ir::Endianness::Big => value.to_be_bytes(),
                });
            }
        }
        let mut input_desc = DataDescription::new();
        if contents.is_empty() {
            contents.push(0);
        }
        input_desc.define(contents.into_boxed_slice());
        input_desc.set_align(8);
        module
            .define_data(input_data, &input_desc)
            .map_err(|e| compile_error(e.to_string()))?;
        let evidence = indirect.get(&name).and_then(|entry| entry.as_ref().ok());
        let signature = evidence.map_or_else(
            || crate::core::ir::parallel::scoped::signature(plan, &name),
            |entry| entry.signature.clone(),
        );
        let scoped_targets = if let Some(evidence) = evidence {
            let mut bytes = vec![0; evidence.targets.len() * 2 * std::mem::size_of::<usize>()];
            for (index, name) in evidence.targets.iter().enumerate() {
                let slots = plan.functions[name].frame_slots;
                let word = match module.isa().endianness() {
                    cranelift_codegen::ir::Endianness::Little => slots.to_le_bytes(),
                    cranelift_codegen::ir::Endianness::Big => slots.to_be_bytes(),
                };
                let offset = (index * 2 + 1) * word.len();
                bytes[offset..offset + word.len()].copy_from_slice(&word);
            }
            let mut data = DataDescription::new();
            data.define(bytes.into_boxed_slice());
            data.set_align(std::mem::align_of::<usize>() as u64);
            for (index, name) in evidence.targets.iter().enumerate() {
                let entry = module.declare_func_in_data(ids[name], &mut data);
                data.write_function_addr((index * 2 * std::mem::size_of::<usize>()) as u32, entry);
            }
            let id = module
                .declare_anonymous_data(false, false)
                .map_err(|e| compile_error(e.to_string()))?;
            module
                .define_data(id, &data)
                .map_err(|e| compile_error(e.to_string()))?;
            Some(id)
        } else {
            None
        };
        let words = crate::runtime::task_scope::layout::encode(&signature)?;
        let mut bytes = Vec::new();
        for word in &words {
            bytes.extend(match module.isa().endianness() {
                cranelift_codegen::ir::Endianness::Little => word.to_le_bytes(),
                cranelift_codegen::ir::Endianness::Big => word.to_be_bytes(),
            });
        }
        let layout = module
            .declare_anonymous_data(false, false)
            .map_err(|e| compile_error(e.to_string()))?;
        let mut data = DataDescription::new();
        data.define(bytes.into_boxed_slice());
        data.set_align(8);
        module
            .define_data(layout, &data)
            .map_err(|e| compile_error(e.to_string()))?;
        // The runtime and compiler target the same native ABI; offsets come from
        // the Rust declaration, never a second handwritten C frame layout.
        let mut bytes = vec![0; std::mem::size_of::<NativeTaskEntryV2>()];
        for (offset, value) in [
            (
                std::mem::offset_of!(NativeTaskEntryV2, scoped_target_count),
                evidence.map_or(0, |entry| entry.targets.len()),
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV2, qualification),
                usize::from(scoped.contains(&name) || evidence.is_some()),
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV2, input_count),
                inputs.len(),
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV2, result_key),
                result_key.unwrap_or(usize::MAX),
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV2, version),
                crate::runtime::cps::task::ENTRY_VERSION,
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV2, frame_slots),
                info.frame_slots,
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV2, layout_words),
                words.len(),
            ),
        ] {
            let word = match module.isa().endianness() {
                cranelift_codegen::ir::Endianness::Little => value.to_le_bytes(),
                cranelift_codegen::ir::Endianness::Big => value.to_be_bytes(),
            };
            bytes[offset..offset + word.len()].copy_from_slice(&word);
        }
        let mut data = DataDescription::new();
        data.define(bytes.into_boxed_slice());
        data.set_align(std::mem::align_of::<NativeTaskEntryV2>() as u64);
        if let Some(&entry) = ids.get(&name) {
            let entry = module.declare_func_in_data(entry, &mut data);
            data.write_function_addr(std::mem::offset_of!(NativeTaskEntryV2, entry) as u32, entry);
        }
        if let Some(targets) = scoped_targets {
            let targets = module.declare_data_in_data(targets, &mut data);
            data.write_data_addr(
                std::mem::offset_of!(NativeTaskEntryV2, scoped_targets) as u32,
                targets,
                0,
            );
        }
        let inputs = module.declare_data_in_data(input_data, &mut data);
        data.write_data_addr(
            std::mem::offset_of!(NativeTaskEntryV2, input_keys) as u32,
            inputs,
            0,
        );
        let layout = module.declare_data_in_data(layout, &mut data);
        data.write_data_addr(
            std::mem::offset_of!(NativeTaskEntryV2, layout) as u32,
            layout,
            0,
        );
        let id = module
            .declare_anonymous_data(false, false)
            .map_err(|e| compile_error(e.to_string()))?;
        module
            .define_data(id, &data)
            .map_err(|e| compile_error(e.to_string()))?;
        result.entry(parent).or_default().insert(key, id);
    }
    Ok(result)
}
