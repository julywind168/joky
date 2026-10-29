//! Immutable Local task entry descriptors shared by JIT and relocatable objects.
use super::*;
use crate::runtime::cps::task::NativeTaskEntryV1;

pub(super) fn descriptors<M: Module>(
    module: &mut M,
    plan: &ModulePlan,
    reachable: &HashSet<String>,
    ids: &HashMap<String, FuncId>,
) -> Result<HashMap<String, HashMap<String, DataId>>, Error> {
    let scoped = crate::core::ir::parallel::scoped::callees(plan);
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
        let signature = crate::core::ir::parallel::scoped::signature(plan, &name);
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
        let mut bytes = vec![0; std::mem::size_of::<NativeTaskEntryV1>()];
        for (offset, value) in [
            (
                std::mem::offset_of!(NativeTaskEntryV1, qualification),
                usize::from(scoped.contains(&name)),
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV1, input_count),
                inputs.len(),
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV1, result_key),
                result_key.unwrap_or(usize::MAX),
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV1, version),
                crate::runtime::cps::task::ENTRY_VERSION,
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV1, frame_slots),
                info.frame_slots,
            ),
            (
                std::mem::offset_of!(NativeTaskEntryV1, layout_words),
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
        data.set_align(std::mem::align_of::<NativeTaskEntryV1>() as u64);
        if let Some(&entry) = ids.get(&name) {
            let entry = module.declare_func_in_data(entry, &mut data);
            data.write_function_addr(std::mem::offset_of!(NativeTaskEntryV1, entry) as u32, entry);
        }
        let inputs = module.declare_data_in_data(input_data, &mut data);
        data.write_data_addr(
            std::mem::offset_of!(NativeTaskEntryV1, input_keys) as u32,
            inputs,
            0,
        );
        let layout = module.declare_data_in_data(layout, &mut data);
        data.write_data_addr(
            std::mem::offset_of!(NativeTaskEntryV1, layout) as u32,
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
