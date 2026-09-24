//! Minimal DWARF function ranges and line tables for native AOT objects.

use std::collections::HashMap;
use std::path::PathBuf;

use cranelift_codegen::Context;
use cranelift_module::FuncId;
use cranelift_object::ObjectProduct;
use gimli::write::{
    Address, AttributeValue, DwarfUnit, EndianVec, LineProgram, LineString, RelocateWriter,
    Relocation, RelocationTarget, Sections, Writer,
};
use gimli::{Encoding, Format, LineEncoding, RunTimeEndian};

use crate::mir::MirFunction;
use crate::module::StableId;

pub(crate) struct DebugSource {
    pub(crate) module: StableId,
    pub(crate) path: PathBuf,
    pub(crate) text: String,
}

pub(crate) struct DebugInfo {
    sources: Vec<DebugSource>,
    functions: Vec<DebugFunction>,
}

struct DebugFunction {
    id: FuncId,
    module: StableId,
    name: String,
    declaration: usize,
    size: u64,
    rows: Vec<(u64, Option<usize>)>,
}

pub(crate) struct PendingDebugRecord {
    module: StableId,
    name: String,
    declaration: usize,
}

impl DebugInfo {
    pub(crate) fn new(sources: Vec<DebugSource>) -> Self {
        Self {
            sources,
            functions: Vec::new(),
        }
    }

    pub(crate) fn pending_record(
        &self,
        function: &MirFunction,
        resume: Option<usize>,
    ) -> Option<PendingDebugRecord> {
        let module = function.source.module?;
        let source = self.sources.iter().find(|source| source.module == module)?;
        let body = function
            .source
            .body
            .filter(|span| span.end() <= source.text.len())?;
        let name = match resume {
            Some(entry) => format!("{} [resume {entry}]", function.name),
            None => function.name.clone(),
        };
        Some(PendingDebugRecord {
            module,
            name,
            declaration: body.start(),
        })
    }

    pub(crate) fn record_compiled(
        &mut self,
        id: FuncId,
        pending: PendingDebugRecord,
        context: &Context,
    ) {
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.module == pending.module)
        else {
            return;
        };
        let code = context
            .compiled_code()
            .expect("defined function has machine code");
        let rows = code
            .buffer
            .get_srclocs_sorted()
            .iter()
            .map(|loc| {
                let offset = loc.loc.bits() as usize;
                (
                    u64::from(loc.start),
                    (offset < source.text.len()).then_some(offset),
                )
            })
            .collect();
        self.functions.push(DebugFunction {
            id,
            module: pending.module,
            name: pending.name,
            declaration: pending.declaration,
            size: code.code_buffer().len() as u64,
            rows,
        });
    }

    pub(crate) fn emit(
        self,
        product: &mut ObjectProduct,
        address_size: u8,
        endianness: cranelift_codegen::ir::Endianness,
    ) -> Result<(), String> {
        if self.functions.is_empty() {
            return Ok(());
        }
        let encoding = Encoding {
            format: Format::Dwarf32,
            version: 4,
            address_size,
        };
        let mut dwarf = DwarfUnit::new(encoding);
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let cwd = cwd.to_string_lossy().as_bytes().to_vec();
        let first_path = self.sources[0].path.to_string_lossy().as_bytes().to_vec();
        let mut lines = LineProgram::new(
            encoding,
            LineEncoding::default(),
            LineString::String(cwd.clone()),
            None,
            LineString::String(first_path.clone()),
            None,
        );
        let files = self
            .sources
            .iter()
            .map(|source| {
                let file = lines.add_file(
                    LineString::String(source.path.to_string_lossy().as_bytes().to_vec()),
                    lines.default_directory(),
                    None,
                );
                let mut starts = vec![0];
                starts.extend(
                    source
                        .text
                        .bytes()
                        .enumerate()
                        .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
                );
                (source.module, (file, starts))
            })
            .collect::<HashMap<_, _>>();
        let root = dwarf.unit.root();
        dwarf
            .unit
            .get_mut(root)
            .set(gimli::DW_AT_name, AttributeValue::String(first_path));
        dwarf
            .unit
            .get_mut(root)
            .set(gimli::DW_AT_comp_dir, AttributeValue::String(cwd));
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_producer,
            AttributeValue::String(format!("Joky {}", env!("CARGO_PKG_VERSION")).into_bytes()),
        );
        let symbols = self
            .functions
            .iter()
            .map(|function| product.function_symbol(function.id))
            .collect::<Vec<_>>();
        let macho = product.object.format() == object::BinaryFormat::MachO;
        // dsymutil consumes object addresses in local section relocations.
        // Read the writer's layout instead of duplicating Mach-O section alignment rules.
        let object_addresses = if macho {
            use object::{Object, ObjectSymbol};
            let bytes = product.object.write().map_err(|error| error.to_string())?;
            let object =
                object::File::parse(bytes.as_slice()).map_err(|error| error.to_string())?;
            let addresses = object
                .symbols()
                .filter_map(|symbol| Some((symbol.name_bytes().ok()?.to_vec(), symbol.address())))
                .collect::<HashMap<_, _>>();
            symbols
                .iter()
                .map(|symbol| addresses[&product.object.symbol(*symbol).name])
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for (index, function) in self.functions.iter().enumerate() {
            let address = Address::Symbol {
                symbol: index,
                addend: 0,
            };
            let (file, starts) = &files[&function.module];
            let line_column = |offset: usize| {
                let line = starts.partition_point(|start| *start <= offset);
                (line as u64, (offset - starts[line - 1] + 1) as u64)
            };
            let entry = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
            let die = dwarf.unit.get_mut(entry);
            die.set(
                gimli::DW_AT_name,
                AttributeValue::String(function.name.as_bytes().to_vec()),
            );
            die.set(
                gimli::DW_AT_linkage_name,
                AttributeValue::String(product.object.symbol(symbols[index]).name.clone()),
            );
            die.set(gimli::DW_AT_low_pc, AttributeValue::Address(address));
            die.set(gimli::DW_AT_high_pc, AttributeValue::Udata(function.size));
            die.set(
                gimli::DW_AT_decl_file,
                AttributeValue::FileIndex(Some(*file)),
            );
            die.set(
                gimli::DW_AT_decl_line,
                AttributeValue::Udata(line_column(function.declaration).0),
            );
            lines.begin_sequence(Some(address));
            for (offset, source) in &function.rows {
                let (line, column) = source.map(line_column).unwrap_or((0, 0));
                let row = lines.row();
                row.address_offset = *offset;
                row.file = *file;
                row.line = line;
                row.column = column;
                row.is_statement = source.is_some();
                lines.generate_row();
            }
            lines.end_sequence(function.size);
        }
        // ObjectModule places generated functions in one text section. Include
        // generated adapters between source functions in the compilation-unit range.
        let (first, start) = symbols
            .iter()
            .enumerate()
            .min_by_key(|(_, symbol)| product.object.symbol(**symbol).value)
            .unwrap();
        let start = product.object.symbol(*start);
        let end = self
            .functions
            .iter()
            .zip(&symbols)
            .map(|(function, symbol)| {
                let symbol = product.object.symbol(*symbol);
                if symbol.section != start.section {
                    return Err("debug functions must share a text section".to_string());
                }
                Ok(symbol.value + function.size)
            })
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .max()
            .unwrap();
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_low_pc,
            AttributeValue::Address(Address::Symbol {
                symbol: first,
                addend: 0,
            }),
        );
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_high_pc,
            AttributeValue::Udata(end - start.value),
        );
        dwarf.unit.line_program = lines;
        let endian = match endianness {
            cranelift_codegen::ir::Endianness::Little => RunTimeEndian::Little,
            cranelift_codegen::ir::Endianness::Big => RunTimeEndian::Big,
        };
        let mut sections = Sections::new(DebugWriter {
            bytes: EndianVec::new(endian),
            relocations: Vec::new(),
        });
        dwarf
            .write(&mut sections)
            .map_err(|error| error.to_string())?;
        let mut section_ids = HashMap::new();
        sections.for_each(|id, section| -> Result<(), String> {
            if section.bytes.slice().is_empty() {
                return Ok(());
            }
            let (segment, name) = if macho {
                (
                    b"__DWARF".to_vec(),
                    format!("__{}", &id.name()[1..]).into_bytes(),
                )
            } else {
                (Vec::new(), id.name().as_bytes().to_vec())
            };
            let section_id = product
                .object
                .add_section(segment, name, object::SectionKind::Debug);
            product
                .object
                .set_section_data(section_id, section.bytes.slice().to_vec(), 1);
            section_ids.insert(id, section_id);
            Ok(())
        })?;
        sections.for_each(|id, section| -> Result<(), String> {
            let Some(section_id) = section_ids.get(&id).copied() else {
                return Ok(());
            };
            for relocation in &section.relocations {
                let mut addend = relocation.addend;
                let symbol = match relocation.target {
                    RelocationTarget::Symbol(index) if macho => {
                        addend += object_addresses[index] as i64;
                        product
                            .object
                            .symbol_section_and_offset(symbols[index])
                            .ok_or_else(|| "debug function has no code section".to_string())?
                            .0
                    }
                    RelocationTarget::Symbol(index) => symbols[index],
                    RelocationTarget::Section(target) => {
                        // Mach-O DWARF offsets are section-relative constants, not relocations.
                        if macho {
                            let mut value = EndianVec::new(endian);
                            value
                                .write_udata(relocation.addend as u64, relocation.size)
                                .map_err(|error| error.to_string())?;
                            let start = relocation.offset;
                            product.object.section_mut(section_id).data_mut()
                                [start..start + relocation.size as usize]
                                .copy_from_slice(value.slice());
                            continue;
                        }
                        product.object.section_symbol(section_ids[&target])
                    }
                };
                product
                    .object
                    .add_relocation(
                        section_id,
                        object::write::Relocation {
                            offset: relocation.offset as u64,
                            symbol,
                            addend,
                            flags: object::RelocationFlags::Generic {
                                kind: object::RelocationKind::Absolute,
                                encoding: object::RelocationEncoding::Generic,
                                size: relocation.size * 8,
                            },
                        },
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
    }
}

#[derive(Clone)]
struct DebugWriter {
    bytes: EndianVec<RunTimeEndian>,
    relocations: Vec<Relocation>,
}

impl RelocateWriter for DebugWriter {
    type Writer = EndianVec<RunTimeEndian>;
    fn writer(&self) -> &Self::Writer {
        &self.bytes
    }
    fn writer_mut(&mut self) -> &mut Self::Writer {
        &mut self.bytes
    }
    fn relocate(&mut self, relocation: Relocation) {
        self.relocations.push(relocation);
    }
}
