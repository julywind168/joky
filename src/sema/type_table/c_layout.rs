//! Host C object layouts. Pointer edges do not participate in value recursion.

use super::{CStructLayout, Type, TypeTable};
use std::collections::{HashMap, HashSet};

fn overflow() -> String {
    "C layout exceeds the host addressable object size".into()
}

fn align_up(size: u64, align: u64) -> Result<u64, String> {
    size.div_ceil(align).checked_mul(align).ok_or_else(overflow)
}

impl TypeTable {
    /// CArray currently describes native memory only. It has no Joky value ABI.
    pub(crate) fn has_inline_c_array(&self, ty: Type) -> bool {
        fn visit(table: &TypeTable, ty: Type, seen: &mut HashSet<Type>) -> bool {
            if !seen.insert(ty) {
                return false;
            }
            match ty {
                Type::CArray(_) => true,
                Type::Tuple(id) => table
                    .tuple_elements(id)
                    .iter()
                    .any(|t| visit(table, *t, seen)),
                Type::Struct(id) => table
                    .struct_fields(id)
                    .iter()
                    .any(|(_, t)| visit(table, *t, seen)),
                Type::Class(id) => table
                    .class_fields(id)
                    .iter()
                    .any(|(_, t)| visit(table, *t, seen)),
                Type::Enum(id) => table
                    .enum_variants(id)
                    .iter()
                    .any(|v| v.fields.iter().any(|(_, t)| visit(table, *t, seen))),
                Type::Option(id) => visit(table, table.option_type(id), seen),
                Type::Result(id) => {
                    let (a, b) = table.result_types(id);
                    visit(table, a, seen) || visit(table, b, seen)
                }
                Type::Function(id) => {
                    let f = table.function_type(id);
                    f.parameters.iter().any(|t| visit(table, *t, seen))
                        || visit(table, f.return_type, seen)
                }
                _ => false,
            }
        }
        visit(self, ty, &mut HashSet::new())
    }

    /// Compute each object once, including nested structs and inline arrays.
    /// This runs after resolution so forward and imported types are complete.
    pub(crate) fn validate_c_layouts(&mut self) -> Result<(), String> {
        let mut layouts = HashMap::new();
        let mut visiting = HashSet::new();
        for (id, structure) in self.structs.iter().enumerate() {
            if structure.repr_c {
                self.compute_c_layout(Type::Struct(id), &mut visiting, &mut layouts)?;
            }
        }
        for id in 0..self.c_arrays.len() {
            self.compute_c_layout(Type::CArray(id), &mut visiting, &mut layouts)?;
        }
        for pointee in &self.c_pointers {
            // Unit is void only behind a pointer. Pointer-to-pointer is a
            // word-sized value whose pointee never participates in value
            // recursion, so nested pointers just validate like any other
            // pointee in this loop.
            if *pointee == Type::Unit {
                continue;
            }
            self.compute_c_layout(*pointee, &mut visiting, &mut layouts)?;
        }
        for (id, structure) in self.structs.iter_mut().enumerate() {
            structure.c_layout = layouts.remove(&Type::Struct(id));
        }
        for class in &self.classes {
            if class
                .fields
                .iter()
                .any(|(_, ty)| self.has_inline_c_array(*ty))
            {
                return Err(
                    "CArray is a native layout type; use CPtr or CMutPtr for Joky values".into(),
                );
            }
        }
        Ok(())
    }

    fn compute_c_layout(
        &self,
        ty: Type,
        visiting: &mut HashSet<Type>,
        layouts: &mut HashMap<Type, CStructLayout>,
    ) -> Result<CStructLayout, String> {
        if let Some(layout) = layouts.get(&ty) {
            return Ok(layout.clone());
        }
        if !visiting.insert(ty) {
            return Err("recursive C value layout".into());
        }
        let scalar = |size: usize, align: usize| CStructLayout {
            size: size as u64,
            align: align as u64,
            field_offsets: Vec::new(),
        };
        let result = match ty {
            Type::I8 | Type::U8 => scalar(size_of::<u8>(), align_of::<u8>()),
            Type::I16 | Type::U16 => scalar(size_of::<u16>(), align_of::<u16>()),
            Type::I32 | Type::U32 => scalar(size_of::<u32>(), align_of::<u32>()),
            Type::I64 | Type::U64 => scalar(size_of::<u64>(), align_of::<u64>()),
            Type::F32 => scalar(size_of::<f32>(), align_of::<f32>()),
            Type::F64 => scalar(size_of::<f64>(), align_of::<f64>()),
            Type::CPtr(_) | Type::CMutPtr(_) | Type::CStr => {
                scalar(size_of::<usize>(), align_of::<usize>())
            }
            Type::CArray(id) => {
                let &(element, count) = self.c_arrays.get(id).ok_or("invalid CArray type index")?;
                if count == 0 {
                    return Err("CArray length must be greater than zero".into());
                }
                let element = self.compute_c_layout(element, visiting, layouts)?;
                CStructLayout {
                    size: element.size.checked_mul(count).ok_or_else(overflow)?,
                    align: element.align,
                    field_offsets: Vec::new(),
                }
            }
            Type::Struct(id) => {
                let structure = self.structs.get(id).ok_or("invalid C struct type index")?;
                if !structure.repr_c {
                    return Err(format!(
                        "C layout requires @repr(c) on '{}'",
                        structure.name
                    ));
                }
                if structure.fields.is_empty() {
                    return Err("empty @repr(c) structs are not supported".into());
                }
                let mut result = scalar(0, 1);
                for (_, field) in &structure.fields {
                    let field = self.compute_c_layout(*field, visiting, layouts)?;
                    result.size = align_up(result.size, field.align)?;
                    result.field_offsets.push(result.size);
                    result.size = result.size.checked_add(field.size).ok_or_else(overflow)?;
                    result.align = result.align.max(field.align);
                }
                result.size = align_up(result.size, result.align)?;
                result
            }
            _ => {
                return Err(format!(
                    "{} is not a C layout type",
                    crate::sema::type_name(ty)
                ))
            }
        };
        if result.size > isize::MAX as u64 {
            return Err(overflow());
        }
        visiting.remove(&ty);
        layouts.insert(ty, result.clone());
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(source: &str) -> TypeTable {
        crate::sema::check_module(&crate::syntax::parse_program(source).unwrap())
            .unwrap()
            .module
            .types
    }

    #[test]
    #[cfg(unix)]
    fn c_compiler_agrees_on_nested_array_and_pointer_layout() {
        let table = table("@repr(c) struct Header { let tag: UInt8; let number: UInt32 }\n\
            @repr(c) struct Packet { let tag: UInt8; let headers: CArray(Header, 3); let measure: Float64; let next: CPtr(Packet) }");
        let mut expected = String::new();
        for name in ["Header", "Packet"] {
            let Type::Struct(id) = table.struct_type(name).unwrap() else {
                unreachable!()
            };
            let layout = table.structs[id].c_layout.as_ref().unwrap();
            let words = [layout.size, layout.align]
                .into_iter()
                .chain(layout.field_offsets.iter().copied());
            expected.push_str(&words.map(|v| v.to_string()).collect::<Vec<_>>().join(" "));
            expected.push('\n');
        }
        let path = std::env::temp_dir().join(format!("joky-c-layout-{}", std::process::id()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        let compile = std::process::Command::new("cc")
            .args(["-std=c11", "-Wall", "-Werror"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/ffi/layout.c"
            ))
            .arg("-o")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = std::process::Command::new(path).output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }

    #[test]
    fn linking_checks_c_representation_and_offsets() {
        let source = table("@repr(c) struct Point { let x: Int32; let y: Int32 }");
        let mut linked = TypeTable::default();
        linked
            .merge_module(&source, crate::module::StableId(42))
            .unwrap();
        for change_repr in [false, true] {
            let mut changed = source.clone();
            if change_repr {
                changed.structs[0].repr_c = false;
            } else {
                changed.structs[0].c_layout.as_mut().unwrap().field_offsets[1] = 5;
            }
            assert!(linked
                .clone()
                .merge_module(&changed, crate::module::StableId(42))
                .unwrap_err()
                .contains("struct layout ABI mismatch"));
        }
    }
}
