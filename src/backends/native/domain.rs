use super::*;
use cranelift_module::{DataDescription, DataId};
pub(super) fn metadata<M: Module>(
    module: &mut M,
    plan: &crate::core::ir::ModulePlan,
    names: &[&String],
    ids: &HashMap<String, FuncId>,
    entries: &HashMap<String, FuncId>,
) -> Result<DataId, Error> {
    let width = module.target_config().pointer_bytes() as usize;
    fn data<M: Module>(module: &mut M, bytes: Vec<u8>, align: u64) -> Result<DataId, Error> {
        let id = module
            .declare_anonymous_data(false, false)
            .map_err(|e| compile_error(e.to_string()))?;
        let mut d = DataDescription::new();
        d.define(bytes.into_boxed_slice());
        d.set_align(align);
        module
            .define_data(id, &d)
            .map_err(|e| compile_error(e.to_string()))?;
        Ok(id)
    }
    let selected: Vec<_> = names
        .iter()
        .filter(|name| {
            let info = &plan.functions[**name];
            info.domain_entry
                || (info.suspends
                    && plan.bodies.contains_key(**name)
                    && info
                        .signature
                        .params
                        .iter()
                        .all(crate::core::instance::scalar)
                    && crate::core::instance::scalar(&info.signature.ret))
        })
        .collect();
    let endian = module.isa().endianness();
    let put = |bytes: &mut [u8], offset: usize, n: usize| {
        let raw = match endian {
            cranelift_codegen::ir::Endianness::Little => (n as u64).to_le_bytes(),
            cranelift_codegen::ir::Endianness::Big => (n as u64).to_be_bytes(),
        };
        let raw = if endian == cranelift_codegen::ir::Endianness::Little {
            &raw[..width]
        } else {
            &raw[8 - width..]
        };
        bytes[offset..offset + width].copy_from_slice(raw);
    };
    let mut bytes = vec![0; selected.len().max(1) * 7 * width];
    let mut refs = Vec::new();
    for (index, name) in selected.iter().enumerate() {
        let info = &plan.functions[**name];
        let sig = crate::runtime::domain::metadata::encode(info);
        let name_id = data(module, name.as_bytes().to_vec(), 1)?;
        let sig_id = data(module, sig.clone(), 1)?;
        let base = index * 7 * width;
        put(&mut bytes, base + width, name.len());
        put(&mut bytes, base + 3 * width, sig.len());
        put(&mut bytes, base + 5 * width, info.frame_slots);
        put(&mut bytes, base + 6 * width, usize::from(info.suspends));
        refs.push((
            base,
            name_id,
            sig_id,
            if info.suspends {
                ids[**name]
            } else {
                entries[**name]
            },
        ));
    }
    let mut table = DataDescription::new();
    table.define(bytes.into_boxed_slice());
    table.set_align(width as u64);
    for (base, name, sig, entry) in refs {
        let n = module.declare_data_in_data(name, &mut table);
        table.write_data_addr(base as u32, n, 0);
        let n = module.declare_data_in_data(sig, &mut table);
        table.write_data_addr((base + 2 * width) as u32, n, 0);
        let f = module.declare_func_in_data(entry, &mut table);
        table.write_function_addr((base + 4 * width) as u32, f);
    }
    let table_id = module
        .declare_anonymous_data(false, false)
        .map_err(|e| compile_error(e.to_string()))?;
    module
        .define_data(table_id, &table)
        .map_err(|e| compile_error(e.to_string()))?;
    let mut bytes = vec![0; 3 * width];
    put(&mut bytes, 0, 1);
    put(&mut bytes, width, selected.len());
    let mut header = DataDescription::new();
    header.define(bytes.into_boxed_slice());
    header.set_align(width as u64);
    let t = module.declare_data_in_data(table_id, &mut header);
    header.write_data_addr((2 * width) as u32, t, 0);
    let id = module
        .declare_anonymous_data(false, false)
        .map_err(|e| compile_error(e.to_string()))?;
    module
        .define_data(id, &header)
        .map_err(|e| compile_error(e.to_string()))?;
    Ok(id)
}
